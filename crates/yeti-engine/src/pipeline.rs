//! The running stream graph (PLAN.md §3):
//!
//! ```text
//! Yeti ─▶ capture cb ─(mono, device rate)─▶ ring ─▶ DSP thread ─▶ out ring ─▶ output cb ─▶ virtual mic
//!                                               │  (48 kHz,     └▶ mon ring ─▶ monitor cb ─▶ Yeti headphones
//!                                               │   480 blocks)
//! system output ─▶ loopback cb ─▶ ref ring ─────┘  (AEC far-end reference)
//! ```
//!
//! - cpal callbacks: ring push/pop + downmix/duplicate only; wrapped in `assert_no_alloc` (debug).
//! - DSP worker: input resample → `VoiceChain` (built **on** the worker, it is `!Send`) →
//!   meters/waveform → drift-compensated output resamplers (ratio driven by ring fill).
//! - Underrun → output silence + counter. Capture ring full → drop + counter.

use crate::drift::{DriftConfig, DriftController};
use crate::resample::{InputStage, OutputStage};
use crate::telemetry::{AtomicF32, Telemetry, peak_rms};
use assert_no_alloc::assert_no_alloc;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, SizedSample, StreamConfig};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::Relaxed};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use yeti_types::{BLOCK_FRAMES, DspParams, ENGINE_SAMPLE_RATE, Result, YetiError};

/// State shared by the engine handle, the supervisor and the DSP worker.
pub struct Control {
    pub params: Mutex<DspParams>,
    pub params_gen: AtomicU64,
    pub monitor_enabled: AtomicBool,
    pub monitor_volume: AtomicF32,
    /// Skip the voice chain (diagnostics / `yeti-passthrough --bypass`).
    pub bypass_chain: AtomicBool,
    pub telemetry: Telemetry,
    /// A/B test recorder (raw + cleaned takes, playback through the monitor).
    pub ab: crate::ab::AbRecorder,
    /// Live end-to-end latency probe (diagnostics).
    pub probe: crate::probe::LatencyProbe,
}

impl Control {
    pub fn new(params: DspParams) -> Self {
        Self {
            params: Mutex::new(params),
            params_gen: AtomicU64::new(1),
            monitor_enabled: AtomicBool::new(false),
            monitor_volume: AtomicF32::new(0.8),
            bypass_chain: AtomicBool::new(false),
            telemetry: Telemetry::default(),
            ab: crate::ab::AbRecorder::default(),
            probe: crate::probe::LatencyProbe::default(),
        }
    }
}

