//! Pure-Rust chain stages: rumble filter, voice gate, EQ + de-esser, auto-level, limiter.
//!
//! All state is fixed-size; `process` never allocates or locks.

use crate::Processor;
use crate::biquad::{Biquad, db_to_lin, lin_to_db, tc_coef};
use yeti_types::{AutoLevelParams, EqParams, GateParams, HighPassParams, LimiterParams, Result};

// ---------------------------------------------------------------------------
// Rumble filter
// ---------------------------------------------------------------------------

/// 4th-order Butterworth high-pass (two cascaded biquads, 24 dB/oct).
/// At the default 80 Hz cut-off: −3 dB @ 80 Hz, ≈ −34 dB @ 30 Hz.
pub struct HighPass {
    params: HighPassParams,
    fs: f64,
    stages: [Biquad; 2],
}

/// Q of the two sections of a 4th-order Butterworth.
const BW4_Q: [f64; 2] = [0.541_196_1, 1.306_563];

impl HighPass {
    pub fn new(params: HighPassParams, fs: u32) -> Self {
        let mut h = Self {
            params,
            fs: fs as f64,
            stages: [Biquad::default(); 2],
        };
        h.recompute();
        h
    }
    fn recompute(&mut self) {
        let f = self.params.cutoff_hz.clamp(20.0, 400.0) as f64;
        for (s, q) in self.stages.iter_mut().zip(BW4_Q) {
            s.set_highpass(self.fs, f, q);
        }
    }
    pub fn set_params(&mut self, p: &HighPassParams) {
        if p.cutoff_hz != self.params.cutoff_hz {
            self.params = *p;
            self.recompute();
        } else if p.enabled != self.params.enabled {
            self.params = *p;
            self.stages.iter_mut().for_each(Biquad::reset);
        }
    }
    /// Total magnitude response in dB (tests/docs).
    pub fn magnitude_db(&self, f: f64) -> f64 {
        self.stages.iter().map(|s| s.magnitude_db(self.fs, f)).sum()
    }
}

impl Processor for HighPass {
    fn name(&self) -> &'static str {
        "hpf"
    }
    fn prepare(&mut self, sample_rate: u32, _max: usize) -> Result<()> {
        self.fs = sample_rate as f64;
        self.recompute();
        Ok(())
    }
    fn process(&mut self, block: &mut [f32]) {
        if !self.params.enabled {
            return;
        }
        for x in block.iter_mut() {
            let mut y = *x;
            for s in self.stages.iter_mut() {
                y = s.process(y);
            }
            *x = y;
        }
    }
    fn reset(&mut self) {
        self.stages.iter_mut().for_each(Biquad::reset);
    }
}

// ---------------------------------------------------------------------------
// Voice gate
// ---------------------------------------------------------------------------

/// Downward gate keyed on the (post-noise-suppression) level **and** the NS model's voice
/// probability:
/// - with a VAD (`vad_valid`): opens while the envelope exceeds `threshold_db` *and* the
///   2-frame-smoothed voice probability exceeded 0.8 within the last 250 ms (hang-over), so
///   residual keyboard clicks above the threshold stay gated; while the smoothed VAD is above
///   0.8 it also opens down to 12 dB below the threshold (quiet word endings);
/// - without a VAD (NS off): level only.
///
/// Closes after a 60 ms hold with a 4 dB hysteresis. Gain is smoothed in dB with
/// `attack_ms` (opening) / `release_ms` (closing). The NS models report VAD for the frame
/// they are analysing, which is ≥ 1 block ahead of their (delayed) output: free look-ahead.
pub struct Gate {
    params: GateParams,
    fs: f32,
    env: f32,
    env_att: f32,
    env_rel: f32,
    gain_db: f32,
    g_att: f32,
    g_rel: f32,
    floor_db: f32,
    open: bool,
    hold_left: u32,
    hold: u32,
    vad_hang_left: u32,
    vad_hang: u32,
    vad_smooth: f32,
    /// Voice probability of the current block (set by the chain before `process`).
    pub voice_probability: f32,
    /// Whether `voice_probability` comes from a model (false → level-only gate).
    pub vad_valid: bool,
}

const GATE_HOLD_MS: f32 = 60.0;
const GATE_HYST_DB: f32 = 4.0;
const GATE_VAD_HANG_MS: f32 = 250.0;
const GATE_VAD_TRIGGER: f32 = 0.8;

