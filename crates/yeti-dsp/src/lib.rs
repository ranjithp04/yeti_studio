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

use yeti_types::{BLOCK_FRAMES, DspParams, ENGINE_SAMPLE_RATE, Result, YetiError};

#[cfg(feature = "dfn")]
pub mod dfn;

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
    /// Input RMS − post-NS RMS, dB (≥ 0).
    pub noise_reduction_db: f32,
    /// Auto-level + limiter gain change, dB (negative = reduction).
    pub gain_change_db: f32,
    pub gate_open: bool,
    /// 0..=1.
    pub voice_probability: f32,
}

/// The full chain driven by [`DspParams`]. Stub: passthrough + output mute until Phase 2.
pub struct VoiceChain {
    params: DspParams,
    sample_rate: u32,
}

impl VoiceChain {
    /// Build the chain for 48 kHz / 480-frame blocks. May load NN models (slow, allocates).
    pub fn new(params: DspParams) -> Result<Self> {
        Ok(Self {
            params,
            sample_rate: ENGINE_SAMPLE_RATE,
        })
    }

    /// Apply new parameters. Called on the DSP thread between blocks; must be cheap
    /// (no model reload — switching `NoiseModel` keeps both models prepared).
    pub fn set_params(&mut self, params: &DspParams) {
        self.params = *params;
    }

    pub fn params(&self) -> &DspParams {
        &self.params
    }

    /// Process one block. `far_end` = loopback reference block (same length) for AEC,
    /// `None` when echo cancel is off or the reference underran.
    pub fn process_block(&mut self, near: &mut [f32], far_end: Option<&[f32]>) -> ChainMetrics {
        debug_assert!(near.len() <= BLOCK_FRAMES);
        let _ = far_end;
        if self.params.output_muted {
            near.fill(0.0);
        }
        ChainMetrics::default()
    }

    /// Total algorithmic latency of the enabled stages, in ms.
    pub fn latency_ms(&self) -> f32 {
        0.0 * 1000.0 / self.sample_rate as f32
    }

    /// Whether echo cancellation is compiled in (`aec` feature).
    pub const fn aec_available() -> bool {
        cfg!(feature = "aec")
    }

    /// Whether DeepFilterNet is compiled in (`dfn` feature).
    pub const fn dfn_available() -> bool {
        cfg!(feature = "dfn")
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
    fn reset(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_chain_is_passthrough_and_mute_works() {
        let mut c = VoiceChain::new(DspParams::default()).unwrap();
        c.prepare(48_000, BLOCK_FRAMES).unwrap();
        let mut b = vec![0.25f32; BLOCK_FRAMES];
        c.process(&mut b);
        assert!(b.iter().all(|&x| x == 0.25));
        let p = DspParams {
            output_muted: true,
            ..DspParams::default()
        };
        c.set_params(&p);
        c.process(&mut b);
        assert!(b.iter().all(|&x| x == 0.0));
    }
}
