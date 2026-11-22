//! RNNoise noise suppressor (pure-Rust `nnnoiseless`), the low-CPU option.
//!
//! RNNoise has no attenuation limit of its own, so the strength slider is implemented as a
//! dry/wet mix against a latency-aligned copy of the input: `out = wet·(1−a) + dry·a` with
//! `a = 10^(−lim/20)`, `lim = 6 + 94·strength` dB (same mapping as DFN's `atten_lim`).

use crate::Processor;
use nnnoiseless::DenoiseState;
use yeti_types::{BLOCK_FRAMES, ENGINE_SAMPLE_RATE, Result, YetiError};

/// nnnoiseless works on 16-bit-scaled floats.
const I16_SCALE: f32 = 32768.0;

/// Algorithmic latency of `DenoiseState::process_frame` in frames (overlap-add synthesis of a
/// 960-sample window with a 480 hop). Verified by cross-correlation in
/// `tests/fixture_quality.rs` (chain reported 528 = 480 + limiter 48, measured 528).
pub const RNNOISE_LATENCY_FRAMES: usize = BLOCK_FRAMES;

pub struct RnnSuppressor {
    state: Box<DenoiseState<'static>>,
    input: [f32; BLOCK_FRAMES],
    output: [f32; BLOCK_FRAMES],
    /// Previous block's input = dry signal aligned with the current output.
    dry: [f32; BLOCK_FRAMES],
    dry_mix: f32,
    first: bool,
    /// Voice-activity probability of the last frame, 0..=1.
    pub last_vad: f32,
}

impl RnnSuppressor {
    pub fn new() -> Self {
        let mut s = Self {
            state: DenoiseState::new(),
            input: [0.0; BLOCK_FRAMES],
            output: [0.0; BLOCK_FRAMES],
            dry: [0.0; BLOCK_FRAMES],
            dry_mix: 0.0,
            first: true,
            last_vad: 0.0,
        };
        s.set_strength(0.8);
        s
    }

    /// Map the UI strength slider (0..=1) to a maximum attenuation of 6..=100 dB.
    pub fn set_strength(&mut self, strength: f32) {
        let lim = 6.0 + strength.clamp(0.0, 1.0) * 94.0;
        self.dry_mix = if lim >= 100.0 {
            0.0
        } else {
            10f32.powf(-lim / 20.0)
        };
    }
}

impl Default for RnnSuppressor {
    fn default() -> Self {
        Self::new()
    }
}

impl Processor for RnnSuppressor {
    fn name(&self) -> &'static str {
        "rnnoise"
    }
    fn prepare(&mut self, sample_rate: u32, max_block_frames: usize) -> Result<()> {
        if sample_rate != ENGINE_SAMPLE_RATE || max_block_frames != DenoiseState::FRAME_SIZE {
            return Err(YetiError::Unsupported(
                "RNNoise requires 48 kHz / 480-frame blocks",
            ));
        }
        Ok(())
    }
    fn process(&mut self, block: &mut [f32]) {
        let n = block.len().min(BLOCK_FRAMES);
        self.input.fill(0.0);
        for (d, s) in self.input.iter_mut().zip(block.iter()) {
            *d = s.clamp(-1.0, 1.0) * I16_SCALE;
        }
        self.last_vad = self.state.process_frame(&mut self.output, &self.input);
        let a = self.dry_mix;
        if self.first {
            // First output frame contains fade-in artefacts.
            self.output.fill(0.0);
            self.first = false;
        }
        for i in 0..n {
            let wet = self.output[i] / I16_SCALE;
            let dry = self.dry[i];
            self.dry[i] = block[i];
            block[i] = wet * (1.0 - a) + dry * a;
        }
    }
    fn reset(&mut self) {
        // DenoiseState has no reset; rebuilding it is a small bounded allocation
        // (tolerated on the DSP worker, see PLAN.md §3).
        self.state = DenoiseState::new();
        self.dry.fill(0.0);
        self.first = true;
    }
    fn latency_frames(&self) -> usize {
        RNNOISE_LATENCY_FRAMES
    }
}
