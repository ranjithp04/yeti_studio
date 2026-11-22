//! Deterministic test fixtures and objective metrics for the voice chain.
//!
//! **Limitation (honest):** no recorded speech/keyboard/fan clips are available offline, so:
//! - *speech* is either macOS `say` TTS (`tests/fixtures/speech_tts.wav`, real phonemes but
//!   synthetic voice) or [`speech_like`]: glottal pulse trains through moving formant
//!   resonators with syllable envelopes, fricatives and pauses;
//! - *keyboard* ([`keyboard`]) is synthesised mechanical-switch clicks (press + release,
//!   broadband transient + plate/body resonances, bursty typing rhythm);
//! - *fan* ([`fan`]) is low-passed pink-ish broadband noise + blade-pass tones + rumble.
//!
//! Real-mic recordings may behave differently (room reverb, real switch spectra).

use crate::biquad::Biquad;
use std::f64::consts::PI;

pub const FS: f64 = 48_000.0;

/// Small deterministic PRNG (xorshift64*).
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in [0, 1).
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// Uniform in [-1, 1).
    pub fn bipolar(&mut self) -> f64 {
        self.uniform() * 2.0 - 1.0
    }
    pub fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.uniform()
    }
}

fn normalize_rms(x: &mut [f32], target_db: f64, mask: Option<&[bool]>) {
    let (mut e, mut n) = (0f64, 0usize);
    for (i, v) in x.iter().enumerate() {
        if mask.is_none_or(|m| m[i]) {
            e += (*v as f64).powi(2);
            n += 1;
        }
    }
    let rms = (e / n.max(1) as f64).sqrt().max(1e-12);
    let g = 10f64.powf(target_db / 20.0) / rms;
    x.iter_mut().for_each(|v| *v = (*v as f64 * g) as f32);
}

/// Vowel formants (F1, F2, F3) in Hz.
const VOWELS: [[f64; 3]; 6] = [
    [730.0, 1090.0, 2440.0], // a
    [530.0, 1840.0, 2480.0], // e
    [270.0, 2290.0, 3010.0], // i
    [570.0, 840.0, 2410.0],  // o
    [300.0, 870.0, 2240.0],  // u
    [660.0, 1720.0, 2410.0], // ae
];

