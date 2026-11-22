//! Lock-free telemetry shared between the DSP thread (writer) and the UI (reader):
//! meters, raw-vs-cleaned waveform ring, counters and stats.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering::Relaxed};
use yeti_types::{EngineCounters, EngineStats, METER_FLOOR_DB, MeterSnapshot, WaveformSnapshot};

/// Waveform history length (buckets) and bucket size (DSP blocks of 10 ms).
pub const WAVEFORM_BUCKETS: usize = 200;
pub const BLOCKS_PER_BUCKET: usize = 2;
pub const BUCKET_MS: f32 = 10.0 * BLOCKS_PER_BUCKET as f32;

/// `f32` stored in an `AtomicU32`.
#[derive(Default)]
pub struct AtomicF32(AtomicU32);

impl AtomicF32 {
    pub fn new(v: f32) -> Self {
        Self(AtomicU32::new(v.to_bits()))
    }
    pub fn get(&self) -> f32 {
        f32::from_bits(self.0.load(Relaxed))
    }
    pub fn set(&self, v: f32) {
        self.0.store(v.to_bits(), Relaxed)
    }
}

pub fn to_db(x: f32) -> f32 {
    if x <= 0.0 || !x.is_finite() {
        METER_FLOOR_DB
    } else {
        (20.0 * x.log10()).max(METER_FLOOR_DB)
    }
}

/// Peak and RMS (linear) of a block.
pub fn peak_rms(x: &[f32]) -> (f32, f32) {
    if x.is_empty() {
        return (0.0, 0.0);
    }
    let mut peak = 0.0f32;
    let mut sum = 0.0f64;
    for &s in x {
        peak = peak.max(s.abs());
        sum += (s as f64) * (s as f64);
    }
    (peak, (sum / x.len() as f64).sqrt() as f32)
}

pub struct Telemetry {
    pub in_peak: AtomicF32,
    pub in_rms: AtomicF32,
    pub out_peak: AtomicF32,
    pub out_rms: AtomicF32,
    pub noise_reduction_db: AtomicF32,
    pub gain_change_db: AtomicF32,
    pub gate_open: AtomicBool,
    pub voice_probability: AtomicF32,

    wave_raw: [AtomicF32; WAVEFORM_BUCKETS],
    wave_clean: [AtomicF32; WAVEFORM_BUCKETS],
    /// Total buckets written (index of next = head % N).
    wave_head: AtomicUsize,

    pub stream_xruns: AtomicU64,
    pub capture_overruns: AtomicU64,
    pub output_underruns: AtomicU64,
    pub reference_underruns: AtomicU64,
    pub blocks_processed: AtomicU64,
    /// Output ring full (DSP produced faster than the device consumed; frames dropped).
    pub output_overruns: AtomicU64,
    /// Output ring hard re-centred (startup burst, stall, or drained by the consumer).
    pub output_resyncs: AtomicU64,

    pub latency_ms: AtomicF32,
    pub dsp_latency_ms: AtomicF32,
    pub cpu_percent: AtomicF32,
    pub drift_ratio: AtomicF32,
    pub monitor_drift_ratio: AtomicF32,
    pub reference_drift_ratio: AtomicF32,
    /// Smoothed fills (frames) for diagnostics.
    pub capture_fill: AtomicF32,
    pub output_fill: AtomicF32,
    pub reference_fill: AtomicF32,
    /// Max |sample| seen on the raw capture since start (non-silence evidence).
    pub max_input_peak: AtomicF32,
    /// Reference (loopback) RMS of the last block, linear.
    pub reference_rms: AtomicF32,
    pub reference_max_peak: AtomicF32,
}

impl Default for Telemetry {
    fn default() -> Self {
        Self {
            in_peak: AtomicF32::new(0.0),
            in_rms: AtomicF32::new(0.0),
            out_peak: AtomicF32::new(0.0),
            out_rms: AtomicF32::new(0.0),
            noise_reduction_db: AtomicF32::new(0.0),
            gain_change_db: AtomicF32::new(0.0),
            gate_open: AtomicBool::new(false),
            voice_probability: AtomicF32::new(0.0),
            wave_raw: std::array::from_fn(|_| AtomicF32::new(0.0)),
            wave_clean: std::array::from_fn(|_| AtomicF32::new(0.0)),
            wave_head: AtomicUsize::new(0),
            stream_xruns: AtomicU64::new(0),
            capture_overruns: AtomicU64::new(0),
            output_underruns: AtomicU64::new(0),
            reference_underruns: AtomicU64::new(0),
            blocks_processed: AtomicU64::new(0),
            output_overruns: AtomicU64::new(0),
            output_resyncs: AtomicU64::new(0),
            latency_ms: AtomicF32::new(0.0),
            dsp_latency_ms: AtomicF32::new(0.0),
            cpu_percent: AtomicF32::new(0.0),
            drift_ratio: AtomicF32::new(1.0),
            monitor_drift_ratio: AtomicF32::new(1.0),
            reference_drift_ratio: AtomicF32::new(1.0),
            capture_fill: AtomicF32::new(0.0),
            output_fill: AtomicF32::new(0.0),
            reference_fill: AtomicF32::new(0.0),
            max_input_peak: AtomicF32::new(0.0),
            reference_rms: AtomicF32::new(0.0),
            reference_max_peak: AtomicF32::new(0.0),
        }
    }
}