impl Gate {
    pub fn new(params: GateParams, fs: u32) -> Self {
        let mut g = Self {
            params,
            fs: fs as f32,
            env: 0.0,
            env_att: 0.0,
            env_rel: 0.0,
            gain_db: 0.0,
            g_att: 0.0,
            g_rel: 0.0,
            floor_db: 0.0,
            open: false,
            hold_left: 0,
            hold: 0,
            vad_hang_left: 0,
            vad_hang: 0,
            vad_smooth: 0.0,
            voice_probability: 0.0,
            vad_valid: false,
        };
        g.recompute();
        g.gain_db = g.floor_db;
        g
    }
    fn recompute(&mut self) {
        let fs = self.fs;
        self.env_att = tc_coef(1.0, fs);
        self.env_rel = tc_coef(40.0, fs);
        self.g_att = tc_coef(self.params.attack_ms.max(0.1), fs);
        self.g_rel = tc_coef(self.params.release_ms.max(1.0), fs);
        self.floor_db = -self.params.range_db.clamp(0.0, 90.0);
        self.hold = (GATE_HOLD_MS * 0.001 * fs) as u32;
        self.vad_hang = (GATE_VAD_HANG_MS * 0.001 * fs) as u32;
    }
    pub fn set_params(&mut self, p: &GateParams) {
        if *p != self.params {
            self.params = *p;
            self.recompute();
        }
    }
    pub fn is_open(&self) -> bool {
        self.open || !self.params.enabled
    }
}

impl Processor for Gate {
    fn name(&self) -> &'static str {
        "gate"
    }
    fn prepare(&mut self, sample_rate: u32, _max: usize) -> Result<()> {
        self.fs = sample_rate as f32;
        self.recompute();
        Ok(())
    }
    fn process(&mut self, block: &mut [f32]) {
        if !self.params.enabled {
            return;
        }
        let thr = self.params.threshold_db;
        // Sustained voice: short keyboard bursts make RNNoise's VAD spike to ~0.6–0.8 for a
        // frame or two; speech holds it near 1.0.
        self.vad_smooth = 0.5 * self.vad_smooth + 0.5 * self.voice_probability;
        if self.vad_smooth > GATE_VAD_TRIGGER {
            self.vad_hang_left = self.vad_hang;
        } else {
            self.vad_hang_left = self.vad_hang_left.saturating_sub(block.len() as u32);
        }
        let vad_ok = !self.vad_valid || self.vad_hang_left > 0;
        let voiced = self.vad_valid && self.vad_smooth > GATE_VAD_TRIGGER;
        for x in block.iter_mut() {
            let a = x.abs();
            let c = if a > self.env {
                self.env_att
            } else {
                self.env_rel
            };
            self.env = a + c * (self.env - a);
            let env_db = lin_to_db(self.env);
            let want_open = (env_db > thr && vad_ok) || (voiced && env_db > thr - 12.0);
            if want_open {
                self.open = true;
                self.hold_left = self.hold;
            } else if self.open {
                let below =
                    (env_db < thr - GATE_HYST_DB || !vad_ok) && !(voiced && env_db > thr - 12.0);
                if below {
                    if self.hold_left == 0 {
                        self.open = false;
                    } else {
                        self.hold_left -= 1;
                    }
                }
            }
            // Smooth in dB (expander-style): even release towards the floor.
            let target = if self.open { 0.0 } else { self.floor_db };
            let c = if target > self.gain_db {
                self.g_att
            } else {
                self.g_rel
            };
            self.gain_db = target + c * (self.gain_db - target);
            *x *= db_to_lin(self.gain_db);
        }
    }
    fn reset(&mut self) {
        self.env = 0.0;
        self.gain_db = self.floor_db;
        self.open = false;
        self.hold_left = 0;
        self.vad_hang_left = 0;
        self.vad_smooth = 0.0;
    }
}

// ---------------------------------------------------------------------------
// Voice EQ + de-esser
// ---------------------------------------------------------------------------

/// Samples between de-esser shelf coefficient updates.
const DEESS_UPDATE: usize = 16;

