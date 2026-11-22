//! Acoustic echo cancellation: WebRTC AEC3 via `sonora` (pure-Rust port of WebRTC audio
//! processing), feature `aec`. Pure Rust, so it builds on macOS and Windows with no C++/meson.
//!
//! Only the echo canceller (+ its high-pass) is enabled: noise suppression and gain control are
//! done by our own chain. 48 kHz mono, 10 ms (480-frame) frames. The far-end reference is the
//! system playback captured via loopback (engine), one block per near block. The render/capture
//! delay is estimated by AEC3 itself.

use crate::Processor;
use sonora::config::{EchoCanceller, HighPassFilter};
use sonora::{AudioProcessing, Config, StreamConfig};
use yeti_types::{BLOCK_FRAMES, ENGINE_SAMPLE_RATE, Result, YetiError};

/// Capture-path delay added by the APM at 48 kHz (measured, see tests).
pub const AEC_LATENCY_FRAMES: usize = 430;

fn build_apm() -> AudioProcessing {
    let sc = StreamConfig::new(ENGINE_SAMPLE_RATE, 1);
    AudioProcessing::builder()
        .config(Config {
            echo_canceller: Some(EchoCanceller::default()),
            high_pass_filter: Some(HighPassFilter::default()),
            ..Config::default()
        })
        .capture_config(sc)
        .render_config(sc)
        .build()
}

pub struct EchoCancel {
    apm: AudioProcessing,
    near: [f32; BLOCK_FRAMES],
    far: [f32; BLOCK_FRAMES],
    render_out: [f32; BLOCK_FRAMES],
    /// Frames where the APM returned an error (near passed through).
    pub errors: u64,
}

impl EchoCancel {
    /// Create the APM (allocates; call on the DSP worker or before starting).
    pub fn new() -> Result<Self> {
        let apm = build_apm();
        if StreamConfig::new(ENGINE_SAMPLE_RATE, 1).num_frames() != BLOCK_FRAMES {
            return Err(YetiError::Unsupported("APM frame size != 480"));
        }
        Ok(Self {
            apm,
            near: [0.0; BLOCK_FRAMES],
            far: [0.0; BLOCK_FRAMES],
            render_out: [0.0; BLOCK_FRAMES],
            errors: 0,
        })
    }

    /// Process one 480-frame block: `far` = loopback reference (or `None` → silence).
    /// Shorter blocks are passed through untouched.
    pub fn process_with_reference(&mut self, near: &mut [f32], far: Option<&[f32]>) {
        if near.len() != BLOCK_FRAMES {
            return;
        }
        match far {
            Some(f) if f.len() == BLOCK_FRAMES => self.far.copy_from_slice(f),
            _ => self.far.fill(0.0),
        }
        if self
            .apm
            .process_render_f32(&[&self.far[..]], &mut [&mut self.render_out[..]])
            .is_err()
        {
            self.errors += 1;
        }
        self.near.copy_from_slice(near);
        if self
            .apm
            .process_capture_f32(&[&self.near[..]], &mut [near])
            .is_err()
        {
            near.copy_from_slice(&self.near);
            self.errors += 1;
        }
    }
}

impl Processor for EchoCancel {
    fn name(&self) -> &'static str {
        "aec"
    }
    fn prepare(&mut self, sample_rate: u32, max_block_frames: usize) -> Result<()> {
        if sample_rate != ENGINE_SAMPLE_RATE || max_block_frames != BLOCK_FRAMES {
            return Err(YetiError::Unsupported(
                "AEC requires 48 kHz / 480-frame blocks",
            ));
        }
        Ok(())
    }
    /// Without a reference this only runs the APM's capture path (render = silence).
    fn process(&mut self, block: &mut [f32]) {
        self.process_with_reference(block, None);
    }
    /// Fresh AEC state (rebuilds the APM; allocates — DSP worker only, never a callback).
    fn reset(&mut self) {
        self.apm = build_apm();
    }
    fn latency_frames(&self) -> usize {
        AEC_LATENCY_FRAMES
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures;

    /// The APM (3-band split + AEC3 framing) delays the capture path by a constant
    /// [`AEC_LATENCY_FRAMES`]; measured with broadband noise (no pitch ambiguity).
    #[test]
    fn aec_latency_matches_constant() {
        for seed in [5, 6] {
            let mut r = fixtures::Rng::new(seed);
            let x: Vec<f32> = (0..96_000).map(|_| (r.bipolar() * 0.1) as f32).collect();
            let mut a = EchoCancel::new().unwrap();
            let mut y = x.clone();
            for b in y.chunks_mut(BLOCK_FRAMES) {
                a.process_with_reference(b, None);
            }
            let d = fixtures::estimate_delay(&x, &y, 48_000, 24_000, 2_000);
            assert_eq!(d, AEC_LATENCY_FRAMES, "seed {seed}");
        }
    }
}