/// Resolved devices for one pipeline run.
#[derive(Debug, Clone, Default)]
pub struct PipelineSpec {
    /// Capture device name (substring match).
    pub input: String,
    /// Main output (virtual mic feed); `None` = no main output.
    pub output: Option<OutputSel>,
    /// Monitor ("hear myself") device; `None` = no monitor stream.
    pub monitor: Option<OutputSel>,
    /// Requested callback buffer size in frames (`None` = device default).
    pub buffer_frames: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OutputSel {
    /// System default output device.
    Default,
    /// Output device whose name contains this string.
    Named(String),
}

/// Facts about the opened streams (for logs / `yeti-passthrough`).
#[derive(Debug, Clone, Default)]
pub struct PipelineInfo {
    pub input_name: String,
    pub input_rate: u32,
    pub input_channels: u16,
    pub input_format: String,
    pub input_buffer: Option<u32>,
    pub output_name: Option<String>,
    pub output_rate: u32,
    pub output_channels: u16,
    pub output_buffer: Option<u32>,
    pub monitor_name: Option<String>,
    pub monitor_rate: u32,
    pub output_target_fill: u32,
    pub chain_latency_ms: f32,
}

/// Why a pipeline stopped on its own.
#[derive(Default)]
pub struct Health {
    /// A stream reported `DeviceNotAvailable` / `StreamInvalidated`.
    pub device_lost: AtomicBool,
    /// Last non-xrun cpal error kind (debug formatting index), 0 = none.
    pub other_errors: AtomicU32,
    /// DSP worker exited (panic or error).
    pub dsp_dead: AtomicBool,
}

/// A running graph. Dropping it stops the streams and joins the DSP thread.
pub struct Pipeline {
    streams: Vec<cpal::Stream>,
    loopback: Option<cpal::Stream>,
    /// Keeps the self-excluding tap + aggregate alive while `loopback` captures from it.
    #[cfg(target_os = "macos")]
    tap: Option<crate::mac_tap::ProcessTap>,
    /// The active reference excludes this process (no monitor feedback into AEC).
    pub reference_excludes_self: bool,
    ref_tx: mpsc::Sender<Option<RefSource>>,
    dsp: Option<JoinHandle<()>>,
    /// Test-tone generator thread (input = [`TEST_TONE_INPUT`]).
    tone: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    pub health: Arc<Health>,
    pub info: PipelineInfo,
    control: Arc<Control>,
}

/// Special capture "device": a 440 Hz sine generated on a thread paced by the system
/// clock (no microphone, no TCC permission). Exercises rings, DSP, output and the drift
/// bridge against a real output device clock.
pub const TEST_TONE_INPUT: &str = "test-tone";
const TONE_CHUNK: usize = 96; // 2 ms

fn spawn_tone(
    mut prod: rtrb::Producer<f32>,
    stop: Arc<AtomicBool>,
    control: Arc<Control>,
) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("yeti-testtone".into())
        .spawn(move || {
            crate::rt_priority::promote_current_thread(2.0, 0.5);
            let t0 = Instant::now();
            let mut produced: u64 = 0;
            let w = 2.0 * std::f64::consts::PI * 440.0 / ENGINE_SAMPLE_RATE as f64;
            while !stop.load(Relaxed) {
                let due = (t0.elapsed().as_secs_f64() * ENGINE_SAMPLE_RATE as f64) as u64;
                while produced + TONE_CHUNK as u64 <= due {
                    if prod.slots() < TONE_CHUNK {
                        control.telemetry.capture_overruns.fetch_add(1, Relaxed);
                        produced += TONE_CHUNK as u64;
                        continue;
                    }
                    if let Ok(chunk) = prod.write_chunk_uninit(TONE_CHUNK) {
                        let base = produced;
                        chunk.fill_from_iter(
                            (0..TONE_CHUNK as u64)
                                .map(|i| (0.25 * ((base + i) as f64 * w).sin()) as f32),
                        );
                    }
                    produced += TONE_CHUNK as u64;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        })
}

type OpenedOutput = (
    cpal::Stream,
    rtrb::Producer<f32>,
    Arc<AtomicBool>,
    String,
    u32,
    Option<u32>,
    u16,
);

struct RefSource {
    cons: rtrb::Consumer<f32>,
    rate: u32,
}

// ---------------------------------------------------------------------------
// Device lookup
// ---------------------------------------------------------------------------

fn dev_name(d: &cpal::Device) -> String {
    d.description()
        .map(|x| x.name().to_string())
        .unwrap_or_default()
}

/// First device (input or output side) whose name equals, else contains, `name`.
pub fn find_device(host: &cpal::Host, name: &str, input: bool) -> Option<cpal::Device> {
    let list: Vec<cpal::Device> = if input {
        host.input_devices().ok()?.collect()
    } else {
        host.output_devices().ok()?.collect()
    };
    let names: Vec<String> = list.iter().map(dev_name).collect();
    let idx = names
        .iter()
        .position(|n| n == name)
        .or_else(|| names.iter().position(|n| n.contains(name)))?;
    list.into_iter().nth(idx)
}

fn resolve_output(host: &cpal::Host, sel: &OutputSel) -> Option<cpal::Device> {
    match sel {
        OutputSel::Default => host.default_output_device(),
        OutputSel::Named(n) => find_device(host, n, false),
    }
}

/// Names of all devices `(inputs, outputs)` as cpal sees them.
pub fn list_device_names() -> (Vec<String>, Vec<String>) {
    let host = cpal::default_host();
    let i = host
        .input_devices()
        .map(|d| d.map(|x| dev_name(&x)).collect())
        .unwrap_or_default();
    let o = host
        .output_devices()
        .map(|d| d.map(|x| dev_name(&x)).collect())
        .unwrap_or_default();
    (i, o)
}

fn backend(e: impl std::fmt::Display) -> YetiError {
    YetiError::Backend(e.to_string())
}

fn map_build_err(e: cpal::Error, what: &str) -> YetiError {
    match e.kind() {
        cpal::ErrorKind::DeviceNotAvailable => YetiError::DeviceDisconnected,
        _ => YetiError::Backend(format!("{what}: {e}")),
    }
}

fn error_cb(health: Arc<Health>, tel: Arc<Control>) -> impl FnMut(cpal::Error) + Send + 'static {
    move |e: cpal::Error| match e.kind() {
        cpal::ErrorKind::Xrun => {
            tel.telemetry.stream_xruns.fetch_add(1, Relaxed);
        }
        cpal::ErrorKind::DeviceNotAvailable | cpal::ErrorKind::StreamInvalidated => {
            health.device_lost.store(true, Relaxed);
        }
        cpal::ErrorKind::DeviceChanged => {}
        k => {
            health.other_errors.store(k as u32 + 1, Relaxed);
        }
    }
}

// ---------------------------------------------------------------------------
// Callbacks (RT-safe)
// ---------------------------------------------------------------------------

fn build_capture<T>(
    dev: &cpal::Device,
    cfg: StreamConfig,
    mut prod: rtrb::Producer<f32>,
    control: Arc<Control>,
    health: Arc<Health>,
    count_overruns: bool,
) -> Result<cpal::Stream>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let ch = cfg.channels.max(1) as usize;
    let inv = 1.0 / ch as f32;
    let c2 = control.clone();
    let rate = cfg.sample_rate.max(1) as u64;
    let probe_period = (crate::probe::PROBE_PERIOD_S * rate as f64) as usize;
    let mut probe_countdown = 0usize;
    dev.build_input_stream::<T, _, _>(
        cfg,
        move |data: &[T], info: &cpal::InputCallbackInfo| {
            assert_no_alloc(|| {
                let frames = data.len() / ch;
                let n = frames.min(prod.slots());
                if n < frames && count_overruns {
                    c2.telemetry.capture_overruns.fetch_add(1, Relaxed);
                }
                // Latency probe (main capture only): silence + one impulse per period.
                let probing = count_overruns && c2.probe.enabled.load(Relaxed);
                let mut impulse_at = usize::MAX;
                if probing {
                    if probe_countdown < n {
                        let t0 = info.timestamp().capture.as_nanos() as u64;
                        let t = t0 + probe_countdown as u64 * 1_000_000_000 / rate;
                        if c2.probe.can_fire(t) {
                            impulse_at = probe_countdown;
                            c2.probe.fired(t);
                        }
                        probe_countdown = probe_period;
                    } else {
                        probe_countdown -= n;
                    }
                }
                if let Ok(chunk) = prod.write_chunk_uninit(n) {
                    if probing {
                        chunk.fill_from_iter(
                            (0..n).map(|i| if i == impulse_at { 1.0 } else { 0.0 }),
                        );
                    } else {
                        chunk.fill_from_iter(data.chunks_exact(ch).map(|f| {
                            let mut s = 0.0f32;
                            for &x in f {
                                s += f32::from_sample(x);
                            }
                            s * inv
                        }));
                    }
                }
            })
        },
        error_cb(health, control),
        Some(Duration::from_secs(5)),
    )
    .map_err(|e| map_build_err(e, "build input stream"))
}