/// Low shelf 200 Hz ("warmth"), presence peak 3 kHz (Q 0.9), high shelf 10 kHz ("air"),
/// then a dynamic-EQ de-esser: a 5.5 kHz 2nd-order high-pass side-chain is compared with the
/// full-band envelope; while sibilant energy comes within `−8 + 4·(1−amount)` dB of the
/// full band, a 4.5 kHz high shelf is lowered by up to `12·amount` dB (coefficients updated
/// every 16 samples, gain smoothed 1 ms attack / 50 ms release in dB).
pub struct VoiceEq {
    params: EqParams,
    fs: f64,
    low: Biquad,
    presence: Biquad,
    air: Biquad,
    detect: Biquad,
    shelf: Biquad,
    env_full: f32,
    env_hi: f32,
    env_att: f32,
    env_rel: f32,
    red_db: f32,
    applied_db: f32,
    ds_att: f32,
    ds_rel: f32,
    counter: usize,
    /// Largest de-esser reduction in the last block, dB (≤ 0), for tests/meters.
    pub deess_db: f32,
}

impl VoiceEq {
    pub fn new(params: EqParams, fs: u32) -> Self {
        let f = fs as f32;
        let mut e = Self {
            params,
            fs: fs as f64,
            low: Biquad::default(),
            presence: Biquad::default(),
            air: Biquad::default(),
            detect: Biquad::default(),
            shelf: Biquad::default(),
            env_full: 0.0,
            env_hi: 0.0,
            env_att: tc_coef(1.0, f),
            env_rel: tc_coef(30.0, f),
            red_db: 0.0,
            applied_db: f32::NAN,
            ds_att: tc_coef(1.0, f),
            ds_rel: tc_coef(50.0, f),
            counter: 0,
            deess_db: 0.0,
        };
        e.recompute();
        e
    }
    fn recompute(&mut self) {
        let p = &self.params;
        let fs = self.fs;
        self.low
            .set_low_shelf(fs, 200.0, p.low_gain_db.clamp(-12.0, 12.0) as f64);
        self.presence.set_peaking(
            fs,
            3000.0,
            0.9,
            p.presence_gain_db.clamp(-12.0, 12.0) as f64,
        );
        self.air
            .set_high_shelf(fs, 10_000.0, p.air_gain_db.clamp(-12.0, 12.0) as f64);
        self.detect
            .set_highpass(fs, 5500.0, std::f64::consts::FRAC_1_SQRT_2);
        self.applied_db = f32::NAN;
    }
    pub fn set_params(&mut self, p: &EqParams) {
        if *p != self.params {
            self.params = *p;
            self.recompute();
        }
    }
}

impl Processor for VoiceEq {
    fn name(&self) -> &'static str {
        "eq"
    }
    fn prepare(&mut self, sample_rate: u32, _max: usize) -> Result<()> {
        self.fs = sample_rate as f64;
        self.recompute();
        Ok(())
    }
    fn process(&mut self, block: &mut [f32]) {
        if !self.params.enabled {
            return;
        }
        let amount = self.params.deesser_amount.clamp(0.0, 1.0);
        let deess = self.params.deesser_enabled && amount > 0.0;
        let thr_db = -8.0 + 4.0 * (1.0 - amount);
        let max_red_db = 12.0 * amount;
        let mut max_red = 0.0f32;
        for x in block.iter_mut() {
            let mut y = self.low.process(*x);
            y = self.presence.process(y);
            y = self.air.process(y);
            if deess {
                let hi = self.detect.process(y);
                let (af, ah) = (y.abs(), hi.abs());
                let c = if af > self.env_full {
                    self.env_att
                } else {
                    self.env_rel
                };
                self.env_full = af + c * (self.env_full - af);
                let c = if ah > self.env_hi {
                    self.env_att
                } else {
                    self.env_rel
                };
                self.env_hi = ah + c * (self.env_hi - ah);
                let hi_db = lin_to_db(self.env_hi);
                let ratio_db = hi_db - lin_to_db(self.env_full);
                let target = if hi_db > -50.0 && ratio_db > thr_db {
                    ((ratio_db - thr_db) * (0.5 + amount) + 3.0 * amount).min(max_red_db)
                } else {
                    0.0
                };
                let c = if target > self.red_db {
                    self.ds_att
                } else {
                    self.ds_rel
                };
                self.red_db = target + c * (self.red_db - target);
                if self.counter == 0 {
                    let g = -(self.red_db * 10.0).round() / 10.0;
                    if g != self.applied_db {
                        self.shelf.set_high_shelf(self.fs, 4500.0, g as f64);
                        self.applied_db = g;
                    }
                }
                self.counter = (self.counter + 1) % DEESS_UPDATE;
                max_red = max_red.max(self.red_db);
                y = self.shelf.process(y);
            }
            *x = y;
        }
        self.deess_db = -max_red;
    }
    fn reset(&mut self) {
        for b in [
            &mut self.low,
            &mut self.presence,
            &mut self.air,
            &mut self.detect,
            &mut self.shelf,
        ] {
            b.reset();
        }
        self.env_full = 0.0;
        self.env_hi = 0.0;
        self.red_db = 0.0;
    }
}

