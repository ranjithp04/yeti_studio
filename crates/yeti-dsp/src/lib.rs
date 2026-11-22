//! Yeti Studio voice chain.
//!
//! Owner: **DSP engineer**.
//!
//! Signal format: 48 kHz mono `f32`, fixed [`BLOCK_FRAMES`] (480) blocks, processed in place.
//! Order: (AEC) → HPF → noise suppression → gate → EQ/de-esser → auto-level → limiter.
//!
//! Real-time rules (PLAN.md §3): [`Processor::process`] runs on the **DSP worker thread**,
//! never inside a cpal callback. Own code must not allocate/lock there; bounded allocation
//! inside third-party NS/AEC (tract, WebRTC APM) is tolerated. All allocation belongs in
//! `new`/`prepare`.
//!
//! Offline tools: `yeti-render` (WAV in → WAV out through a preset) and `yeti-dsp-bench`
//! (DFN vs RNNoise latency/CPU/quality, AEC ERLE). Results: `docs/DSP_RESULTS.md`.

use yeti_types::{BLOCK_FRAMES, DspParams, ENGINE_SAMPLE_RATE, NoiseModel, Result, YetiError};

#[cfg(feature = "aec")]
pub mod aec;
pub mod biquad;
#[cfg(feature = "dfn")]
pub mod dfn;
pub mod fixtures;
pub mod rnnoise;
pub mod stages;

use biquad::rms_db;
use stages::{AutoLevel, Gate, HighPass, Limiter, VoiceEq};

/// One stage of the voice chain (or the whole chain).
///
/// **Not `Send`**: DeepFilterNet's tract state holds `Rc`s, so a DFN-backed processor (and
/// therefore [`VoiceChain`] with feature `dfn`) is `!Send`. The engine must **construct the
/// chain on the DSP worker thread** (pass a factory/`DspParams` into the thread, not a chain).
pub trait Processor {
    /// Short identifier for logs/benchmarks, e.g. `"hpf"`, `"dfn"`.
    fn name(&self) -> &'static str;

    /// Allocate state for `sample_rate` and blocks of up to `max_block_frames`.
    /// Called off the RT path; may allocate. Engine always passes 48 000 / 480.
    fn prepare(&mut self, sample_rate: u32, max_block_frames: usize) -> Result<()>;

    /// Process one mono block in place. `block.len()` ≤ `max_block_frames` from `prepare`
    /// (normally exactly [`BLOCK_FRAMES`]). Must not panic on silence/denormals.
    fn process(&mut self, block: &mut [f32]);

    /// Clear internal state (e.g. after a device reconnect) without reallocating.
    fn reset(&mut self);

    /// Algorithmic latency added by this processor, in frames at the prepared rate.
    fn latency_frames(&self) -> usize {
        0
    }
}

/// Per-block analysis the chain hands back to the engine for meters.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ChainMetrics {
    /// Input RMS − post-NS RMS, dB (≥ 0), smoothed over ~50 ms.
    pub noise_reduction_db: f32,
    /// Auto-level + limiter gain change, dB (negative = reduction).
    pub gain_change_db: f32,
    pub gate_open: bool,
    /// 0..=1.
    pub voice_probability: f32,
}

/// Which noise suppressor actually runs (the requested one may be unavailable).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveNs {
    Off,
    RnNoise,
    DeepFilterNet,
}

/// The full chain driven by [`DspParams`].
///
/// Both NS models are loaded in [`VoiceChain::new`] (DFN ≈ 0.2 s) so switching
/// `noise.model` in [`VoiceChain::set_params`] is cheap. If DFN is not compiled in or fails
/// to load, `DeepFilterNet` falls back to RNNoise ([`VoiceChain::active_noise_model`]).
pub struct VoiceChain {
    params: DspParams,
    sample_rate: u32,
    hpf: HighPass,
    rnn: rnnoise::RnnSuppressor,
    #[cfg(feature = "dfn")]
    dfn: Option<dfn::DfnSuppressor>,
    dfn_error: Option<String>,
    gate: Gate,
    eq: VoiceEq,
    agc: AutoLevel,
    limiter: Limiter,
    #[cfg(feature = "aec")]
    aec: Option<aec::EchoCancel>,
    aec_error: Option<String>,
    active_ns: ActiveNs,
    nr_smooth: f32,
}