/// Speech-like signal: phrases of syllables separated by pauses, with explicit leading and
/// trailing silence. Returns the signal normalised to −20 dBFS RMS over voiced samples.
///
/// Layout: `lead_s` silence, then phrases (1.2–2.5 s) separated by 0.6–1.0 s pauses until
/// `dur_s − tail_s`, then silence.
pub fn speech_like(dur_s: f64, lead_s: f64, tail_s: f64, seed: u64) -> Vec<f32> {
    let n = (dur_s * FS) as usize;
    let mut out = vec![0f32; n];
    let mut rng = Rng::new(seed);
    let end = ((dur_s - tail_s) * FS) as usize;
    let mut t = (lead_s * FS) as usize;
    let mut phase = 0f64;
    let mut formants = [Biquad::default(); 3];
    let mut fric = Biquad::default();
    fric.set_bandpass(FS, 6000.0, 1.2);
    let mut fric2 = Biquad::default();
    fric2.set_highpass(FS, 3500.0, 0.7);
    let f0_base = rng.range(105.0, 135.0);
    while t < end {
        let phrase_len = (rng.range(1.2, 2.5) * FS) as usize;
        let phrase_end = (t + phrase_len).min(end);
        let mut v_prev = rng.uniform() * VOWELS.len() as f64;
        while t < phrase_end {
            let syl = (rng.range(0.14, 0.26) * FS) as usize;
            let syl_end = (t + syl).min(phrase_end);
            let len = syl_end - t;
            let v = (rng.uniform() * VOWELS.len() as f64) as usize % VOWELS.len();
            let from = VOWELS[(v_prev as usize) % VOWELS.len()];
            let to = VOWELS[v];
            v_prev = v as f64;
            let has_fric = rng.uniform() < 0.35;
            let fric_len = if has_fric {
                (rng.range(0.05, 0.11) * FS) as usize
            } else {
                0
            };
            let amp = rng.range(0.6, 1.0);
            let f0_slope = rng.range(-30.0, 20.0);
            for k in 0..len {
                let i = t + k;
                let u = k as f64 / len as f64;
                // Fricative onset (unvoiced, "s"/"sh").
                if k < fric_len {
                    let e = (PI * k as f64 / fric_len as f64).sin();
                    let nz = rng.bipolar();
                    out[i] +=
                        (0.35 * e * fric2.process(fric.process(nz as f32) * 3.0) as f64) as f32;
                    continue;
                }
                let uv = (k - fric_len) as f64 / (len - fric_len).max(1) as f64;
                // Formant glide.
                if k % 48 == 0 {
                    let g = (u * 1.5).min(1.0);
                    for (j, b) in formants.iter_mut().enumerate() {
                        let f = from[j] + (to[j] - from[j]) * g;
                        b.set_bandpass(FS, f, 6.0 + 2.0 * j as f64);
                    }
                }
                let f0 = f0_base + f0_slope * uv + 4.0 * rng.bipolar();
                phase += f0 / FS;
                // Rosenberg-like glottal pulse: open phase 60 %.
                let ph = phase.fract();
                let pulse = if ph < 0.4 {
                    0.5 * (1.0 - (PI * ph / 0.4).cos())
                } else if ph < 0.6 {
                    (PI * (ph - 0.4) / 0.4).cos()
                } else {
                    0.0
                };
                let src = (pulse - 0.35) as f32 + 0.02 * rng.bipolar() as f32;
                let y = formants[0].process(src) * 1.0
                    + formants[1].process(src) * 0.6
                    + formants[2].process(src) * 0.3;
                // Syllable envelope: raised cosine attack/decay.
                let env = (PI * uv).sin().powf(0.7) * amp;
                out[i] += (y as f64 * env) as f32;
            }
            t = syl_end;
        }
        t += (rng.range(0.6, 1.0) * FS) as usize;
    }
    let mask: Vec<bool> = out.iter().map(|v| v.abs() > 1e-4).collect();
    normalize_rms(&mut out, -20.0, Some(&mask));
    out
}

/// Mechanical keyboard typing: bursts of 3–12 keystrokes (≈ 6–9 keys/s), each a press and a
/// release click 60–120 ms apart. Click = 1–4 ms broadband transient + damped resonances
/// (plate ≈ 2–4 kHz, body ≈ 300–600 Hz). Normalised to 0 dBFS peak before mixing.
pub fn keyboard(dur_s: f64, seed: u64) -> Vec<f32> {
    let n = (dur_s * FS) as usize;
    let mut out = vec![0f32; n];
    let mut rng = Rng::new(seed);
    let mut t = (rng.range(0.05, 0.3) * FS) as usize;
    while t < n {
        let burst = 3 + (rng.uniform() * 10.0) as usize;
        for _ in 0..burst {
            let press_amp = rng.range(0.5, 1.0);
            for (off, amp) in [
                (0usize, press_amp),
                ((rng.range(0.06, 0.12) * FS) as usize, press_amp * 0.6),
            ] {
                let start = t + off;
                let plate_f = rng.range(2000.0, 4000.0);
                let body_f = rng.range(300.0, 600.0);
                let tlen = (rng.range(0.001, 0.004) * FS) as usize;
                let len = (0.04 * FS) as usize;
                for k in 0..len {
                    let i = start + k;
                    if i >= n {
                        break;
                    }
                    let tt = k as f64 / FS;
                    let transient = if k < tlen {
                        rng.bipolar() * (1.0 - k as f64 / tlen as f64)
                    } else {
                        0.0
                    };
                    let plate = (2.0 * PI * plate_f * tt).sin() * (-tt / 0.004).exp() * 0.6;
                    let body = (2.0 * PI * body_f * tt).sin() * (-tt / 0.012).exp() * 0.4;
                    out[i] += (amp * (transient + plate + body)) as f32;
                }
            }
            t += (rng.range(0.09, 0.2) * FS) as usize;
        }
        t += (rng.range(0.3, 1.5) * FS) as usize;
    }
    let peak = out.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-9);
    out.iter_mut().for_each(|v| *v /= peak);
    out
}

