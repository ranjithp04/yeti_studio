//! Live end-to-end latency probe (`yeti-passthrough --probe`).
//!
//! While enabled, the main capture callback replaces the microphone signal with silence plus
//! a single full-scale impulse every [`PROBE_PERIOD_S`] and records the impulse's capture
//! time (`InputStreamTimestamp::capture` + frame offset). The DSP worker still runs the voice
//! chain (so its CPU time is part of the path) but forwards the raw block, because the noise
//! suppressor/gate would remove an impulse. The main output callback finds the impulse in the
//! data it is about to play and records `OutputStreamTimestamp::playback` + frame offset,
//! then writes **silence** to the device (nothing audible).
//!
//! Both timestamps are on the same host clock (CoreAudio `mHostTime` on macOS) and include the
//! HAL-reported device buffer, safety offset and device latency, so
//! `playback − capture` = rings + resamplers + DSP scheduling + device buffers, measured.
//! The voice chain's algorithmic delay (DFN 30 ms, RNNoise 10 ms, AEC 9 ms, limiter 1 ms;
//! cross-correlation-verified offline, see docs/DSP_RESULTS.md) is added on top by the caller.
//!
//! All state is atomics: safe in cpal callbacks.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::Relaxed};

/// Seconds between impulses.
pub const PROBE_PERIOD_S: f64 = 0.25;
/// Detection threshold (the input/output resamplers smear the impulse a little).
pub const PROBE_THRESHOLD: f32 = 0.25;
/// An impulse not seen at the output within this time is counted as lost.
const LOST_AFTER_NS: u64 = 1_000_000_000;

#[derive(Default)]
pub struct LatencyProbe {
    pub enabled: AtomicBool,
    /// An impulse is in flight (captured, not yet seen at the output).
    pending: AtomicBool,
    /// Capture time of the in-flight impulse, host-clock nanoseconds.
    cap_ns: AtomicU64,
    count: AtomicU64,
    sum_ns: AtomicU64,
    min_ns: AtomicU64,
    max_ns: AtomicU64,
    lost: AtomicU64,
}

/// Summary of the measurements so far.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ProbeStats {
    pub count: u64,
    pub lost: u64,
    pub mean_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
}

impl LatencyProbe {
    pub fn reset(&self) {
        self.pending.store(false, Relaxed);
        for a in [&self.count, &self.sum_ns, &self.max_ns, &self.lost] {
            a.store(0, Relaxed);
        }
        self.min_ns.store(u64::MAX, Relaxed);
    }

    /// Capture side: may an impulse be injected at host time `now_ns`? (None in flight, or
    /// the one in flight is lost.)
    pub fn can_fire(&self, now_ns: u64) -> bool {
        if !self.pending.load(Relaxed) {
            return true;
        }
        if now_ns.saturating_sub(self.cap_ns.load(Relaxed)) > LOST_AFTER_NS {
            self.lost.fetch_add(1, Relaxed);
            self.pending.store(false, Relaxed);
            return true;
        }
        false
    }

    /// Capture side: an impulse was written; its capture time is `t_ns`.
    pub fn fired(&self, t_ns: u64) {
        self.cap_ns.store(t_ns, Relaxed);
        self.pending.store(true, Relaxed);
    }

    /// Output side: an impulse will be played at host time `t_ns`.
    pub fn arrived(&self, t_ns: u64) {
        if !self.pending.swap(false, Relaxed) {
            return;
        }
        let d = t_ns.saturating_sub(self.cap_ns.load(Relaxed));
        self.count.fetch_add(1, Relaxed);
        self.sum_ns.fetch_add(d, Relaxed);
        self.min_ns.fetch_min(d, Relaxed);
        self.max_ns.fetch_max(d, Relaxed);
    }

    pub fn stats(&self) -> ProbeStats {
        let count = self.count.load(Relaxed);
        let ms = |ns: u64| ns as f64 / 1e6;
        if count == 0 {
            return ProbeStats {
                lost: self.lost.load(Relaxed),
                ..Default::default()
            };
        }
        ProbeStats {
            count,
            lost: self.lost.load(Relaxed),
            mean_ms: ms(self.sum_ns.load(Relaxed)) / count as f64,
            min_ms: ms(self.min_ns.load(Relaxed)),
            max_ms: ms(self.max_ns.load(Relaxed)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_round_trips_and_losses() {
        let p = LatencyProbe::default();
        p.reset();
        assert!(p.can_fire(0));
        p.fired(1_000_000);
        assert!(!p.can_fire(2_000_000));
        p.arrived(21_000_000); // 20 ms
        p.arrived(30_000_000); // no impulse in flight: ignored
        assert!(p.can_fire(40_000_000));
        p.fired(40_000_000);
        p.arrived(70_000_000); // 30 ms
        p.fired(100_000_000);
        assert!(p.can_fire(100_000_000 + 2 * LOST_AFTER_NS)); // lost
        let s = p.stats();
        assert_eq!((s.count, s.lost), (2, 1));
        assert!((s.mean_ms - 25.0).abs() < 1e-9);
        assert!((s.min_ms - 20.0).abs() < 1e-9 && (s.max_ms - 30.0).abs() < 1e-9);
    }
}
