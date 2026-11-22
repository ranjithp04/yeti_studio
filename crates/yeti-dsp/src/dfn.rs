//! DeepFilterNet noise suppressor (libDF + tract), feature `dfn`.
//!
//! Verified by the Phase 0 spike (`spikes/dfn_spike`): 48 kHz, hop 480, ~0.6 ms/frame
//! release build on M-series, ~21 dB noise-only reduction on a synthetic pink-noise mix.

use crate::Processor;
use df::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::Array2;
use yeti_types::{BLOCK_FRAMES, ENGINE_SAMPLE_RATE, Result, YetiError};

/// DFN3 default model wrapped as a [`Processor`]. Block size must equal the model hop (480).
pub struct DfnSuppressor {
    model: DfTract,
    noisy: Array2<f32>,
    enh: Array2<f32>,
    /// Last local SNR estimate from the model, dB.
    pub last_lsnr_db: f32,
}

impl DfnSuppressor {
    /// Load the embedded default model (allocates, ~200 ms).
    pub fn new() -> Result<Self> {
        let model = DfTract::new(DfParams::default(), &RuntimeParams::default_with_ch(1))
            .map_err(|e| YetiError::Backend(format!("DeepFilterNet load: {e:#}")))?;
        if model.hop_size != BLOCK_FRAMES || model.sr != ENGINE_SAMPLE_RATE as usize {
            return Err(YetiError::Unsupported("DFN model hop/sr mismatch"));
        }
        Ok(Self {
            noisy: Array2::zeros((1, model.hop_size)),
            enh: Array2::zeros((1, model.hop_size)),
            model,
            last_lsnr_db: 0.0,
        })
    }

    /// Map the UI strength slider (0..=1) to DFN's attenuation limit (dB).
    pub fn set_strength(&mut self, strength: f32) {
        let s = strength.clamp(0.0, 1.0);
        self.model.set_atten_lim(6.0 + s * 94.0);
    }
}

impl Processor for DfnSuppressor {
    fn name(&self) -> &'static str {
        "dfn"
    }
    fn prepare(&mut self, sample_rate: u32, max_block_frames: usize) -> Result<()> {
        if sample_rate != ENGINE_SAMPLE_RATE || max_block_frames != self.model.hop_size {
            return Err(YetiError::Unsupported(
                "DFN requires 48 kHz / 480-frame blocks",
            ));
        }
        Ok(())
    }
    fn process(&mut self, block: &mut [f32]) {
        let n = block.len().min(self.model.hop_size);
        self.noisy.fill(0.0);
        for (d, s) in self.noisy.iter_mut().zip(block.iter()) {
            *d = *s;
        }
        match self.model.process(self.noisy.view(), self.enh.view_mut()) {
            Ok(lsnr) => {
                self.last_lsnr_db = lsnr;
                for (d, s) in block[..n].iter_mut().zip(self.enh.iter()) {
                    *d = *s;
                }
            }
            Err(e) => log::error!("DFN process failed: {e}"),
        }
    }
    fn reset(&mut self) {
        let _ = self.model.init();
    }
    fn latency_frames(&self) -> usize {
        // STFT overlap + model lookahead (estimate; DSP engineer to measure in Phase 2).
        (self.model.fft_size - self.model.hop_size) + self.model.lookahead * self.model.hop_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dfn_loads_and_attenuates_noise() {
        let mut d = DfnSuppressor::new().unwrap();
        d.prepare(48_000, BLOCK_FRAMES).unwrap();
        // Deterministic white-ish noise (LCG), 1 s.
        let mut seed = 12345u32;
        let (mut e_in, mut e_out) = (0f64, 0f64);
        for i in 0..100 {
            let mut b = [0f32; BLOCK_FRAMES];
            for x in b.iter_mut() {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                *x = ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.1;
            }
            let ein: f64 = b.iter().map(|x| (*x as f64).powi(2)).sum();
            d.process(&mut b);
            if i >= 50 {
                e_in += ein;
                e_out += b.iter().map(|x| (*x as f64).powi(2)).sum::<f64>();
            }
        }
        let red_db = 10.0 * (e_in / e_out.max(1e-20)).log10();
        assert!(red_db > 10.0, "noise reduction only {red_db:.1} dB");
    }
}
