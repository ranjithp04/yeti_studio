//! Clock-drift controller for the bridge between two independent audio clocks
//! (Yeti USB clock → virtual mic / monitor clock, system output → AEC reference).
//!
//! The controlled variable is the **fill level of the SPSC ring** between the two clock
//! domains. A PI controller turns the (smoothed) fill error into a resample-ratio correction
//! `r` that is applied on top of the nominal rate ratio:
//!
//! - fill above target → the producer runs fast relative to the consumer → `r < 1`
//!   (produce fewer frames / consume input faster),
//! - fill below target → `r > 1`.
//!
//! Pure arithmetic, no allocation: safe to call on the DSP thread every block. Unit tests
//! simulate two clocks with a known ppm offset and jittery callback sizes.

/// Tuning for [`DriftController`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DriftConfig {
    /// Desired ring fill, frames.
    pub target_fill: f64,
    /// Proportional gain: ratio correction per frame of (smoothed) fill error.
    pub kp: f64,
    /// Integral gain: ratio correction per frame·second of fill error.
    pub ki: f64,
    /// Time constant of the fill-level low-pass, seconds (removes callback-granularity jitter).
    pub smoothing_s: f64,
    /// Maximum |r − 1| (e.g. 0.005 = ±5000 ppm).
    pub max_correction: f64,
    /// The integral (which learns the steady clock offset) only accumulates while
    /// |fill error| is below this many frames; large transients (startup bursts, stalls)
    /// are handled by the P term alone, so they cannot wind the integral up.
    pub integral_band: f64,
    /// Bound on the integral term (real clock offsets are a few hundred ppm).
    pub max_integral: f64,
}

impl DriftConfig {
    /// Defaults suitable for 48 kHz rings with 5–25 ms target fill. Corrects USB-class
    /// clock offsets (≤ ±1000 ppm) within a few seconds without audible pitch modulation.
    pub fn for_target(target_fill: f64) -> Self {
        Self {
            target_fill,
            kp: 2.0e-6,
            ki: 4.0e-7,
            smoothing_s: 0.5,
            max_correction: 0.005,
            integral_band: (target_fill * 0.25).max(240.0),
            max_integral: 0.002,
        }
    }
}

/// PI controller from ring fill level to resample-ratio correction.
#[derive(Debug, Clone)]
pub struct DriftController {
    cfg: DriftConfig,
    smoothed: Option<f64>,
    integral: f64,
    ratio: f64,
}

impl DriftController {
    pub fn new(cfg: DriftConfig) -> Self {
        Self {
            cfg,
            smoothed: None,
            integral: 0.0,
            ratio: 1.0,
        }
    }

    pub fn config(&self) -> &DriftConfig {
        &self.cfg
    }

    /// Change the target (e.g. after the stream buffer size is known). Keeps the integral.
    pub fn set_target(&mut self, target_fill: f64) {
        self.cfg.target_fill = target_fill;
    }

    /// Forget history (after a reconnect / ring reset).
    pub fn reset(&mut self) {
        self.smoothed = None;
        self.integral = 0.0;
        self.ratio = 1.0;
    }

    /// Feed one fill measurement taken `dt_s` seconds after the previous one; returns the
    /// new ratio correction (also available via [`ratio`](Self::ratio)).
    pub fn update(&mut self, fill_frames: f64, dt_s: f64) -> f64 {
        let dt = dt_s.max(0.0);
        let s = match self.smoothed {
            None => fill_frames,
            Some(prev) => {
                let a = if self.cfg.smoothing_s > 0.0 {
                    1.0 - (-dt / self.cfg.smoothing_s).exp()
                } else {
                    1.0
                };
                prev + a * (fill_frames - prev)
            }
        };
        self.smoothed = Some(s);
        let err = s - self.cfg.target_fill;
        // Conditional integration (anti-windup): only near the target.
        if err.abs() < self.cfg.integral_band {
            let mi = self.cfg.max_integral;
            self.integral = (self.integral + self.cfg.ki * err * dt).clamp(-mi, mi);
        }
        let max = self.cfg.max_correction;
        let corr = -(self.cfg.kp * err + self.integral);
        self.ratio = 1.0 + corr.clamp(-max, max);
        self.ratio
    }

    /// The ring was re-centred externally (frames dropped / silence inserted): restart the
    /// fill low-pass from `fill_frames`, keep the learned clock offset (integral).
    pub fn resynced(&mut self, fill_frames: f64) {
        self.smoothed = Some(fill_frames);
    }

    /// Current ratio correction (1.0 = no correction).
    pub fn ratio(&self) -> f64 {
        self.ratio
    }