// ---------------------------------------------------------------------------
// Auto-level (slow AGC + compressor)
// ---------------------------------------------------------------------------

/// Slow AGC that moves speech towards `target_db` (adapts only while the gate reports voice),
/// followed by an RMS compressor whose ratio/threshold firm up with `firmness`.
///
/// - AGC: speech level = 400 ms mean-square average while voice is active; gain slews at
///   `3 + 9·firmness` dB/s, clamped to `[-12, max_gain_db]`.
/// - Compressor: threshold `target + 8 − 6·firmness` dB, ratio `1.5 + 4.5·firmness`,
///   attack 5 ms, release 120 ms.
pub struct AutoLevel {
    params: AutoLevelParams,
    fs: f32,
    speech_ms: f32,
    speech_coef: f32,
    agc_db: f32,
    comp_env: f32,
    comp_att: f32,
    comp_rel: f32,
    comp_gain_db: f32,
    /// Voice active for the current block (set by the chain from the gate).
    pub voice_active: bool,
    /// Average gain change over the last block, dB.
    pub gain_change_db: f32,
}

impl AutoLevel {
    pub fn new(params: AutoLevelParams, fs: u32) -> Self {
        let f = fs as f32;
        Self {
            params,
            fs: f,
            speech_ms: 0.0,
            speech_coef: tc_coef(400.0, f),
            agc_db: 0.0,
            comp_env: 0.0,
            comp_att: tc_coef(5.0, f),
            comp_rel: tc_coef(120.0, f),
            comp_gain_db: 0.0,
            voice_active: false,
            gain_change_db: 0.0,
        }
    }
    pub fn set_params(&mut self, p: &AutoLevelParams) {
        self.params = *p;
    }
}

impl Processor for AutoLevel {
    fn name(&self) -> &'static str {
        "auto-level"
    }
    fn prepare(&mut self, sample_rate: u32, _max: usize) -> Result<()> {
        *self = Self::new(self.params, sample_rate);
        Ok(())
    }
    fn process(&mut self, block: &mut [f32]) {
        if !self.params.enabled || block.is_empty() {
            self.gain_change_db = 0.0;
            return;
        }
        let p = self.params;
        let firm = p.firmness.clamp(0.0, 1.0);
        // --- AGC (block rate) ---
        if self.voice_active {
            for x in block.iter() {
                let s = x * x;
                self.speech_ms = s + self.speech_coef * (self.speech_ms - s);
            }
            if self.speech_ms > 1e-9 {
                let level_db = 10.0 * self.speech_ms.log10();
                let want = (p.target_db - level_db).clamp(-12.0, p.max_gain_db.max(0.0));
                let step = (3.0 + 9.0 * firm) * block.len() as f32 / self.fs;
                self.agc_db += (want - self.agc_db).clamp(-step, step);
            }
        }
        let agc = db_to_lin(self.agc_db);
        // --- compressor (sample rate) ---
        let thr = p.target_db + 8.0 - 6.0 * firm;
        let ratio = 1.5 + 4.5 * firm;
        let mut sum_db = 0.0f32;
        for x in block.iter_mut() {
            let y = *x * agc;
            let s = y * y;
            let c = if s > self.comp_env {
                self.comp_att
            } else {
                self.comp_rel
            };
            self.comp_env = s + c * (self.comp_env - s);
            let lvl = 10.0 * self.comp_env.max(1e-12).log10();
            self.comp_gain_db = if lvl > thr {
                (thr - lvl) * (1.0 - 1.0 / ratio)
            } else {
                0.0
            };
            sum_db += self.comp_gain_db;
            *x = y * db_to_lin(self.comp_gain_db);
        }
        self.gain_change_db = self.agc_db + sum_db / block.len() as f32;
    }
    fn reset(&mut self) {
        self.speech_ms = 0.0;
        self.agc_db = 0.0;
        self.comp_env = 0.0;
        self.comp_gain_db = 0.0;
    }
}

