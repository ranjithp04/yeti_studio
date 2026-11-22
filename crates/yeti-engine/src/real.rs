//! The real engine: cpal capture → DSP worker → virtual mic / monitor, driven by a
//! supervisor thread that owns the [`Pipeline`] and handles hot-plug.
//!
//! All [`Engine`] methods are non-blocking: lifecycle requests are messages to the
//! supervisor; telemetry is read from atomics ([`crate::telemetry`]).

use crate::pipeline::{
    Control, OutputSel, Pipeline, PipelineInfo, PipelineSpec, TEST_TONE_INPUT, find_device,
};
use crate::{Engine, EngineConfig};
use std::collections::VecDeque;
use std::sync::atomic::Ordering::Relaxed;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use yeti_device::{DeviceControl, YETI_NAME};
use yeti_types::{
    AbState, AbVariant, DeviceCommand, DeviceEvent, DeviceState, DspParams, EngineEvent,
    EngineStats, EngineStatus, MeterSnapshot, MonitorSettings, Preset, Result, WaveformSnapshot,
    YetiError,
};

/// Default virtual-mic feed device on macOS (Phase 5 driver).
pub const MAC_FEED_DEVICE: &str = "Yeti Studio Mic (Feed)";
/// Default virtual-mic feed device on Windows (VB-CABLE).
pub const WIN_FEED_DEVICE: &str = "CABLE Input";

/// Extra diagnostics beyond [`EngineStats`] (for `yeti-passthrough` and the Pro view).
#[derive(Debug, Clone, Default)]
pub struct EngineDiagnostics {
    pub info: Option<PipelineInfo>,
    pub capture_fill_frames: f32,
    pub output_fill_frames: f32,
    pub output_overruns: u64,
    /// Output ring re-centred (startup, stalls, or after an underrun).
    pub output_resyncs: u64,
    pub monitor_drift_ratio: f32,
    pub reference_active: bool,
    /// The reference capture excludes this process (macOS process tap).
    pub reference_excludes_self: bool,
    pub reference_fill_frames: f32,
    pub reference_drift_ratio: f32,
    pub reference_rms_db: f32,
    pub reference_max_peak_db: f32,
    pub max_input_peak_db: f32,
}

enum Msg {
    Start(EngineConfig),
    Stop,
    Restart,
    Device(DeviceEvent),
    Shutdown,
}

struct Shared {
    control: Arc<Control>,
    status: Mutex<EngineStatus>,
    preset: Mutex<Preset>,
    monitor: Mutex<MonitorSettings>,
    events: Mutex<VecDeque<EngineEvent>>,
    device_state: Mutex<Option<DeviceState>>,
    info: Mutex<Option<PipelineInfo>>,
    reference_active: std::sync::atomic::AtomicBool,
    reference_excludes_self: std::sync::atomic::AtomicBool,
}

impl Shared {
    fn push(&self, ev: EngineEvent) {
        let mut q = self.events.lock().unwrap();
        if q.len() > 256 {
            q.pop_front();
        }
        q.push_back(ev);
    }

    fn set_status(&self, st: EngineStatus) {
        let mut s = self.status.lock().unwrap();
        if *s != st {
            *s = st.clone();
            drop(s);
            self.push(EngineEvent::Status(st));
        }
    }
}

/// cpal-based engine (capture → DSP thread → virtual mic).
pub struct YetiEngine {
    shared: Arc<Shared>,
    device: Option<Arc<dyn DeviceControl>>,
    tx: mpsc::Sender<Msg>,
    supervisor: Mutex<Option<JoinHandle<()>>>,
}