impl VoiceChain {
    /// Build the chain for 48 kHz / 480-frame blocks. Loads NN models (slow, allocates):
    /// call on the DSP worker thread.
    pub fn new(params: DspParams) -> Result<Self> {
        let fs = ENGINE_SAMPLE_RATE;
        #[cfg(feature = "dfn")]
        let (dfn, dfn_error) = match dfn::DfnSuppressor::new() {
            Ok(d) => (Some(d), None),
            Err(e) => (None, Some(e.to_string())),
        };
        #[cfg(not(feature = "dfn"))]
        let dfn_error = Some("built without the `dfn` feature".to_string());
        #[cfg(feature = "aec")]
        let (aec, aec_error) = match aec::EchoCancel::new() {
            Ok(a) => (Some(a), None),
            Err(e) => (None, Some(e.to_string())),
        };
        #[cfg(not(feature = "aec"))]
        let aec_error = Some("built without the `aec` feature".to_string());
        let mut c = Self {
            params,
            sample_rate: fs,
            hpf: HighPass::new(params.high_pass, fs),
            rnn: rnnoise::RnnSuppressor::new(),
            #[cfg(feature = "dfn")]
            dfn,
            dfn_error,
            gate: Gate::new(params.gate, fs),
            eq: VoiceEq::new(params.eq, fs),
            agc: AutoLevel::new(params.auto_level, fs),
            limiter: Limiter::new(params.limiter, fs),
            #[cfg(feature = "aec")]
            aec,
            aec_error,
            active_ns: ActiveNs::Off,
            nr_smooth: 0.0,
        };
        c.apply_params(&params, true);
        Ok(c)
    }

    fn resolve_ns(&self, p: &DspParams) -> ActiveNs {
        if !p.noise.enabled {
            return ActiveNs::Off;
        }
        match p.noise.model {
            NoiseModel::RnNoise => ActiveNs::RnNoise,
            NoiseModel::DeepFilterNet => {
                #[cfg(feature = "dfn")]
                if self.dfn.is_some() {
                    return ActiveNs::DeepFilterNet;
                }
                ActiveNs::RnNoise
            }
        }
    }

    fn apply_params(&mut self, p: &DspParams, force: bool) {
        let ns = self.resolve_ns(p);
        if ns != self.active_ns || force {
            // Newly selected model starts from a clean state.
            match ns {
                ActiveNs::RnNoise if !force => self.rnn.reset(),
                #[cfg(feature = "dfn")]
                ActiveNs::DeepFilterNet if !force => {
                    if let Some(d) = self.dfn.as_mut() {
                        d.reset();
                    }
                }
                _ => {}
            }
            self.active_ns = ns;
        }
        self.rnn.set_strength(p.noise.strength);
        #[cfg(feature = "dfn")]
        if let Some(d) = self.dfn.as_mut()
            && (force || p.noise.strength != self.params.noise.strength)
        {
            d.set_strength(p.noise.strength);
        }
        #[cfg(feature = "aec")]
        if p.echo_cancel.enabled
            && !self.params.echo_cancel.enabled
            && !force
            && let Some(a) = self.aec.as_mut()
        {
            a.reset();
        }
        self.hpf.set_params(&p.high_pass);
        self.gate.set_params(&p.gate);
        self.eq.set_params(&p.eq);
        self.agc.set_params(&p.auto_level);
        self.limiter.set_params(&p.limiter);
        self.params = *p;
    }

    /// Apply new parameters. Called on the DSP thread between blocks; cheap (no model
    /// reload; switching `NoiseModel` resets the newly selected model's state).
    pub fn set_params(&mut self, params: &DspParams) {
        if *params != self.params {
            self.apply_params(params, false);
        }
    }