/// Fan / wind: pink-ish broadband noise (Paul Kellet filter) low-passed at 2.5 kHz, a
/// 110 Hz blade-pass tone with harmonics, and < 60 Hz rumble (brown noise).
pub fn fan(dur_s: f64, seed: u64) -> Vec<f32> {
    let n = (dur_s * FS) as usize;
    let mut rng = Rng::new(seed);
    let (mut b0, mut b1, mut b2, mut b3, mut b4, mut b5, mut b6) =
        (0f64, 0f64, 0f64, 0f64, 0f64, 0f64, 0f64);
    let mut lp = Biquad::default();
    lp.set_lowpass(FS, 2500.0, 0.7);
    let mut rumble_lp = Biquad::default();
    rumble_lp.set_lowpass(FS, 60.0, 0.7);
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let w = rng.bipolar();
        b0 = 0.99886 * b0 + w * 0.0555179;
        b1 = 0.99332 * b1 + w * 0.0750759;
        b2 = 0.96900 * b2 + w * 0.1538520;
        b3 = 0.86650 * b3 + w * 0.3104856;
        b4 = 0.55000 * b4 + w * 0.5329522;
        b5 = -0.7616 * b5 - w * 0.0168980;
        let pink = b0 + b1 + b2 + b3 + b4 + b5 + b6 + w * 0.5362;
        b6 = w * 0.115926;
        let t = i as f64 / FS;
        let wobble = 1.0 + 0.01 * (2.0 * PI * 0.3 * t).sin();
        let tones = (1..=4)
            .map(|h| (2.0 * PI * 110.0 * h as f64 * wobble * t).sin() / h as f64)
            .sum::<f64>();
        let broadband = lp.process(pink as f32) as f64 * 0.12;
        let rumble = rumble_lp.process(rng.bipolar() as f32) as f64 * 4.0;
        out.push((broadband + 0.02 * tones + 0.08 * rumble) as f32);
    }
    normalize_rms(&mut out, -30.0, None);
    out
}

/// Noise mix used by the fixture tests: keyboard at −8 dB relative to the fan (by RMS).
pub fn keyboard_and_fan(dur_s: f64, seed: u64) -> Vec<f32> {
    let f = fan(dur_s, seed);
    let mut k = keyboard(dur_s, seed ^ 0xABCD);
    normalize_rms(&mut k, -38.0, None);
    f.iter().zip(&k).map(|(a, b)| a + b).collect()
}

/// Mix `speech + noise` so that speech-active RMS / overall noise RMS = `snr_db`.
/// Returns `(noisy, scaled_noise)`; `speech` is unchanged.
pub fn mix_at_snr(speech: &[f32], noise: &[f32], snr_db: f64) -> (Vec<f32>, Vec<f32>) {
    let n = speech.len().min(noise.len());
    let mask = speech_mask(&speech[..n]);
    let ps = mean_sq(&speech[..n], Some(&mask));
    let pn = mean_sq(&noise[..n], None);
    let g = (ps / pn.max(1e-20) / 10f64.powf(snr_db / 10.0)).sqrt();
    let scaled: Vec<f32> = noise[..n].iter().map(|v| (*v as f64 * g) as f32).collect();
    let noisy = speech[..n]
        .iter()
        .zip(&scaled)
        .map(|(a, b)| a + b)
        .collect();
    (noisy, scaled)
}

fn mean_sq(x: &[f32], mask: Option<&[bool]>) -> f64 {
    let (mut e, mut c) = (0f64, 0usize);
    for (i, v) in x.iter().enumerate() {
        if mask.is_none_or(|m| m[i]) {
            e += (*v as f64).powi(2);
            c += 1;
        }
    }
    e / c.max(1) as f64
}