impl Default for YetiEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl YetiEngine {
    pub fn new() -> Self {
        let params = DspParams::for_preset(Preset::Gaming);
        let shared = Arc::new(Shared {
            control: Arc::new(Control::new(params)),
            status: Mutex::new(EngineStatus::Stopped),
            preset: Mutex::new(Preset::Gaming),
            monitor: Mutex::new(MonitorSettings::default()),
            events: Mutex::new(VecDeque::new()),
            device_state: Mutex::new(None),
            info: Mutex::new(None),
            reference_active: Default::default(),
            reference_excludes_self: Default::default(),
        });
        let (tx, rx) = mpsc::channel();
        let device: Option<Arc<dyn DeviceControl>> = match yeti_device::watch_yeti() {
            Ok(d) => Some(Arc::from(d)),
            Err(e) => {
                log::warn!("Yeti hardware control unavailable: {e}");
                None
            }
        };
        if let Some(d) = &device {
            let st = d.state().ok().filter(|s| s.connected);
            *shared.device_state.lock().unwrap() = st;
            let sh = shared.clone();
            let tx2 = tx.clone();
            d.set_event_callback(Some(Arc::new(move |ev: DeviceEvent| {
                match &ev {
                    DeviceEvent::Connected(s) | DeviceEvent::Changed(s) => {
                        *sh.device_state.lock().unwrap() = Some(s.clone());
                    }
                    DeviceEvent::Disconnected => {
                        if let Some(s) = sh.device_state.lock().unwrap().as_mut() {
                            s.connected = false;
                        }
                    }
                }
                sh.push(EngineEvent::Device(ev.clone()));
                let _ = tx2.send(Msg::Device(ev));
            })));
        }
        let sh = shared.clone();
        let sup = std::thread::Builder::new()
            .name("yeti-engine".into())
            .spawn(move || Supervisor::new(sh).run(rx))
            .expect("spawn engine supervisor");
        Self {
            shared,
            device,
            tx,
            supervisor: Mutex::new(Some(sup)),
        }
    }

    /// Skip the voice chain (raw passthrough) — diagnostics only.
    pub fn set_bypass_chain(&self, bypass: bool) {
        self.shared.control.bypass_chain.store(bypass, Relaxed);
    }

    pub fn diagnostics(&self) -> EngineDiagnostics {
        let t = &self.shared.control.telemetry;
        EngineDiagnostics {
            info: self.shared.info.lock().unwrap().clone(),
            capture_fill_frames: t.capture_fill.get(),
            output_fill_frames: t.output_fill.get(),
            output_overruns: t.output_overruns.load(Relaxed),
            output_resyncs: t.output_resyncs.load(Relaxed),
            monitor_drift_ratio: t.monitor_drift_ratio.get(),
            reference_active: self.shared.reference_active.load(Relaxed),
            reference_excludes_self: self.shared.reference_excludes_self.load(Relaxed),
            reference_fill_frames: t.reference_fill.get(),
            reference_drift_ratio: t.reference_drift_ratio.get(),
            reference_rms_db: crate::telemetry::to_db(t.reference_rms.get()),
            reference_max_peak_db: crate::telemetry::to_db(t.reference_max_peak.get()),
            max_input_peak_db: crate::telemetry::to_db(t.max_input_peak.get()),
        }
    }

    fn publish_params(&self, p: DspParams) {
        *self.shared.control.params.lock().unwrap() = p;
        self.shared.control.params_gen.fetch_add(1, Relaxed);
    }
}

impl Drop for YetiEngine {
    fn drop(&mut self) {
        if let Some(d) = &self.device {
            d.set_event_callback(None);
        }
        let _ = self.tx.send(Msg::Shutdown);
        if let Some(h) = self.supervisor.lock().unwrap().take() {
            let _ = h.join();
        }
    }
}

// ---------------------------------------------------------------------------
// Supervisor
// ---------------------------------------------------------------------------

struct Supervisor {
    sh: Arc<Shared>,
    pipeline: Option<Pipeline>,
    /// Private aggregate wrapping the hidden virtual-mic Feed device (macOS).
    #[cfg(target_os = "macos")]
    feed_alias: Option<crate::mac_feed::DeviceAlias>,
    want: Option<EngineConfig>,
    retry_at: Option<Instant>,
    warned_ref: bool,
}

impl Supervisor {
    fn new(sh: Arc<Shared>) -> Self {
        Self {
            sh,
            pipeline: None,
            #[cfg(target_os = "macos")]
            feed_alias: None,
            want: None,
            retry_at: None,
            warned_ref: false,
        }
    }