// ---------------------------------------------------------------------------
// Limiter
// ---------------------------------------------------------------------------

/// Limiter look-ahead length in frames (1 ms @ 48 kHz).
pub const LIMITER_LOOKAHEAD: usize = 48;
const LA: usize = LIMITER_LOOKAHEAD;

/// Brick-wall look-ahead peak limiter. Guarantees `|y| ≤ ceiling`:
/// required gain → sliding minimum over the look-ahead window → moving average of the same
/// length (each averaged value already covers the peak, so the mean is ≤ the required gain)
/// → instant-down / 80 ms release smoothing (always ≤ the averaged gain) → final clamp.
/// Adds [`LIMITER_LOOKAHEAD`] frames latency (also when disabled, so latency is constant).
pub struct Limiter {
    params: LimiterParams,
    ceiling: f32,
    delay: [f32; LA],
    req: [f32; LA + 1],
    mins: [f32; LA],
    pos: usize,
    gain: f32,
    rel: f32,
    /// Minimum gain applied in the last block, dB (≤ 0).
    pub gain_reduction_db: f32,
}

impl Limiter {
    pub fn new(params: LimiterParams, fs: u32) -> Self {
        Self {
            ceiling: db_to_lin(params.ceiling_db.min(0.0)),
            params,
            delay: [0.0; LA],
            req: [1.0; LA + 1],
            mins: [1.0; LA],
            pos: 0,
            gain: 1.0,
            rel: tc_coef(80.0, fs as f32),
            gain_reduction_db: 0.0,
        }
    }
    pub fn set_params(&mut self, p: &LimiterParams) {
        self.params = *p;
        self.ceiling = db_to_lin(p.ceiling_db.min(0.0));
    }
}