fn build_playback<T>(
    dev: &cpal::Device,
    cfg: StreamConfig,
    mut cons: rtrb::Consumer<f32>,
    started: Arc<AtomicBool>,
    control: Arc<Control>,
    health: Arc<Health>,
    count_underruns: bool,
) -> Result<cpal::Stream>
where
    T: SizedSample + FromSample<f32> + Send + 'static,
{
    let ch = cfg.channels.max(1) as usize;
    let c2 = control.clone();
    let rate = cfg.sample_rate.max(1) as u64;
    // Underruns only count once the DSP worker has started feeding this stream (the worker
    // waits for `started`, then fills the ring to its target in one go).
    let mut primed = false;
    dev.build_output_stream::<T, _, _>(
        cfg,
        move |data: &mut [T], info: &cpal::OutputCallbackInfo| {
            assert_no_alloc(|| {
                started.store(true, Relaxed);
                let frames = data.len() / ch;
                let n = frames.min(cons.slots());
                primed |= n > 0;
                let probing = count_underruns && c2.probe.enabled.load(Relaxed);
                if let Ok(chunk) = cons.read_chunk(n) {
                    let (a, b) = chunk.as_slices();
                    if probing {
                        // Find the impulse, play silence.
                        if let Some(j) = a
                            .iter()
                            .chain(b)
                            .position(|v| *v > crate::probe::PROBE_THRESHOLD)
                        {
                            let t0 = info.timestamp().playback.as_nanos() as u64;
                            c2.probe.arrived(t0 + j as u64 * 1_000_000_000 / rate);
                        }
                        for s in &mut data[..n * ch] {
                            *s = T::EQUILIBRIUM;
                        }
                    } else {
                        for (frame, &v) in data.chunks_exact_mut(ch).zip(a.iter().chain(b)) {
                            frame.fill(T::from_sample(v));
                        }
                    }
                    chunk.commit_all();
                }
                if n < frames {
                    for s in &mut data[n * ch..] {
                        *s = T::EQUILIBRIUM;
                    }
                    if count_underruns && primed {
                        c2.telemetry.output_underruns.fetch_add(1, Relaxed);
                    }
                }
            })
        },
        error_cb(health, control),
        Some(Duration::from_secs(5)),
    )
    .map_err(|e| map_build_err(e, "build output stream"))
}

macro_rules! by_format {
    ($fmt:expr, $f:ident, $($arg:expr),*) => {
        match $fmt {
            SampleFormat::I16 => $f::<i16>($($arg),*),
            SampleFormat::I32 => $f::<i32>($($arg),*),
            _ => $f::<f32>($($arg),*),
        }
    };
}

fn stream_config(sc: &cpal::SupportedStreamConfig, buffer_frames: Option<u32>) -> StreamConfig {
    let mut c = sc.config();
    if let (Some(n), cpal::SupportedBufferSize::Range { min, max }) =
        (buffer_frames, sc.buffer_size())
    {
        c.buffer_size = cpal::BufferSize::Fixed(n.clamp(*min, *max));
    }
    c
}

fn fmt_ok(f: SampleFormat) -> SampleFormat {
    match f {
        SampleFormat::I16 | SampleFormat::I32 | SampleFormat::F32 => f,
        _ => SampleFormat::F32,
    }
}

