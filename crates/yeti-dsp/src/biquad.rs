//! Minimal RBJ-cookbook biquad (transposed direct form II, `f64` state).
//!
//! Own implementation instead of the `biquad` crate: we only need five filter shapes, and
//! `f64` state keeps the 30–80 Hz high-pass accurate at 48 kHz. Coefficient updates are
//! allocation-free and keep the filter state.

use std::f64::consts::PI;

#[derive(Debug, Clone, Copy)]
pub struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl Default for Biquad {
    /// Identity filter.
    fn default() -> Self {
        Self {
            b0: 1.0,
            b1: 0.0,
            b2: 0.0,
            a1: 0.0,
            a2: 0.0,
            z1: 0.0,
            z2: 0.0,
        }
    }
}

/// Clamp the frequency below Nyquist so coefficient maths never blows up.
fn w0(fs: f64, f: f64) -> f64 {
    2.0 * PI * f.clamp(1.0, fs * 0.49) / fs
}

impl Biquad {
    fn set_raw(&mut self, b0: f64, b1: f64, b2: f64, a0: f64, a1: f64, a2: f64) {
        self.b0 = b0 / a0;
        self.b1 = b1 / a0;
        self.b2 = b2 / a0;
        self.a1 = a1 / a0;
        self.a2 = a2 / a0;
    }

    pub fn highpass(fs: f64, f: f64, q: f64) -> Self {
        let mut b = Self::default();
        b.set_highpass(fs, f, q);
        b
    }

    pub fn set_highpass(&mut self, fs: f64, f: f64, q: f64) {
        let w = w0(fs, f);
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q);
        self.set_raw(
            (1.0 + c) / 2.0,
            -(1.0 + c),
            (1.0 + c) / 2.0,
            1.0 + alpha,
            -2.0 * c,
            1.0 - alpha,
        );
    }

    pub fn set_lowpass(&mut self, fs: f64, f: f64, q: f64) {
        let w = w0(fs, f);
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q);
        self.set_raw(
            (1.0 - c) / 2.0,
            1.0 - c,
            (1.0 - c) / 2.0,
            1.0 + alpha,
            -2.0 * c,
            1.0 - alpha,
        );
    }

    pub fn set_bandpass(&mut self, fs: f64, f: f64, q: f64) {
        let w = w0(fs, f);
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q);
        self.set_raw(alpha, 0.0, -alpha, 1.0 + alpha, -2.0 * c, 1.0 - alpha);
    }

    pub fn set_peaking(&mut self, fs: f64, f: f64, q: f64, gain_db: f64) {
        let a = 10f64.powf(gain_db / 40.0);
        let w = w0(fs, f);
        let (s, c) = w.sin_cos();
        let alpha = s / (2.0 * q);
        self.set_raw(
            1.0 + alpha * a,
            -2.0 * c,
            1.0 - alpha * a,
            1.0 + alpha / a,
            -2.0 * c,
            1.0 - alpha / a,
        );
    }

    /// Shelf slope S = 1.
    pub fn set_low_shelf(&mut self, fs: f64, f: f64, gain_db: f64) {
        let a = 10f64.powf(gain_db / 40.0);
        let w = w0(fs, f);
        let (s, c) = w.sin_cos();
        let alpha = s / 2.0 * 2f64.sqrt();
        let sa = 2.0 * a.sqrt() * alpha;
        self.set_raw(
            a * ((a + 1.0) - (a - 1.0) * c + sa),
            2.0 * a * ((a - 1.0) - (a + 1.0) * c),
            a * ((a + 1.0) - (a - 1.0) * c - sa),
            (a + 1.0) + (a - 1.0) * c + sa,
            -2.0 * ((a - 1.0) + (a + 1.0) * c),
            (a + 1.0) + (a - 1.0) * c - sa,
        );
    }

    /// Shelf slope S = 1.
    pub fn set_high_shelf(&mut self, fs: f64, f: f64, gain_db: f64) {
        let a = 10f64.powf(gain_db / 40.0);
        let w = w0(fs, f);
        let (s, c) = w.sin_cos();
        let alpha = s / 2.0 * 2f64.sqrt();
        let sa = 2.0 * a.sqrt() * alpha;
        self.set_raw(
            a * ((a + 1.0) + (a - 1.0) * c + sa),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
            a * ((a + 1.0) + (a - 1.0) * c - sa),
            (a + 1.0) - (a - 1.0) * c + sa,
            2.0 * ((a - 1.0) - (a + 1.0) * c),
            (a + 1.0) - (a - 1.0) * c - sa,
        );
    }

    #[inline]
    pub fn process(&mut self, x: f32) -> f32 {
        let x = x as f64;
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        // Flush denormals (silence after a signal decays into the subnormal range).
        if self.z1.abs() < 1e-30 {
            self.z1 = 0.0;
        }
        if self.z2.abs() < 1e-30 {
            self.z2 = 0.0;
        }
        y as f32
    }

    pub fn reset(&mut self) {
        self.z1 = 0.0;
        self.z2 = 0.0;
    }

    /// Magnitude response in dB at `f` (for tests and docs).
    pub fn magnitude_db(&self, fs: f64, f: f64) -> f64 {
        let w = 2.0 * PI * f / fs;
        let (re1, im1) = (w.cos(), -w.sin());
        let (re2, im2) = ((2.0 * w).cos(), -(2.0 * w).sin());
        let nr = self.b0 + self.b1 * re1 + self.b2 * re2;
        let ni = self.b1 * im1 + self.b2 * im2;
        let dr = 1.0 + self.a1 * re1 + self.a2 * re2;
        let di = self.a1 * im1 + self.a2 * im2;
        10.0 * ((nr * nr + ni * ni) / (dr * dr + di * di)).log10()
    }
}

/// One-pole smoothing coefficient for a time constant in ms (per-sample).
#[inline]
pub fn tc_coef(ms: f32, fs: f32) -> f32 {
    if ms <= 0.0 {
        0.0
    } else {
        (-1.0 / (ms * 0.001 * fs)).exp()
    }
}

#[inline]
pub fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

#[inline]
pub fn lin_to_db(x: f32) -> f32 {
    20.0 * x.max(1e-12).log10()
}

/// RMS of a block in dBFS (floored at −120).
pub fn rms_db(block: &[f32]) -> f32 {
    if block.is_empty() {
        return -120.0;
    }
    let ms = block.iter().map(|x| (*x as f64) * (*x as f64)).sum::<f64>() / block.len() as f64;
    (10.0 * ms.max(1e-12).log10()) as f32
}