impl Processor for Limiter {
    fn name(&self) -> &'static str {
        "limiter"
    }
    fn prepare(&mut self, sample_rate: u32, _max: usize) -> Result<()> {
        *self = Self::new(self.params, sample_rate);
        Ok(())
    }
    fn process(&mut self, block: &mut [f32]) {
        let mut min_g = 1.0f32;
        let enabled = self.params.enabled;
        let c = self.ceiling;
        for x in block.iter_mut() {
            let input = if x.is_finite() { *x } else { 0.0 };
            let i = self.pos;
            let delayed = self.delay[i];
            self.delay[i] = input;
            let a = input.abs();
            let g_req = if a > c { c / a } else { 1.0 };
            // Required-gain history (oldest first), LA+1 values.
            self.req.copy_within(1.., 0);
            self.req[LA] = g_req;
            let m = self.req.iter().copied().fold(1.0f32, f32::min);
            self.mins[i] = m;
            let avg = self.mins.iter().sum::<f32>() / LA as f32;
            self.gain = if avg < self.gain {
                avg
            } else {
                avg + self.rel * (self.gain - avg)
            };
            self.pos = (i + 1) % LA;
            let y = if enabled {
                min_g = min_g.min(self.gain);
                (delayed * self.gain).clamp(-c, c)
            } else {
                delayed
            };
            *x = y;
        }
        self.gain_reduction_db = lin_to_db(min_g);
    }
    fn reset(&mut self) {
        self.delay = [0.0; LA];
        self.req = [1.0; LA + 1];
        self.mins = [1.0; LA];
        self.pos = 0;
        self.gain = 1.0;
    }
    fn latency_frames(&self) -> usize {
        LA
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::biquad::rms_db;
    use yeti_types::{BLOCK_FRAMES, ENGINE_SAMPLE_RATE};

    const FS: u32 = ENGINE_SAMPLE_RATE;

    fn sine(f: f32, amp: f32, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| amp * (2.0 * std::f32::consts::PI * f * i as f32 / FS as f32).sin())
            .collect()
    }

    fn run(p: &mut dyn Processor, x: &mut [f32]) {
        for b in x.chunks_mut(BLOCK_FRAMES) {
            p.process(b);
        }
    }

    #[test]
    fn hpf_attenuates_30hz_by_20db_and_passes_voice() {
        let mut h = HighPass::new(HighPassParams::default(), FS);
        // Analytic response.
        assert!(h.magnitude_db(30.0) < -20.0, "{}", h.magnitude_db(30.0));
        assert!(h.magnitude_db(300.0).abs() < 0.5);
        // Measured on signals (skip 0.5 s transient).
        for (f, check) in [(30.0, -20.0f32), (1000.0, 0.0)] {
            let mut x = sine(f, 0.5, FS as usize * 2);
            let in_db = rms_db(&x[FS as usize..]);
            run(&mut h, &mut x);
            let out_db = rms_db(&x[FS as usize..]);
            let d = out_db - in_db;
            if f < 100.0 {
                assert!(d <= check, "HPF @{f} Hz: {d:.1} dB");
            } else {
                assert!(d.abs() < 0.5, "HPF @{f} Hz: {d:.1} dB");
            }
            h.reset();
        }
    }

    #[test]
    fn limiter_holds_ceiling_on_hot_signals() {
        let mut l = Limiter::new(LimiterParams::default(), FS);
        let ceiling = db_to_lin(-1.0);
        // +6 dBFS sine, then full-scale square-ish bursts and single-sample spikes.
        let mut x = sine(440.0, 2.0, FS as usize);
        x.extend((0..FS as usize).map(|i| if (i / 100) % 2 == 0 { 1.5 } else { -1.5 }));
        x.extend((0..FS as usize).map(|i| if i % 997 == 0 { 8.0 } else { 0.01 }));
        run(&mut l, &mut x);
        let peak = x.iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(peak <= ceiling + 1e-6, "peak {} dBFS", lin_to_db(peak));
        // Quiet signals pass unchanged (apart from the look-ahead delay).
        let mut l = Limiter::new(LimiterParams::default(), FS);
        let src = sine(440.0, 0.25, 4800);
        let mut y = src.clone();
        run(&mut l, &mut y);
        for i in LA..4800 {
            assert!((y[i] - src[i - LA]).abs() < 1e-6);
        }
    }

    #[test]
    fn gate_passes_voice_level_and_attenuates_quiet_noise() {
        let p = GateParams::default(); // −50 dBFS threshold, 30 dB range
        let mut g = Gate::new(p, FS);
        // Loud tone (−20 dBFS RMS): passes within 0.5 dB after attack.
        let mut x = sine(300.0, 0.141, FS as usize);
        let in_db = rms_db(&x[4800..]);
        run(&mut g, &mut x);
        assert!((rms_db(&x[4800..]) - in_db).abs() < 0.5);
        assert!(g.is_open());
        // Quiet noise (≈ −66 dBFS): closes and attenuates by ~range.
        let mut seed = 1u32;
        let mut n: Vec<f32> = (0..FS as usize * 3 / 2)
            .map(|_| {
                seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                ((seed >> 8) as f32 / (1u32 << 24) as f32 - 0.5) * 0.0017
            })
            .collect();
        let in_db = rms_db(&n[48_000..]);
        run(&mut g, &mut n);
        let att = in_db - rms_db(&n[48_000..]);
        assert!(!g.is_open());
        assert!(att > 28.0, "gate attenuation {att:.1} dB");
    }

    #[test]
    fn deesser_reduces_sibilance_but_not_vowels() {
        let mut p = EqParams {
            presence_gain_db: 0.0,
            air_gain_db: 0.0,
            ..EqParams::default()
        };
        p.deesser_amount = 0.8;
        let mut e = VoiceEq::new(p, FS);
        let mut v = sine(250.0, 0.3, FS as usize / 2);
        let vin = rms_db(&v[4800..]);
        run(&mut e, &mut v);
        assert!((rms_db(&v[4800..]) - vin).abs() < 0.5, "vowel changed");
        let mut s = sine(7000.0, 0.3, FS as usize / 2);
        let sin = rms_db(&s[4800..]);
        run(&mut e, &mut s);
        assert!(sin - rms_db(&s[4800..]) > 4.0, "sibilance not reduced");
    }

    #[test]
    fn auto_level_moves_speech_towards_target() {
        let p = AutoLevelParams {
            firmness: 0.8,
            ..AutoLevelParams::default()
        };
        let mut a = AutoLevel::new(p, FS);
        a.voice_active = true;
        let mut x = sine(200.0, 0.02, FS as usize * 6); // ≈ −37 dBFS
        run(&mut a, &mut x);
        let out = rms_db(&x[FS as usize * 5..]);
        // Limited by max_gain 12 dB → ≈ −25 dBFS.
        assert!(out > -27.0 && out < -22.0, "out {out:.1}");
        assert!(a.gain_change_db > 10.0);
    }
}