// ---------------------------------------------------------------------------
// Output path (DSP side)
// ---------------------------------------------------------------------------

struct OutputPath {
    prod: rtrb::Producer<f32>,
    /// Set by the output callback on its first call.
    started: Arc<AtomicBool>,
    /// The worker has fed this ring at least once (startup fill done).
    feeding: bool,
    stage: OutputStage,
    ctl: DriftController,
    buf: Vec<f32>,
    capacity: usize,
    zeros: Vec<f32>,
    /// Dropping blocks to bring an over-full ring back to target.
    draining: bool,
}

impl OutputPath {
    fn new(
        prod: rtrb::Producer<f32>,
        started: Arc<AtomicBool>,
        out_rate: u32,
        target: usize,
    ) -> Result<Self> {
        let stage = OutputStage::new(ENGINE_SAMPLE_RATE, out_rate)?;
        let buf = vec![0.0; stage.max_output() + 16];
        Ok(Self {
            started,
            feeding: false,
            capacity: prod.buffer().capacity(),
            prod,
            stage,
            ctl: DriftController::new(DriftConfig::for_target(target as f64)),
            buf,
            zeros: vec![0.0; target],
            draining: false,
        })
    }

    fn fill(&self) -> usize {
        self.capacity - self.prod.slots()
    }

    /// Top the ring up to the target with silence (startup / monitor re-enable).
    fn prefill(&mut self) {
        self.ctl.reset();
        let need = self.zeros.len().saturating_sub(self.fill());
        let _ = self
            .prod
            .push_entire_slice(&self.zeros[..need.min(self.prod.slots())]);
    }

    /// Resample one block into the ring. Hard re-centres the ring when it is far off target
    /// (consumer drained it → insert silence; stall recovery → drop the block) so the drift
    /// controller only ever sees small, slow errors.
    ///
    /// Startup: blocks are discarded until the device callback has started; the first block
    /// after that fills the (still empty) ring to the target in one go. This is not counted
    /// as a resync and avoids the startup burst (capture running before the output device).
    fn push(&mut self, block: &[f32]) -> PushResult {
        let mut res = PushResult::default();
        if !self.feeding {
            if !self.started.load(Relaxed) {
                return res;
            }
            self.feeding = true;
            self.ctl.reset();
            let n = self.stage.process(block, &mut self.buf).unwrap_or(0);
            let target = self.ctl.config().target_fill as usize;
            let pad = target
                .saturating_sub(self.fill() + n)
                .min(self.zeros.len())
                .min(self.prod.slots());
            let _ = self.prod.push_entire_slice(&self.zeros[..pad]);
            let (_, rest) = self.prod.push_partial_slice(&self.buf[..n]);
            res.overflow = rest.len();
            self.ctl.update(
                self.fill() as f64,
                BLOCK_FRAMES as f64 / ENGINE_SAMPLE_RATE as f64,
            );
            return res;
        }
        let n = self.stage.process(block, &mut self.buf).unwrap_or(0);
        let target = self.ctl.config().target_fill as usize;
        let before = self.fill();
        if before == 0 {
            let pad = target
                .saturating_sub(n)
                .min(self.zeros.len())
                .min(self.prod.slots());
            let _ = self.prod.push_entire_slice(&self.zeros[..pad]);
            res.resynced = true;
        } else if before > target + target.max(2 * BLOCK_FRAMES) {
            self.draining = true;
        }
        if self.draining {
            // Drop whole blocks until the fill is back near the target (hysteresis).
            if before > target + BLOCK_FRAMES / 2 {
                res.resynced = true;
                self.ctl.resynced(before as f64);
                return res;
            }
            self.draining = false;
        }
        let (_, rest) = self.prod.push_partial_slice(&self.buf[..n]);
        res.overflow = rest.len();
        let fill = self.fill() as f64;
        if res.resynced {
            self.ctl.resynced(fill);
        }
        let r = self
            .ctl
            .update(fill, BLOCK_FRAMES as f64 / ENGINE_SAMPLE_RATE as f64);
        self.stage.set_correction(r);
        res
    }
}

#[derive(Default)]
struct PushResult {
    /// Frames that did not fit (ring full).
    overflow: usize,
    /// The ring was re-centred (silence inserted or block dropped).
    resynced: bool,
}

// ---------------------------------------------------------------------------
// Build
// ---------------------------------------------------------------------------