    fn run(mut self, rx: mpsc::Receiver<Msg>) {
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(Msg::Start(cfg)) => {
                    if self.pipeline.is_none() || self.want.as_ref() != Some(&cfg) {
                        self.pipeline = None;
                        self.want = Some(cfg);
                        self.try_start();
                    }
                }
                Ok(Msg::Stop) => {
                    self.want = None;
                    self.teardown();
                    self.sh.set_status(EngineStatus::Stopped);
                }
                Ok(Msg::Restart) => {
                    if self.want.is_some() {
                        self.teardown();
                        self.retry_at = Some(Instant::now() + Duration::from_millis(300));
                    }
                }
                Ok(Msg::Device(DeviceEvent::Disconnected)) => {
                    if self.want.is_some() {
                        self.teardown();
                        self.sh.set_status(EngineStatus::WaitingForDevice);
                    }
                }
                Ok(Msg::Device(DeviceEvent::Connected(_))) => {
                    if self.want.is_some() && self.pipeline.is_none() {
                        // Let the HAL finish publishing both Yeti devices.
                        self.retry_at = Some(Instant::now() + Duration::from_millis(700));
                    }
                }
                Ok(Msg::Device(DeviceEvent::Changed(_))) => {}
                Ok(Msg::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if self.sh.control.ab.take_ready() {
                self.sh.push(EngineEvent::AbReady);
            }

            // Health: a stream lost its device or the DSP worker died.
            if let Some(p) = &self.pipeline {
                if p.health.device_lost.load(Relaxed) {
                    log::warn!("audio device lost; waiting for it to return");
                    self.teardown();
                    self.sh.set_status(EngineStatus::WaitingForDevice);
                    self.retry_at = Some(Instant::now() + Duration::from_secs(1));
                } else if p.health.dsp_dead.load(Relaxed) {
                    self.teardown();
                    self.sh
                        .set_status(EngineStatus::Error("DSP worker stopped".into()));
                    self.want = None;
                }
            }
            if self.want.is_some()
                && self.pipeline.is_none()
                && self.retry_at.is_some_and(|t| Instant::now() >= t)
            {
                self.retry_at = None;
                self.try_start();
            }
            self.update_reference();
        }
        self.teardown();
    }

    fn teardown(&mut self) {
        self.sh.control.ab.interrupt();
        self.pipeline = None;
        #[cfg(target_os = "macos")]
        {
            self.feed_alias = None;
        }
        self.sh.reference_active.store(false, Relaxed);
    }

    /// Main output when none is configured: the virtual mic ("Yeti Studio Mic (Feed)" on
    /// macOS, VB-CABLE "CABLE Input" on Windows). macOS without the driver: the default
    /// output plus a visible warning. Windows without VB-CABLE: no main output + warning.
    fn virtual_mic_output(&mut self, host: &cpal::Host) -> Option<OutputSel> {
        #[cfg(target_os = "macos")]
        {
            self.feed_alias = None;
            if find_device(host, MAC_FEED_DEVICE, false).is_some() {
                return Some(OutputSel::Named(MAC_FEED_DEVICE.to_string()));
            }
            // The Feed is hidden from the device list: open it by UID via a private aggregate.
            match crate::mac_feed::DeviceAlias::new(
                crate::mac_feed::FEED_DEVICE_UID,
                MAC_FEED_DEVICE,
            ) {
                Ok(alias) => {
                    let name = alias.device_name.clone();
                    self.feed_alias = Some(alias);
                    return Some(OutputSel::Named(name));
                }
                Err(YetiError::DeviceNotFound) => {}
                Err(e) => log::warn!("virtual mic feed: {e}"),
            }
            self.sh.push(EngineEvent::Warning(
                "Virtual mic \"Yeti Studio Mic\" is not installed: processed audio is playing \
                 on the default output instead (use headphones to avoid feedback). Install it \
                 with `cargo xtask bundle-mac`, then `cargo xtask install-driver`."
                    .to_string(),
            ));
            Some(OutputSel::Default)
        }
        #[cfg(windows)]
        {
            match crate::windows_routing::virtual_mic_output(host, None) {
                Ok((dev, _)) => {
                    use cpal::traits::DeviceTrait;
                    let name = dev
                        .description()
                        .map(|d| d.name().to_string())
                        .unwrap_or_else(|_| WIN_FEED_DEVICE.to_string());
                    Some(OutputSel::Named(name))
                }
                Err(e) => {
                    log::warn!("virtual mic: {e}");
                    self.sh.push(EngineEvent::Warning(
                        yeti_device::endpoint_names::VB_CABLE_MISSING_MESSAGE.to_string(),
                    ));
                    None
                }
            }
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            let _ = host;
            self.sh.push(EngineEvent::Warning(
                YetiError::VirtualMicMissing("no virtual mic on this platform".into()).to_string(),
            ));
            None
        }
    }