/// Frame length used for segment labelling (10 ms).
pub const SEG: usize = 480;

/// Per-sample speech-activity mask from the clean signal: 10 ms frames whose RMS is within
/// 35 dB of the loudest frame.
pub fn speech_mask(clean: &[f32]) -> Vec<bool> {
    let frames: Vec<f64> = clean.chunks(SEG).map(|c| mean_sq(c, None)).collect();
    let max = frames.iter().cloned().fold(0.0, f64::max);
    let thr = max * 10f64.powf(-35.0 / 10.0);
    let mut m = vec![false; clean.len()];
    for (fi, e) in frames.iter().enumerate() {
        if *e > thr {
            let s = fi * SEG;
            let end = (s + SEG).min(clean.len());
            m[s..end].iter_mut().for_each(|v| *v = true);
        }
    }
    m
}

/// Segment classes for metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seg {
    Speech,
    /// Pure noise, at least `margin` away from any speech frame.
    NoiseOnly,
    /// Transition (excluded from both).
    Margin,
}

/// Label 10 ms frames of the clean speech: speech = within 35 dB of the loudest frame;
/// noise-only = no frame within `margin_ms` has energy above −50 dB re. the loudest frame
/// (the margin covers gate release and NS/STFT smearing); everything else is excluded.
pub fn label_frames(clean: &[f32], margin_ms: f64) -> Vec<Seg> {
    let frames: Vec<f64> = clean.chunks(SEG).map(|c| mean_sq(c, None)).collect();
    let max = frames.iter().cloned().fold(0.0, f64::max);
    let speech_thr = max * 10f64.powf(-35.0 / 10.0);
    // "Silent" = more than 50 dB below the loudest frame (TTS has −70…−88 dBFS dither).
    let silent_thr = max * 1e-5;
    let m = (margin_ms / 10.0).ceil() as isize;
    let is_sp: Vec<bool> = frames.iter().map(|e| *e > speech_thr).collect();
    let any_energy: Vec<bool> = frames.iter().map(|e| *e > silent_thr).collect();
    (0..frames.len())
        .map(|i| {
            if is_sp[i] {
                return Seg::Speech;
            }
            let lo = (i as isize - m).max(0) as usize;
            let hi = ((i as isize + m) as usize).min(frames.len() - 1);
            if (lo..=hi).any(|j| any_energy[j]) {
                Seg::Margin
            } else {
                Seg::NoiseOnly
            }
        })
        .collect()
}

/// Estimate the delay of `y` relative to `x` (0..=max_lag samples) by cross-correlation
/// over the first `win` samples of `x` starting at `start`.
pub fn estimate_delay(x: &[f32], y: &[f32], start: usize, win: usize, max_lag: usize) -> usize {
    let mut best = (0usize, f64::MIN);
    for lag in 0..=max_lag {
        let mut acc = 0f64;
        let end = (start + win).min(x.len());
        for (i, xv) in x.iter().enumerate().take(end).skip(start) {
            if let Some(v) = y.get(i + lag) {
                acc += *xv as f64 * *v as f64;
            }
        }
        if acc > best.1 {
            best = (lag, acc);
        }
    }
    best.0
}

/// Objective results of one fixture run.
#[derive(Debug, Clone, Copy, Default)]
pub struct FixtureMetrics {
    /// Noisy-input RMS − output RMS over noise-only frames, dB.
    pub noise_reduction_db: f64,
    /// Output speech-frame RMS − clean speech-frame RMS, dB (0 = level preserved).
    pub speech_level_delta_db: f64,
    pub si_sdr_in_db: f64,
    pub si_sdr_out_db: f64,
    pub noise_frames: usize,
    pub speech_frames: usize,
}