impl Telemetry {
    /// Zero everything (new engine run).
    pub fn reset(&self) {
        for a in [
            &self.in_peak,
            &self.in_rms,
            &self.out_peak,
            &self.out_rms,
            &self.noise_reduction_db,
            &self.gain_change_db,
            &self.voice_probability,
            &self.latency_ms,
            &self.dsp_latency_ms,
            &self.cpu_percent,
            &self.capture_fill,
            &self.output_fill,
            &self.reference_fill,
            &self.max_input_peak,
            &self.reference_rms,
            &self.reference_max_peak,
        ] {
            a.set(0.0);
        }
        for a in [
            &self.drift_ratio,
            &self.monitor_drift_ratio,
            &self.reference_drift_ratio,
        ] {
            a.set(1.0);
        }
        for c in [
            &self.stream_xruns,
            &self.capture_overruns,
            &self.output_underruns,
            &self.reference_underruns,
            &self.blocks_processed,
            &self.output_overruns,
            &self.output_resyncs,
        ] {
            c.store(0, Relaxed);
        }
        for (r, c) in self.wave_raw.iter().zip(&self.wave_clean) {
            r.set(0.0);
            c.set(0.0);
        }
        self.wave_head.store(0, Relaxed);
        self.gate_open.store(false, Relaxed);
    }

    /// Writer side: append one waveform bucket.
    pub fn push_wave(&self, raw: f32, clean: f32) {
        let h = self.wave_head.load(Relaxed);
        self.wave_raw[h % WAVEFORM_BUCKETS].set(raw);
        self.wave_clean[h % WAVEFORM_BUCKETS].set(clean);
        self.wave_head
            .store(h + 1, std::sync::atomic::Ordering::Release);
    }

    pub fn meters(&self) -> MeterSnapshot {
        let ri = to_db(self.in_rms.get());
        let ro = to_db(self.out_rms.get());
        MeterSnapshot {
            input_peak_db: to_db(self.in_peak.get()),
            input_rms_db: ri,
            output_peak_db: to_db(self.out_peak.get()),
            output_rms_db: ro,
            noise_reduction_db: self.noise_reduction_db.get().max(0.0),
            gain_change_db: self.gain_change_db.get(),
            gate_open: self.gate_open.load(Relaxed),
            voice_probability: self.voice_probability.get(),
        }
    }

    /// Reader side: oldest-first copy into `out` (reuses its allocation).
    pub fn waveform(&self, out: &mut WaveformSnapshot) {
        out.raw.clear();
        out.cleaned.clear();
        out.bucket_ms = BUCKET_MS;
        let head = self.wave_head.load(std::sync::atomic::Ordering::Acquire);
        let n = head.min(WAVEFORM_BUCKETS);
        for k in (head - n)..head {
            out.raw.push(self.wave_raw[k % WAVEFORM_BUCKETS].get());
            out.cleaned
                .push(self.wave_clean[k % WAVEFORM_BUCKETS].get());
        }
    }

    pub fn counters(&self) -> EngineCounters {
        EngineCounters {
            stream_xruns: self.stream_xruns.load(Relaxed),
            capture_overruns: self.capture_overruns.load(Relaxed),
            output_underruns: self.output_underruns.load(Relaxed),
            reference_underruns: self.reference_underruns.load(Relaxed),
            blocks_processed: self.blocks_processed.load(Relaxed),
        }
    }

    pub fn stats(&self) -> EngineStats {
        EngineStats {
            latency_ms: self.latency_ms.get(),
            dsp_latency_ms: self.dsp_latency_ms.get(),
            cpu_percent: self.cpu_percent.get(),
            drift_ratio: self.drift_ratio.get() as f64,
            counters: self.counters(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waveform_ring_is_oldest_first_and_bounded() {
        let t = Telemetry::default();
        let mut w = WaveformSnapshot::default();
        t.waveform(&mut w);
        assert!(w.raw.is_empty());
        for i in 0..(WAVEFORM_BUCKETS + 5) {
            t.push_wave(i as f32, i as f32 * 0.5);
        }
        t.waveform(&mut w);
        assert_eq!(w.raw.len(), WAVEFORM_BUCKETS);
        assert_eq!(w.raw[0], 5.0);
        assert_eq!(*w.raw.last().unwrap(), (WAVEFORM_BUCKETS + 4) as f32);
        assert_eq!(w.cleaned[0], 2.5);
    }

    #[test]
    fn peak_rms_sine() {
        let x: Vec<f32> = (0..4800).map(|i| (i as f32 * 0.1).sin() * 0.5).collect();
        let (p, r) = peak_rms(&x);
        assert!((p - 0.5).abs() < 1e-3);
        assert!((r - 0.5 / 2f32.sqrt()).abs() < 5e-3);
        assert_eq!(to_db(0.0), METER_FLOOR_DB);
    }
}