    fn update_reference(&mut self) {
        let Some(p) = &mut self.pipeline else { return };
        let Some(cfg) = &self.want else { return };
        let echo = self.sh.control.params.lock().unwrap().echo_cancel.enabled;
        let want = cfg
            .capture_reference
            .unwrap_or(echo && yeti_dsp::VoiceChain::aec_available());
        if want != p.reference_active() {
            match p.set_reference(want) {
                Ok(()) => {}
                Err(e) => {
                    if !self.warned_ref {
                        self.sh.push(EngineEvent::Warning(format!(
                            "echo-cancel reference capture failed: {e}"
                        )));
                        self.warned_ref = true;
                    }
                }
            }
            self.sh
                .reference_active
                .store(p.reference_active(), Relaxed);
            self.sh
                .reference_excludes_self
                .store(p.reference_excludes_self, Relaxed);
        }
    }

    fn try_start(&mut self) {
        let Some(cfg) = self.want.clone() else { return };
        self.sh.set_status(EngineStatus::Starting);
        let host = cpal::default_host();
        let input = cfg
            .input_device
            .clone()
            .unwrap_or_else(|| YETI_NAME.to_string());
        if input != TEST_TONE_INPUT && find_device(&host, &input, true).is_none() {
            self.sh.set_status(EngineStatus::WaitingForDevice);
            return;
        }
        let output = match cfg.output_device.as_deref() {
            Some("none") => None,
            Some("default") => Some(OutputSel::Default),
            Some(n) => Some(OutputSel::Named(n.to_string())),
            None => self.virtual_mic_output(&host),
        };
        let monitor = match cfg.monitor_device.as_deref() {
            Some("none") => None,
            Some("default") => Some(OutputSel::Default),
            Some(n) => Some(OutputSel::Named(n.to_string())),
            None => find_device(&host, YETI_NAME, false)
                .map(|_| OutputSel::Named(YETI_NAME.to_string())),
        };
        let spec = PipelineSpec {
            input,
            output,
            monitor,
            buffer_frames: cfg.buffer_frames,
        };
        self.sh.control.telemetry.reset();
        match Pipeline::start(&spec, self.sh.control.clone()) {
            Ok(p) => {
                log::info!("engine running: {:?}", p.info);
                *self.sh.info.lock().unwrap() = Some(p.info.clone());
                self.pipeline = Some(p);
                self.warned_ref = false;
                self.sh.set_status(EngineStatus::Running);
            }
            Err(YetiError::DeviceNotFound | YetiError::DeviceDisconnected) => {
                self.sh.set_status(EngineStatus::WaitingForDevice);
                self.retry_at = Some(Instant::now() + Duration::from_secs(2));
            }
            Err(e) => {
                log::error!("engine start failed: {e}");
                self.sh.set_status(EngineStatus::Error(e.to_string()));
                self.want = None;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Engine impl
// ---------------------------------------------------------------------------

impl Engine for YetiEngine {
    fn start(&self, config: EngineConfig) -> Result<()> {
        self.tx
            .send(Msg::Start(config))
            .map_err(|_| YetiError::Backend("engine supervisor stopped".into()))
    }

    fn stop(&self) {
        let _ = self.tx.send(Msg::Stop);
    }

    fn status(&self) -> EngineStatus {
        self.shared.status.lock().unwrap().clone()
    }

    fn set_params(&self, params: DspParams) {
        let preset = [Preset::Gaming, Preset::Meeting]
            .into_iter()
            .find(|p| {
                let mut q = DspParams::for_preset(*p);
                q.output_muted = params.output_muted;
                q == params
            })
            .unwrap_or(Preset::Custom);
        *self.shared.preset.lock().unwrap() = preset;
        self.publish_params(params);
    }

    fn params(&self) -> DspParams {
        *self.shared.control.params.lock().unwrap()
    }

    fn set_preset(&self, preset: Preset) {
        let muted = self.is_muted();
        let mut p = DspParams::for_preset(preset);
        p.output_muted = muted;
        *self.shared.preset.lock().unwrap() = preset;
        self.publish_params(p);
    }

    fn preset(&self) -> Preset {
        *self.shared.preset.lock().unwrap()
    }

    fn set_muted(&self, muted: bool) {
        let mut p = self.params();
        p.output_muted = muted;
        self.publish_params(p);
    }

    fn is_muted(&self) -> bool {
        self.shared.control.params.lock().unwrap().output_muted
    }

    fn set_monitor(&self, monitor: MonitorSettings) {
        *self.shared.monitor.lock().unwrap() = monitor;
        self.shared
            .control
            .monitor_volume
            .set(monitor.volume.clamp(0.0, 1.0));
        self.shared
            .control
            .monitor_enabled
            .store(monitor.enabled, Relaxed);
    }

    fn monitor(&self) -> MonitorSettings {
        *self.shared.monitor.lock().unwrap()
    }

    fn meters(&self) -> MeterSnapshot {
        if self.status() != EngineStatus::Running {
            return MeterSnapshot::default();
        }
        self.shared.control.telemetry.meters()
    }

    fn waveform(&self, out: &mut WaveformSnapshot) {
        if self.status() != EngineStatus::Running {
            out.raw.clear();
            out.cleaned.clear();
            return;
        }
        self.shared.control.telemetry.waveform(out);
    }

    fn stats(&self) -> EngineStats {
        self.shared.control.telemetry.stats()
    }

    fn poll_events(&self) -> Vec<EngineEvent> {
        self.shared.events.lock().unwrap().drain(..).collect()
    }

    fn device_state(&self) -> Option<DeviceState> {
        self.shared.device_state.lock().unwrap().clone()
    }

    fn device_command(&self, cmd: DeviceCommand) -> Result<()> {
        let d = self.device.as_ref().ok_or(YetiError::DeviceNotFound)?;
        d.apply(cmd)?;
        if let Ok(st) = d.state() {
            *self.shared.device_state.lock().unwrap() = Some(st);
        }
        if matches!(cmd, DeviceCommand::SetSampleRate(_)) {
            // Rebuild streams so the input resampler matches the new device rate.
            let _ = self.tx.send(Msg::Restart);
        }
        Ok(())
    }

    fn ab_record(&self, seconds: f32) -> Result<()> {
        if self.status() != EngineStatus::Running {
            return Err(YetiError::NotRunning);
        }
        self.shared.control.ab.record(seconds)
    }
    fn ab_play(&self, variant: AbVariant) -> Result<()> {
        if self.status() != EngineStatus::Running {
            return Err(YetiError::NotRunning);
        }
        let has_monitor = self
            .shared
            .info
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|i| i.monitor_name.is_some());
        if !has_monitor {
            return Err(YetiError::Backend(
                "no monitor output (Yeti headphones) to play the A/B take on".into(),
            ));
        }
        self.shared.control.ab.play(variant)
    }
    fn ab_stop(&self) {
        self.shared.control.ab.stop();
    }
    fn ab_state(&self) -> AbState {
        self.shared.control.ab.state()
    }
    fn aec_available(&self) -> bool {
        yeti_dsp::VoiceChain::aec_available()
    }
}