impl Pipeline {
    /// Open devices, start the DSP worker and play. Blocks the caller (supervisor) until the
    /// voice chain is built on the worker (model load) or fails.
    pub fn start(spec: &PipelineSpec, control: Arc<Control>) -> Result<Self> {
        let host = cpal::default_host();
        let health = Arc::new(Health::default());
        let mut info = PipelineInfo::default();

        // --- capture -----------------------------------------------------------
        let mut streams = Vec::new();
        let (tone_prod, cap_cons) = if spec.input == TEST_TONE_INPUT {
            {
                info.input_name = "test tone (440 Hz, paced by the system clock)".into();
                info.input_rate = ENGINE_SAMPLE_RATE;
                info.input_channels = 1;
                info.input_format = "F32".into();
                info.input_buffer = Some(TONE_CHUNK as u32);
                let (prod, cons) = rtrb::RingBuffer::<f32>::new(ENGINE_SAMPLE_RATE as usize / 2);
                (Some(prod), cons)
            }
        } else {
            {
                let in_dev =
                    find_device(&host, &spec.input, true).ok_or(YetiError::DeviceNotFound)?;
                let in_sc = in_dev
                    .default_input_config()
                    .map_err(|e| map_build_err(e, "input config"))?;
                let in_cfg = stream_config(&in_sc, spec.buffer_frames);
                info.input_name = dev_name(&in_dev);
                info.input_rate = in_cfg.sample_rate;
                info.input_channels = in_cfg.channels;
                info.input_format = format!("{:?}", in_sc.sample_format());
                let (cap_prod, cap_cons) =
                    rtrb::RingBuffer::<f32>::new(in_cfg.sample_rate as usize / 2);
                log::info!(
                    "opening capture {:?} {:?} {}",
                    info.input_name,
                    in_cfg,
                    info.input_format
                );
                let in_stream = by_format!(
                    fmt_ok(in_sc.sample_format()),
                    build_capture,
                    &in_dev,
                    in_cfg,
                    cap_prod,
                    control.clone(),
                    health.clone(),
                    true
                )?;
                info.input_buffer = in_stream.buffer_size().ok();
                streams.push(in_stream);
                log::info!("capture stream built (buffer {:?})", info.input_buffer);
                (None, cap_cons)
            }
        };

        // --- outputs -------------------------------------------------------------
        let open_out = |sel: &OutputSel, count_underruns: bool| -> Result<Option<OpenedOutput>> {
            let Some(dev) = resolve_output(&host, sel) else {
                return Ok(None);
            };
            let sc = dev
                .default_output_config()
                .map_err(|e| map_build_err(e, "output config"))?;
            let cfg = stream_config(&sc, spec.buffer_frames);
            let (prod, cons) = rtrb::RingBuffer::<f32>::new(cfg.sample_rate as usize / 4);
            let started = Arc::new(AtomicBool::new(false));
            let s = by_format!(
                fmt_ok(sc.sample_format()),
                build_playback,
                &dev,
                cfg,
                cons,
                started.clone(),
                control.clone(),
                health.clone(),
                count_underruns
            )?;
            let buf = s.buffer_size().ok();
            Ok(Some((
                s,
                prod,
                started,
                dev_name(&dev),
                cfg.sample_rate,
                buf,
                cfg.channels,
            )))
        };

        let out = match &spec.output {
            Some(sel) => open_out(sel, true)?,
            None => None,
        };
        let mon = match &spec.monitor {
            Some(sel) => open_out(sel, false)?,
            None => None,
        };

        let in_buf = info.input_buffer.unwrap_or(512) as usize;
        // Output ring target: one device callback + one DSP block of slack on each side,
        // plus the capture burst (the DSP thread is fed in capture-callback-sized bursts).
        let target_for = |rate: u32, buf: Option<u32>| -> usize {
            let b = buf.unwrap_or(512) as usize;
            let in_at_rate = in_buf * rate as usize / info.input_rate.max(1) as usize;
            let blk = BLOCK_FRAMES * rate as usize / ENGINE_SAMPLE_RATE as usize;
            b + blk + in_at_rate / 2 + rate as usize / 500 // + 2 ms
        };
        let mut out_path = None;
        if let Some((s, prod, started, name, rate, buf, chans)) = out {
            let t = target_for(rate, buf);
            info.output_name = Some(name);
            info.output_rate = rate;
            info.output_buffer = buf;
            info.output_target_fill = t as u32;
            info.output_channels = chans;
            out_path = Some(OutputPath::new(prod, started, rate, t)?);
            streams.push(s);
        }
        let mut mon_path = None;
        if let Some((s, prod, started, name, rate, buf, _)) = mon {
            info.monitor_name = Some(name);
            info.monitor_rate = rate;
            mon_path = Some(OutputPath::new(prod, started, rate, target_for(rate, buf))?);
            streams.push(s);
        }

        // --- DSP worker ------------------------------------------------------------
        let input_stage = InputStage::new(info.input_rate, ENGINE_SAMPLE_RATE, false)?;
        let stop = Arc::new(AtomicBool::new(false));
        let (ready_tx, ready_rx) = mpsc::channel::<Result<f32>>();
        let (ref_tx, ref_rx) = mpsc::channel::<Option<RefSource>>();
        let worker = DspWorker {
            control: control.clone(),
            stop: stop.clone(),
            cap: cap_cons,
            input_stage,
            out: out_path,
            mon: mon_path,
            ref_rx,
            in_rate: info.input_rate,
            in_buf_frames: in_buf,
            out_rate: info.output_rate,
            out_buf_frames: info.output_buffer.unwrap_or(0) as usize,
        };
        let h2 = health.clone();
        let dsp = std::thread::Builder::new()
            .name("yeti-dsp".into())
            .spawn(move || {
                let r =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| worker.run(ready_tx)));
                if r.is_err() {
                    log::error!("DSP worker panicked");
                }
                h2.dsp_dead.store(true, Relaxed);
            })
            .map_err(backend)?;
        log::info!("streams built; waiting for the voice chain on the DSP worker");
        let t_chain = Instant::now();
        let chain_latency = match ready_rx.recv_timeout(Duration::from_secs(60)) {
            Ok(Ok(l)) => l,
            Ok(Err(e)) => {
                stop.store(true, Relaxed);
                let _ = dsp.join();
                return Err(e);
            }
            Err(_) => {
                stop.store(true, Relaxed);
                return Err(YetiError::Backend("DSP worker did not start".into()));
            }
        };
        info.chain_latency_ms = chain_latency;
        log::info!(
            "voice chain ready in {:.0} ms",
            t_chain.elapsed().as_secs_f64() * 1000.0
        );

        // Build `Self` before playing: on a play / tone-spawn error below, `Drop` stops and
        // joins the DSP worker instead of leaking it.
        let mut p = Self {
            tone: None,
            streams,
            loopback: None,
            #[cfg(target_os = "macos")]
            tap: None,
            reference_excludes_self: false,
            ref_tx,
            dsp: Some(dsp),
            stop,
            health,
            info,
            control,
        };
        // Outputs first (they discard silently until fed), then capture.
        for s in p.streams.iter().rev() {
            s.play().map_err(|e| map_build_err(e, "play"))?;
        }
        if let Some(tp) = tone_prod {
            p.tone = Some(spawn_tone(tp, p.stop.clone(), p.control.clone()).map_err(backend)?);
        }
        Ok(p)
    }

    /// Start/stop capture of the system output as AEC far-end reference.
    ///
    /// macOS: first a **self-excluding** global process tap ([`crate::mac_tap`]) so the
    /// monitor output of this app never enters the reference; if that fails, cpal 0.18's
    /// built-in loopback of the default output device (includes this process). Windows:
    /// cpal's WASAPI loopback (includes this process). Needs macOS 14.2+ and the "System
    /// Audio Recording" permission (silence without it). Reference stream errors never stop
    /// the main pipeline.
    pub fn set_reference(&mut self, enabled: bool) -> Result<()> {
        if enabled == self.loopback.is_some() {
            return Ok(());
        }
        if !enabled {
            let _ = self.ref_tx.send(None);
            self.loopback = None;
            #[cfg(target_os = "macos")]
            {
                self.tap = None;
            }
            self.reference_excludes_self = false;
            return Ok(());
        }
        let host = cpal::default_host();
        let ref_health = Arc::new(Health::default());
        #[cfg(target_os = "macos")]
        {
            match crate::mac_tap::ProcessTap::new_excluding_self() {
                Ok(tap) => {
                    let dev = find_device(&host, &tap.device_name, true);
                    match dev.map(|d| self.open_reference(&d, true, ref_health.clone())) {
                        Some(Ok(())) => {
                            self.reference_excludes_self = tap.excludes_self;
                            log::info!(
                                "AEC reference: self-excluding process tap {:?} (excludes self: {})",
                                tap.device_name,
                                tap.excludes_self
                            );
                            self.tap = Some(tap);
                            return Ok(());
                        }
                        Some(Err(e)) => log::warn!(
                            "process tap capture failed ({e}); falling back to cpal loopback"
                        ),
                        None => log::warn!(
                            "process tap device {:?} not visible to cpal; falling back to cpal loopback",
                            tap.device_name
                        ),
                    }
                }
                Err(e) => log::warn!("{e}; falling back to cpal loopback"),
            }
        }
        let dev = host
            .default_output_device()
            .ok_or(YetiError::Backend("no default output device".into()))?;
        self.open_reference(&dev, false, ref_health)?;
        self.reference_excludes_self = false;
        log::info!(
            "AEC reference: cpal loopback of {:?} (not self-excluding)",
            dev_name(&dev)
        );
        Ok(())
    }

    /// Build + play the reference capture stream from `dev` (an input device such as the
    /// tap aggregate, or an output device → cpal loopback) and hand its ring to the worker.
    fn open_reference(
        &mut self,
        dev: &cpal::Device,
        is_input: bool,
        health: Arc<Health>,
    ) -> Result<()> {
        // Use the device's own current format so cpal does not change its rate.
        let sc = if is_input {
            dev.default_input_config()
        } else {
            dev.default_output_config()
        }
        .map_err(|e| map_build_err(e, "reference config"))?;
        let cfg = stream_config(&sc, None);
        let (prod, cons) = rtrb::RingBuffer::<f32>::new(cfg.sample_rate as usize / 2);
        let s = by_format!(
            fmt_ok(sc.sample_format()),
            build_capture,
            dev,
            cfg,
            prod,
            self.control.clone(),
            health,
            false
        )?;
        s.play().map_err(|e| map_build_err(e, "play reference"))?;
        let _ = self.ref_tx.send(Some(RefSource {
            cons,
            rate: cfg.sample_rate,
        }));
        self.loopback = Some(s);
        Ok(())
    }

    pub fn reference_active(&self) -> bool {
        self.loopback.is_some()
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        for s in &self.streams {
            let _ = s.pause();
        }
        self.loopback = None;
        self.streams.clear();
        self.stop.store(true, Relaxed);
        if let Some(h) = self.tone.take() {
            let _ = h.join();
        }
        if let Some(h) = self.dsp.take() {
            let _ = h.join();
        }
    }
}