fn si_sdr(reference: &[f64], est: &[f64]) -> f64 {
    let dot: f64 = reference.iter().zip(est).map(|(a, b)| a * b).sum();
    let rr: f64 = reference.iter().map(|a| a * a).sum::<f64>().max(1e-20);
    let alpha = dot / rr;
    let (mut ts, mut es) = (0f64, 0f64);
    for (a, b) in reference.iter().zip(est) {
        let t = alpha * a;
        ts += t * t;
        es += (b - t) * (b - t);
    }
    10.0 * (ts / es.max(1e-20)).log10()
}

/// Compare `out` (delayed by `delay` samples) with `clean`/`noisy` using frame labels.
pub fn evaluate(
    clean: &[f32],
    noisy: &[f32],
    out: &[f32],
    delay: usize,
    margin_ms: f64,
) -> FixtureMetrics {
    let labels = label_frames(clean, margin_ms);
    let aligned = |i: usize| out.get(i + delay).copied().unwrap_or(0.0) as f64;
    let (mut n_in, mut n_out, mut nf) = (0f64, 0f64, 0usize);
    let (mut s_clean, mut s_out, mut sf) = (0f64, 0f64, 0usize);
    let (mut r, mut ein, mut eout) = (Vec::new(), Vec::new(), Vec::new());
    for (fi, l) in labels.iter().enumerate() {
        let s = fi * SEG;
        let e = (s + SEG).min(clean.len());
        match l {
            Seg::NoiseOnly => {
                nf += 1;
                for (i, v) in noisy.iter().enumerate().take(e).skip(s) {
                    n_in += (*v as f64).powi(2);
                    n_out += aligned(i).powi(2);
                }
            }
            Seg::Speech => {
                sf += 1;
                for (i, v) in clean.iter().enumerate().take(e).skip(s) {
                    s_clean += (*v as f64).powi(2);
                    s_out += aligned(i).powi(2);
                }
            }
            Seg::Margin => {}
        }
        if *l != Seg::Margin {
            for i in s..e {
                r.push(clean[i] as f64);
                ein.push(noisy[i] as f64);
                eout.push(aligned(i));
            }
        }
    }
    FixtureMetrics {
        noise_reduction_db: 10.0 * (n_in / n_out.max(1e-20)).log10(),
        speech_level_delta_db: 10.0 * (s_out / s_clean.max(1e-20)).log10(),
        si_sdr_in_db: si_sdr(&r, &ein),
        si_sdr_out_db: si_sdr(&r, &eout),
        noise_frames: nf,
        speech_frames: sf,
    }
}

/// Synthetic room echo path: `delay_ms` pure delay, then an exponentially decaying noise
/// impulse response (`rt60_ms`), scaled to `gain_db` energy gain. Returns the IR.
pub fn room_ir(delay_ms: f64, rt60_ms: f64, gain_db: f64, seed: u64) -> Vec<f32> {
    let d = (delay_ms * FS / 1000.0) as usize;
    let len = (rt60_ms * FS / 1000.0) as usize;
    let mut rng = Rng::new(seed);
    let mut ir = vec![0f32; d + len];
    let tau = rt60_ms / 1000.0 / 6.91; // −60 dB at rt60
    for k in 0..len {
        let t = k as f64 / FS;
        ir[d + k] = (rng.bipolar() * (-t / tau).exp()) as f32;
    }
    ir[d] += 1.0;
    let e: f64 = ir.iter().map(|v| (*v as f64).powi(2)).sum();
    let g = 10f64.powf(gain_db / 20.0) / e.sqrt();
    ir.iter_mut().for_each(|v| *v = (*v as f64 * g) as f32);
    ir
}

/// Direct-form convolution (offline only).
pub fn convolve(x: &[f32], ir: &[f32]) -> Vec<f32> {
    let nz: Vec<(usize, f32)> = ir
        .iter()
        .copied()
        .enumerate()
        .filter(|(_, v)| *v != 0.0)
        .collect();
    let mut y = vec![0f32; x.len()];
    for (i, yi) in y.iter_mut().enumerate() {
        let mut acc = 0f32;
        for &(k, h) in &nz {
            if k > i {
                break;
            }
            acc += h * x[i - k];
        }
        *yi = acc;
    }
    y
}

