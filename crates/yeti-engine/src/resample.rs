//! Mono resampling stages around the 48 kHz / 480-frame DSP core, built on rubato's
//! adjustable [`Async`] sinc resampler. All buffers are allocated in the constructors;
//! `process` runs on the DSP worker thread.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Adjustable, Async, FixedAsync, Resampler, SincInterpolationParameters, WindowFunction,
};
use yeti_types::{BLOCK_FRAMES, Result, YetiError};

/// Max relative ratio change the drift controller may request (±1 %).
const MAX_REL: f64 = 1.01;

fn sinc_params() -> SincInterpolationParameters {
    SincInterpolationParameters::new(128, WindowFunction::BlackmanHarris2)
}

fn err(e: impl std::fmt::Display) -> YetiError {
    YetiError::Backend(format!("resampler: {e}"))
}

/// Device rate → 48 kHz, always emitting exactly one [`BLOCK_FRAMES`] block per call.
/// Bypassed (pure copy) when the device already runs at 48 kHz and no drift correction is
/// requested (`adjustable = false`).
pub struct InputStage {
    rs: Option<Async<f32>>,
    nominal: f64,
}

impl InputStage {
    pub fn new(in_rate: u32, out_rate: u32, adjustable: bool) -> Result<Self> {
        let nominal = out_rate as f64 / in_rate as f64;
        let rs = if in_rate == out_rate && !adjustable {
            None
        } else {
            Some(
                Async::<f32>::new_sinc(
                    nominal,
                    MAX_REL,
                    &sinc_params(),
                    BLOCK_FRAMES,
                    1,
                    FixedAsync::Output,
                )
                .map_err(err)?,
            )
        };
        Ok(Self { rs, nominal })
    }

    /// Input frames required for the next block.
    pub fn needed(&self) -> usize {
        self.rs
            .as_ref()
            .map_or(BLOCK_FRAMES, |r| r.input_frames_next())
    }

    /// Upper bound of [`needed`](Self::needed) (for buffer sizing).
    pub fn max_needed(&self) -> usize {
        self.rs
            .as_ref()
            .map_or(BLOCK_FRAMES, |r| r.input_frames_max())
    }

    /// Apply a drift correction (1.0 = nominal). No-op when bypassed.
    pub fn set_correction(&mut self, r: f64) {
        if let Some(rs) = &mut self.rs {
            let _ = rs.set_resample_ratio(self.nominal * r, true);
        }
    }

    /// Algorithmic delay in output frames.
    pub fn delay(&self) -> usize {
        self.rs.as_ref().map_or(0, |r| r.output_delay())
    }

    /// `input.len()` must equal [`needed`](Self::needed); `out.len() == BLOCK_FRAMES`.
    pub fn process(&mut self, input: &[f32], out: &mut [f32]) -> Result<()> {
        match &mut self.rs {
            None => {
                out.copy_from_slice(&input[..BLOCK_FRAMES]);
                Ok(())
            }
            Some(rs) => {
                let i = InterleavedSlice::new(input, 1, input.len()).map_err(err)?;
                let n = out.len();
                let mut o = InterleavedSlice::new_mut(out, 1, n).map_err(err)?;
                rs.process_into_buffer(&i, &mut o, None).map_err(err)?;
                Ok(())
            }
        }
    }
}

/// 48 kHz block → device rate with drift correction; output length varies per call.
pub struct OutputStage {
    rs: Async<f32>,
    nominal: f64,
}

impl OutputStage {
    pub fn new(in_rate: u32, out_rate: u32) -> Result<Self> {
        let nominal = out_rate as f64 / in_rate as f64;
        let rs = Async::<f32>::new_sinc(
            nominal,
            MAX_REL,
            &sinc_params(),
            BLOCK_FRAMES,
            1,
            FixedAsync::Input,
        )
        .map_err(err)?;
        Ok(Self { rs, nominal })
    }

    /// Upper bound of frames one call can produce (for buffer sizing).
    pub fn max_output(&self) -> usize {
        self.rs.output_frames_max()
    }

    pub fn set_correction(&mut self, r: f64) {
        let _ = self.rs.set_resample_ratio(self.nominal * r, true);
    }

    pub fn delay(&self) -> usize {
        self.rs.output_delay()
    }

    /// Resample one [`BLOCK_FRAMES`] block into `out` (len ≥ [`max_output`](Self::max_output));
    /// returns frames written.
    pub fn process(&mut self, block: &[f32], out: &mut [f32]) -> Result<usize> {
        let i = InterleavedSlice::new(block, 1, block.len()).map_err(err)?;
        let n = out.len();
        let mut o = InterleavedSlice::new_mut(out, 1, n).map_err(err)?;
        let (_, written) = self.rs.process_into_buffer(&i, &mut o, None).map_err(err)?;
        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_stage_44k1_to_48k_block_sizes() {
        let mut s = InputStage::new(44_100, 48_000, false).unwrap();
        let mut total_in = 0usize;
        let mut out = vec![0.0; BLOCK_FRAMES];
        for _ in 0..200 {
            let n = s.needed();
            let inp = vec![0.1f32; n];
            s.process(&inp, &mut out).unwrap();
            total_in += n;
        }
        // 200 blocks of 480 @ 48k = 2 s → ≈ 88 200 input frames.
        assert!((total_in as i64 - 88_200).abs() < 600, "{total_in}");
    }

    #[test]
    fn output_stage_tracks_correction() {
        let mut s = OutputStage::new(48_000, 48_000).unwrap();
        let block = vec![0.0f32; BLOCK_FRAMES];
        let mut out = vec![0.0; s.max_output()];
        s.set_correction(1.002);
        let mut produced = 0usize;
        for _ in 0..500 {
            produced += s.process(&block, &mut out).unwrap();
        }
        let expect = 500.0 * 480.0 * 1.002;
        assert!(
            (produced as f64 - expect).abs() < 100.0,
            "{produced} vs {expect}"
        );
    }
}