// ---------------------------------------------------------------------------
// DSP worker
// ---------------------------------------------------------------------------

struct DspWorker {
    control: Arc<Control>,
    stop: Arc<AtomicBool>,
    cap: rtrb::Consumer<f32>,
    input_stage: InputStage,
    out: Option<OutputPath>,
    mon: Option<OutputPath>,
    ref_rx: mpsc::Receiver<Option<RefSource>>,
    in_rate: u32,
    in_buf_frames: usize,
    out_rate: u32,
    out_buf_frames: usize,
}

struct RefPath {
    cons: rtrb::Consumer<f32>,
    stage: InputStage,
    ctl: DriftController,
    buf: Vec<f32>,
    block: Vec<f32>,
    warm: bool,
}

impl DspWorker {
    fn run(mut self, ready: mpsc::Sender<Result<f32>>) {
        let c = self.control.clone();
        let tel = &c.telemetry;
        let mut seen_gen = c.params_gen.load(Relaxed);
        let mut params = *c.params.lock().unwrap();
        // VoiceChain is !Send (DFN): construct it here, on the worker.
        let mut chain = match yeti_dsp::VoiceChain::new(params) {
            Ok(ch) => ch,
            Err(e) => {
                let _ = ready.send(Err(e));
                return;
            }
        };
        let chain_latency = chain.latency_ms();
        tel.dsp_latency_ms.set(chain_latency);
        let mut mon_on = false;
        // After the (slow, allocating) model load: run with real-time scheduling.
        if !crate::rt_priority::promote_current_thread(10.0, 5.0) {
            log::warn!("DSP worker: real-time scheduling not available");
        }
        let _ = ready.send(Ok(chain_latency));

        let mut in_buf = vec![0.0f32; self.input_stage.max_needed() + 16];
        let mut raw = vec![0.0f32; BLOCK_FRAMES];
        let mut block = vec![0.0f32; BLOCK_FRAMES];
        let mut mon_block = vec![0.0f32; BLOCK_FRAMES];
        let mut refp: Option<RefPath> = None;
        let mut cap_fill_avg = 0.0f32;
        let mut cpu_avg = 0.0f32;
        let (mut bucket_raw, mut bucket_clean, mut bucket_n) = (0.0f32, 0.0f32, 0usize);
        let block_s = BLOCK_FRAMES as f32 / ENGINE_SAMPLE_RATE as f32;

        while !self.stop.load(Relaxed) {
            // Reference (loopback) source changes.
            while let Ok(src) = self.ref_rx.try_recv() {
                refp = src.and_then(|s| {
                    let stage = InputStage::new(s.rate, ENGINE_SAMPLE_RATE, true).ok()?;
                    let max = stage.max_needed();
                    let target = (s.rate as f64 * 0.03).max(max as f64 * 2.0);
                    Some(RefPath {
                        cons: s.cons,
                        stage,
                        ctl: DriftController::new(DriftConfig::for_target(target)),
                        buf: vec![0.0; max + 16],
                        block: vec![0.0; BLOCK_FRAMES],
                        warm: false,
                    })
                });
                tel.reference_rms.set(0.0);
            }

            let need = self.input_stage.needed();
            let avail = self.cap.slots();
            if avail < need {
                std::thread::sleep(Duration::from_micros(700));
                continue;
            }
            // Backlog beyond the block being taken now (queueing latency on the capture side).
            cap_fill_avg += 0.01 * ((avail - need) as f32 - cap_fill_avg);
            tel.capture_fill.set(cap_fill_avg);
            let t0 = Instant::now();
            let _ = self.cap.pop_entire_slice(&mut in_buf[..need]);
            if self.input_stage.process(&in_buf[..need], &mut raw).is_err() {
                continue;
            }

            let g = c.params_gen.load(Relaxed);
            if g != seen_gen {
                seen_gen = g;
                params = *c.params.lock().unwrap();
                chain.set_params(&params);
                tel.dsp_latency_ms.set(chain.latency_ms());
            }

            // Far-end reference block (drift-compensated to the capture clock).
            let mut far: Option<&[f32]> = None;
            if let Some(r) = &mut refp {
                let rneed = r.stage.needed();
                let rav = r.cons.slots();
                if !r.warm && rav as f64 >= r.ctl.config().target_fill {
                    r.warm = true;
                }
                if r.warm && rav >= rneed {
                    let _ = r.cons.pop_entire_slice(&mut r.buf[..rneed]);
                    if r.stage.process(&r.buf[..rneed], &mut r.block).is_ok() {
                        // Fill above target → consume faster → lower out/in ratio.
                        let ratio = r.ctl.update((rav - rneed) as f64, block_s as f64);
                        r.stage.set_correction(ratio);
                        tel.reference_drift_ratio.set(ratio as f32);
                        tel.reference_fill
                            .set(r.ctl.smoothed_fill().unwrap_or(0.0) as f32);
                        let (p, rms) = peak_rms(&r.block);
                        tel.reference_rms.set(rms);
                        if p > tel.reference_max_peak.get() {
                            tel.reference_max_peak.set(p);
                        }
                        far = Some(&r.block);
                    }
                } else if r.warm {
                    tel.reference_underruns.fetch_add(1, Relaxed);
                }
            }

            block.copy_from_slice(&raw);
            let m = if c.bypass_chain.load(Relaxed) {
                yeti_dsp::ChainMetrics::default()
            } else {
                chain.process_block(&mut block, far)
            };
            // A/B take: cleaned audio before the software mute.
            c.ab.on_block(&raw, &block);
            if c.probe.enabled.load(Relaxed) {
                // Latency probe: the chain ran (CPU), forward the impulse train unchanged.
                block.copy_from_slice(&raw);
            } else if params.output_muted {
                block.fill(0.0);
            }

            // Meters + waveform.
            let (ip, ir) = peak_rms(&raw);
            let (op, or) = peak_rms(&block);
            tel.in_peak.set(ip);
            tel.in_rms.set(ir);
            tel.out_peak.set(op);
            tel.out_rms.set(or);
            if ip > tel.max_input_peak.get() {
                tel.max_input_peak.set(ip);
            }
            tel.noise_reduction_db.set(m.noise_reduction_db);
            tel.gain_change_db.set(m.gain_change_db);
            tel.gate_open.store(m.gate_open, Relaxed);
            tel.voice_probability.set(m.voice_probability);
            bucket_raw = bucket_raw.max(ip);
            bucket_clean = bucket_clean.max(op);
            bucket_n += 1;
            if bucket_n == crate::telemetry::BLOCKS_PER_BUCKET {
                tel.push_wave(bucket_raw.min(1.0), bucket_clean.min(1.0));
                (bucket_raw, bucket_clean, bucket_n) = (0.0, 0.0, 0);
            }

            // Outputs.
            if let Some(o) = &mut self.out {
                let r = o.push(&block);
                if r.overflow > 0 {
                    tel.output_overruns.fetch_add(1, Relaxed);
                }
                if r.resynced {
                    tel.output_resyncs.fetch_add(1, Relaxed);
                }
                tel.drift_ratio.set(o.ctl.ratio() as f32);
                tel.output_fill
                    .set(o.ctl.smoothed_fill().unwrap_or(0.0) as f32);
            }
            if let Some(mp) = &mut self.mon {
                // A/B playback replaces the live monitor signal while it runs.
                let ab_play = c.ab.playback(&mut mon_block);
                let on = ab_play || c.monitor_enabled.load(Relaxed);
                if on && !mon_on {
                    mp.prefill();
                }
                mon_on = on;
                if on {
                    let v = c.monitor_volume.get().clamp(0.0, 1.0);
                    if ab_play {
                        for d in mon_block.iter_mut() {
                            *d *= v;
                        }
                    } else {
                        for (d, s) in mon_block.iter_mut().zip(&block) {
                            *d = s * v;
                        }
                    }
                    let _ = mp.push(&mon_block);
                    tel.monitor_drift_ratio.set(mp.ctl.ratio() as f32);
                }
            }

            // Stats.
            let dt = t0.elapsed().as_secs_f32();
            cpu_avg += 0.02 * (dt / block_s * 100.0 - cpu_avg);
            tel.cpu_percent.set(cpu_avg);
            tel.blocks_processed.fetch_add(1, Relaxed);
            let mut lat = self.in_buf_frames as f32 / self.in_rate as f32
                + cap_fill_avg / self.in_rate as f32
                + self.input_stage.delay() as f32 / ENGINE_SAMPLE_RATE as f32
                + block_s
                + tel.dsp_latency_ms.get() / 1000.0;
            if let Some(o) = &self.out {
                lat += o.stage.delay() as f32 / self.out_rate as f32
                    + o.ctl.smoothed_fill().unwrap_or(0.0) as f32 / self.out_rate as f32
                    + self.out_buf_frames as f32 / self.out_rate as f32;
            }
            tel.latency_ms.set(lat * 1000.0);
        }
    }
}