/// Read a WAV as mono f32 (channels averaged). Returns `(samples, sample_rate)`.
pub fn read_wav_mono(path: &std::path::Path) -> Result<(Vec<f32>, u32), hound::Error> {
    let mut r = hound::WavReader::open(path)?;
    let spec = r.spec();
    let ch = spec.channels.max(1) as usize;
    let inter: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => r.samples::<f32>().collect::<Result<_, _>>()?,
        hound::SampleFormat::Int => {
            let scale = 1.0 / (1u64 << (spec.bits_per_sample - 1)) as f32;
            r.samples::<i32>()
                .map(|s| s.map(|v| v as f32 * scale))
                .collect::<Result<_, _>>()?
        }
    };
    let mono = inter
        .chunks(ch)
        .map(|f| f.iter().sum::<f32>() / ch as f32)
        .collect();
    Ok((mono, spec.sample_rate))
}

/// Write mono 48 kHz WAV, 16-bit PCM (clamped) or 32-bit float.
pub fn write_wav_mono(path: &std::path::Path, x: &[f32], float: bool) -> Result<(), hound::Error> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: FS as u32,
        bits_per_sample: if float { 32 } else { 16 },
        sample_format: if float {
            hound::SampleFormat::Float
        } else {
            hound::SampleFormat::Int
        },
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for v in x {
        if float {
            w.write_sample(*v)?;
        } else {
            w.write_sample((v.clamp(-1.0, 1.0) * 32767.0).round() as i16)?;
        }
    }
    w.finalize()
}

/// Run `input` (and optional far-end reference) through `chain` in 480-frame blocks.
/// The input is zero-padded by the chain latency + one block so the delayed tail comes out.
/// Returns `(output, per-block processing time in seconds)`; output length = padded length.
pub fn render(
    chain: &mut crate::VoiceChain,
    input: &[f32],
    far: Option<&[f32]>,
) -> (Vec<f32>, Vec<f64>) {
    use yeti_types::BLOCK_FRAMES;
    let pad = chain.latency_frames() + BLOCK_FRAMES;
    let total = (input.len() + pad).div_ceil(BLOCK_FRAMES) * BLOCK_FRAMES;
    let mut out = input.to_vec();
    out.resize(total, 0.0);
    let mut times = Vec::with_capacity(total / BLOCK_FRAMES);
    let mut far_blk = [0f32; BLOCK_FRAMES];
    for (bi, blk) in out.chunks_mut(BLOCK_FRAMES).enumerate() {
        let far_ref = far.map(|f| {
            for (k, v) in far_blk.iter_mut().enumerate() {
                *v = f.get(bi * BLOCK_FRAMES + k).copied().unwrap_or(0.0);
            }
            &far_blk[..]
        });
        let t = std::time::Instant::now();
        chain.process_block(blk, far_ref);
        times.push(t.elapsed().as_secs_f64());
    }
    (out, times)
}

/// Locate `tests/fixtures/speech_tts.wav` of this crate (works from tests and binaries
/// run inside the workspace). Returns the clip normalised to −20 dBFS active RMS.
pub fn tts_speech() -> Option<Vec<f32>> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/speech_tts.wav");
    let (mut x, sr) = read_wav_mono(&p).ok()?;
    if sr != FS as u32 {
        return None;
    }
    let mask = speech_mask(&x);
    normalize_rms(&mut x, -20.0, Some(&mask));
    Some(x)
}

/// `x` through the chain's rumble filter alone: the fair reference for SI-SDR / level
/// comparisons (the HPF's phase shift and rumble removal are intended, not distortion).
pub fn highpassed(x: &[f32], p: yeti_types::HighPassParams) -> Vec<f32> {
    use crate::Processor;
    let mut h = crate::stages::HighPass::new(p, FS as u32);
    let mut y = x.to_vec();
    for b in y.chunks_mut(yeti_types::BLOCK_FRAMES) {
        h.process(b);
    }
    y
}