    pub fn params(&self) -> &DspParams {
        &self.params
    }

    /// The suppressor that actually runs for the current params.
    pub fn active_noise_model(&self) -> ActiveNs {
        self.active_ns
    }

    /// Why DFN is unavailable (not compiled in / load failure), if it is.
    pub fn dfn_error(&self) -> Option<&str> {
        self.dfn_error.as_deref()
    }

    /// Why AEC is unavailable, if it is.
    pub fn aec_error(&self) -> Option<&str> {
        self.aec_error.as_deref()
    }

    fn aec_active(&self) -> bool {
        #[cfg(feature = "aec")]
        {
            self.params.echo_cancel.enabled && self.aec.is_some()
        }
        #[cfg(not(feature = "aec"))]
        {
            false
        }
    }

    /// Process one block. `far_end` = loopback reference block (same length) for AEC,
    /// `None` when echo cancel is off or the reference underran.
    ///
    /// NS and AEC need exactly [`BLOCK_FRAMES`] frames; shorter blocks skip those stages.
    pub fn process_block(&mut self, near: &mut [f32], far_end: Option<&[f32]>) -> ChainMetrics {
        debug_assert!(near.len() <= BLOCK_FRAMES);
        for x in near.iter_mut() {
            if !x.is_finite() {
                *x = 0.0;
            }
        }
        let full = near.len() == BLOCK_FRAMES;
        let in_db = rms_db(near);

        #[cfg(feature = "aec")]
        if self.params.echo_cancel.enabled
            && full
            && let Some(a) = self.aec.as_mut()
        {
            a.process_with_reference(near, far_end);
        }
        #[cfg(not(feature = "aec"))]
        let _ = far_end;

        self.hpf.process(near);

        let mut vp = None;
        if full {
            match self.active_ns {
                ActiveNs::Off => {}
                ActiveNs::RnNoise => {
                    self.rnn.process(near);
                    vp = Some(self.rnn.last_vad);
                }
                ActiveNs::DeepFilterNet =>
                {
                    #[cfg(feature = "dfn")]
                    if let Some(d) = self.dfn.as_mut() {
                        d.process(near);
                        vp = Some(d.voice_probability());
                    }
                }
            }
        }
        let post_ns_db = rms_db(near);
        let nr = if self.active_ns == ActiveNs::Off {
            0.0
        } else {
            (in_db - post_ns_db).max(0.0)
        };
        self.nr_smooth = 0.8 * self.nr_smooth + 0.2 * nr;

        self.gate.voice_probability = vp.unwrap_or(0.0);
        self.gate.vad_valid = vp.is_some();
        self.gate.process(near);
        let gate_open = self.gate.is_open();

        self.eq.process(near);

        self.agc.voice_active = gate_open;
        self.agc.process(near);

        self.limiter.process(near);

        if self.params.output_muted {
            near.fill(0.0);
        }
        let lim_db = if self.params.limiter.enabled {
            self.limiter.gain_reduction_db
        } else {
            0.0
        };
        ChainMetrics {
            noise_reduction_db: self.nr_smooth,
            gain_change_db: self.agc.gain_change_db + lim_db,
            gate_open,
            voice_probability: vp
                .unwrap_or(if gate_open { 1.0 } else { 0.0 })
                .clamp(0.0, 1.0),
        }
    }

    /// Total algorithmic latency of the enabled stages, in frames.
    pub fn latency_frames(&self) -> usize {
        let ns = match self.active_ns {
            ActiveNs::Off => 0,
            ActiveNs::RnNoise => self.rnn.latency_frames(),
            #[cfg(feature = "dfn")]
            ActiveNs::DeepFilterNet => self.dfn.as_ref().map_or(0, Processor::latency_frames),
            #[cfg(not(feature = "dfn"))]
            ActiveNs::DeepFilterNet => 0,
        };
        #[cfg(feature = "aec")]
        let aec = if self.aec_active() {
            aec::AEC_LATENCY_FRAMES
        } else {
            0
        };
        #[cfg(not(feature = "aec"))]
        let aec = 0;
        aec + ns + self.limiter.latency_frames()
    }