    /// Smoothed fill level, frames (`None` before the first update).
    pub fn smoothed_fill(&self) -> Option<f64> {
        self.smoothed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulated bridge: a producer at `prod_hz` pushes blocks of `prod_block` frames,
    /// resampled by `r`; a consumer at `cons_hz` pulls callbacks of jittering size.
    /// Time advances in 1 ms ticks. Returns (mean ratio, mean smoothed fill, underruns,
    /// max |smoothed fill − target|), the means/max taken over the second (settled) half.
    fn simulate(prod_ppm: f64, seconds: f64, target: f64) -> (f64, f64, u64, f64) {
        let nominal = 48_000.0;
        let prod_hz = nominal * (1.0 + prod_ppm * 1e-6);
        let cons_hz = nominal;
        let mut ctl = DriftController::new(DriftConfig::for_target(target));
        let mut fill: f64 = target; // prefilled
        let (mut prod_acc, mut cons_acc) = (0.0f64, 0.0f64);
        let dt = 0.001;
        let mut t = 0.0;
        let mut underruns = 0u64;
        let mut max_dev: f64 = 0.0;
        let mut sum_fill = 0.0;
        let mut n_fill = 0u64;
        let mut sum_ratio = 0.0;
        let mut cb_i = 0usize;
        let cb_sizes = [512.0, 512.0, 470.0, 554.0, 512.0];
        while t < seconds {
            t += dt;
            prod_acc += prod_hz * dt;
            // producer delivers 480-frame blocks (DSP block), each resampled by r.
            while prod_acc >= 480.0 {
                prod_acc -= 480.0;
                fill += 480.0 * ctl.ratio();
                ctl.update(fill, 0.01);
            }
            cons_acc += cons_hz * dt;
            let need = cb_sizes[cb_i % cb_sizes.len()];
            if cons_acc >= need {
                cons_acc -= need;
                cb_i += 1;
                if fill < need {
                    underruns += 1;
                    fill = 0.0;
                } else {
                    fill -= need;
                }
            }
            if t > seconds * 0.5 {
                let s = ctl.smoothed_fill().unwrap();
                max_dev = max_dev.max((s - target).abs());
                sum_fill += s;
                sum_ratio += ctl.ratio();
                n_fill += 1;
            }
        }
        (
            sum_ratio / n_fill as f64,
            sum_fill / n_fill as f64,
            underruns,
            max_dev,
        )
    }

    #[test]
    fn converges_with_fast_producer() {
        // Producer clock +300 ppm → controller must slow it down by ~300 ppm.
        let (r, mean_fill, underruns, max_dev) = simulate(300.0, 120.0, 1500.0);
        assert!((r - (1.0 - 300e-6)).abs() < 20e-6, "ratio {r}");
        assert!((mean_fill - 1500.0).abs() < 50.0, "mean fill {mean_fill}");
        assert!(max_dev < 200.0, "max deviation {max_dev}");
        assert_eq!(underruns, 0);
    }

    #[test]
    fn converges_with_slow_producer() {
        let (r, mean_fill, underruns, _) = simulate(-500.0, 120.0, 1500.0);
        assert!((r - (1.0 + 500e-6)).abs() < 20e-6, "ratio {r}");
        assert!((mean_fill - 1500.0).abs() < 50.0, "mean fill {mean_fill}");
        assert_eq!(underruns, 0);
    }

    #[test]
    fn no_drift_stays_at_unity() {
        let (r, mean_fill, underruns, _) = simulate(0.0, 60.0, 1500.0);
        assert!((r - 1.0).abs() < 20e-6, "ratio {r}");
        assert!((mean_fill - 1500.0).abs() < 50.0);
        assert_eq!(underruns, 0);
    }

    #[test]
    fn uncorrected_drift_would_underrun() {
        // Sanity check of the simulation itself: with the controller frozen at 1.0 a
        // −500 ppm producer drains the 1500-frame cushion in ~60 s and underruns.
        let target = 1500.0;
        let prod_hz = 48_000.0 * (1.0 - 500e-6);
        let mut fill = target;
        let (mut p, mut c) = (0.0, 0.0);
        let mut under = 0;
        for _ in 0..120_000 {
            p += prod_hz * 0.001;
            while p >= 480.0 {
                p -= 480.0;
                fill += 480.0;
            }
            c += 48.0;
            if c >= 512.0 {
                c -= 512.0;
                if fill < 512.0 {
                    under += 1;
                    fill = 0.0;
                } else {
                    fill -= 512.0;
                }
            }
        }
        assert!(under > 0);
    }

    #[test]
    fn large_initial_error_does_not_wind_up() {
        // Start 900 frames above target (inside the resync threshold) with no clock drift:
        // the P term drains it and the integral must not overshoot into underruns.
        let target = 1344.0;
        let mut ctl = DriftController::new(DriftConfig::for_target(target));
        let mut fill: f64 = target + 900.0;
        let (mut p, mut c) = (0.0, 0.0);
        let mut under = 0;
        let mut min_fill = f64::MAX;
        for ms in 0..120_000 {
            p += 48.0;
            while p >= 480.0 {
                p -= 480.0;
                fill += 480.0 * ctl.ratio();
                ctl.update(fill, 0.01);
            }
            c += 48.0;
            if c >= 512.0 {
                c -= 512.0;
                if fill < 512.0 {
                    under += 1;
                    fill = 0.0;
                } else {
                    fill -= 512.0;
                }
            }
            if ms > 30_000 {
                min_fill = min_fill.min(ctl.smoothed_fill().unwrap());
            }
        }
        assert_eq!(under, 0);
        assert!(
            (ctl.smoothed_fill().unwrap() - target).abs() < 60.0,
            "{:?}",
            ctl.smoothed_fill()
        );
        assert!(min_fill > target - 200.0, "overshoot to {min_fill}");
        assert!((ctl.ratio() - 1.0).abs() < 50e-6, "ratio {}", ctl.ratio());
    }

    #[test]
    fn correction_is_clamped() {
        let mut c = DriftController::new(DriftConfig::for_target(1000.0));
        for _ in 0..10_000 {
            c.update(100_000.0, 0.01);
        }
        assert!((c.ratio() - (1.0 - 0.005)).abs() < 1e-12);
        // Recovers promptly once the error flips (no integral wind-up beyond the clamp).
        for _ in 0..3_000 {
            c.update(0.0, 0.01);
        }
        assert!(c.ratio() > 1.0);
    }
}