    /// Total algorithmic latency of the enabled stages, in ms.
    pub fn latency_ms(&self) -> f32 {
        self.latency_frames() as f32 * 1000.0 / self.sample_rate as f32
    }

    /// Whether echo cancellation is compiled in (`aec` feature).
    pub const fn aec_available() -> bool {
        cfg!(feature = "aec")
    }

    /// Whether DeepFilterNet is compiled in (`dfn` feature).
    pub const fn dfn_available() -> bool {
        cfg!(feature = "dfn")
    }

    /// Echo canceller currently running (enabled in params and compiled/loaded).
    pub fn echo_cancel_active(&self) -> bool {
        self.aec_active()
    }
}

impl Processor for VoiceChain {
    fn name(&self) -> &'static str {
        "voice-chain"
    }
    fn prepare(&mut self, sample_rate: u32, max_block_frames: usize) -> Result<()> {
        if sample_rate != ENGINE_SAMPLE_RATE || max_block_frames > BLOCK_FRAMES {
            return Err(YetiError::Unsupported(
                "voice chain runs at 48 kHz / 480-frame blocks",
            ));
        }
        self.sample_rate = sample_rate;
        Ok(())
    }
    fn process(&mut self, block: &mut [f32]) {
        self.process_block(block, None);
    }
    fn reset(&mut self) {
        self.hpf.reset();
        self.rnn.reset();
        #[cfg(feature = "dfn")]
        if let Some(d) = self.dfn.as_mut() {
            d.reset();
        }
        #[cfg(feature = "aec")]
        if let Some(a) = self.aec.as_mut() {
            a.reset();
        }
        self.gate.reset();
        self.eq.reset();
        self.agc.reset();
        self.limiter.reset();
        self.nr_smooth = 0.0;
    }
    fn latency_frames(&self) -> usize {
        VoiceChain::latency_frames(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yeti_types::Preset;

    #[test]
    fn mute_silences_output_and_silence_stays_finite() {
        let mut c = VoiceChain::new(DspParams::default()).unwrap();
        c.prepare(48_000, BLOCK_FRAMES).unwrap();
        let mut b = vec![0.0f32; BLOCK_FRAMES];
        for _ in 0..50 {
            c.process(&mut b);
            assert!(b.iter().all(|x| x.is_finite()));
        }
        let p = DspParams {
            output_muted: true,
            ..DspParams::default()
        };
        c.set_params(&p);
        let mut b = vec![0.25f32; BLOCK_FRAMES];
        c.process(&mut b);
        assert!(b.iter().all(|&x| x == 0.0));
    }

    #[test]
    fn nan_input_does_not_poison_chain() {
        let mut c = VoiceChain::new(DspParams::for_preset(Preset::Gaming)).unwrap();
        let mut b = vec![f32::NAN; BLOCK_FRAMES];
        c.process(&mut b);
        let mut b = vec![0.1f32; BLOCK_FRAMES];
        for _ in 0..10 {
            c.process(&mut b);
        }
        assert!(b.iter().all(|x| x.is_finite()));
    }

    #[test]
    fn model_switch_is_cheap_and_reports_active_model() {
        let mut p = DspParams::default();
        p.noise.model = NoiseModel::RnNoise;
        let mut c = VoiceChain::new(p).unwrap();
        assert_eq!(c.active_noise_model(), ActiveNs::RnNoise);
        p.noise.model = NoiseModel::DeepFilterNet;
        let t = std::time::Instant::now();
        c.set_params(&p);
        assert!(t.elapsed().as_millis() < 50);
        let want = if VoiceChain::dfn_available() {
            ActiveNs::DeepFilterNet
        } else {
            ActiveNs::RnNoise
        };
        assert_eq!(c.active_noise_model(), want);
        p.noise.enabled = false;
        c.set_params(&p);
        assert_eq!(c.active_noise_model(), ActiveNs::Off);
        assert_eq!(c.latency_frames(), stages::LIMITER_LOOKAHEAD);
    }
}
