//! Offline benchmark: DFN vs RNNoise latency / CPU / quality on the speech + keyboard/fan
//! fixtures, per-stage CPU, and (feature `aec`) AEC ERLE. Prints Markdown to stdout.
//!
//! ```sh
//! cargo run --release -p yeti-dsp --features aec --bin yeti-dsp-bench [-- --wav-dir DIR]
//! ```
//! Run in **release**: debug numbers are meaningless for CPU.

use std::time::Instant;
use yeti_dsp::fixtures::{self, FixtureMetrics};
use yeti_dsp::stages::{AutoLevel, Gate, HighPass, Limiter, VoiceEq};
use yeti_dsp::{Processor, VoiceChain};
use yeti_types::{BLOCK_FRAMES, DspParams, NoiseModel, Preset};

const SNR_DB: f64 = 5.0;
const MARGIN_MS: f64 = 200.0;

struct Fixture {
    name: &'static str,
    clean: Vec<f32>,
    noisy: Vec<f32>,
}

fn fixtures_list() -> Vec<Fixture> {
    let mut v = Vec::new();
    if let Some(clean) = fixtures::tts_speech() {
        let noise = fixtures::keyboard_and_fan(clean.len() as f64 / fixtures::FS + 0.1, 21);
        let (noisy, _) = fixtures::mix_at_snr(&clean, &noise, SNR_DB);
        v.push(Fixture {
            name: "TTS speech + keyboard/fan",
            clean,
            noisy,
        });
    } else {
        eprintln!("warning: tests/fixtures/speech_tts.wav not found, TTS fixture skipped");
    }
    let clean = fixtures::speech_like(12.0, 1.0, 1.0, 7);
    let noise = fixtures::keyboard_and_fan(12.1, 33);
    let (noisy, _) = fixtures::mix_at_snr(&clean, &noise, SNR_DB);
    v.push(Fixture {
        name: "synthetic speech + keyboard/fan",
        clean,
        noisy,
    });
    v
}

/// HPF → NS → gate only (EQ / auto-level / limiter off): isolates noise removal.
fn ns_only(model: NoiseModel) -> DspParams {
    let mut p = DspParams::for_preset(Preset::Gaming);
    p.noise.model = model;
    p.eq.enabled = false;
    p.auto_level.enabled = false;
    p.limiter.enabled = false;
    p
}

fn preset(pr: Preset, model: NoiseModel) -> DspParams {
    let mut p = DspParams::for_preset(pr);
    p.noise.model = model;
    p
}

fn pct(sorted: &[f64], q: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

struct Row {
    label: String,
    latency_reported_ms: f64,
    latency_measured_ms: f64,
    mean_us: f64,
    p99_us: f64,
    max_us: f64,
    m: FixtureMetrics,
}

fn run(label: String, p: DspParams, fx: &Fixture, wav_dir: Option<&str>) -> Row {
    let mut chain = VoiceChain::new(p).expect("chain");
    // Warm-up pass (first DFN frames are slower), then a fresh chain for the measured run.
    let _ = fixtures::render(&mut chain, &fx.noisy[..48_000.min(fx.noisy.len())], None);
    let mut chain = VoiceChain::new(p).expect("chain");
    let (out, times) = fixtures::render(&mut chain, &fx.noisy, None);
    let reported = chain.latency_frames();
    // Measured latency: cross-correlate clean speech vs output around the first speech.
    let start = fixtures::speech_mask(&fx.clean)
        .iter()
        .position(|v| *v)
        .unwrap_or(0);
    let measured = fixtures::estimate_delay(&fx.clean, &out, start, 48_000, 4_800);
    // Reference = clean speech through the same rumble filter (its phase/rumble removal is
    // intended). SI-SDR is only meaningful for NS-only rows (EQ/AGC change the waveform).
    let reference = fixtures::highpassed(&fx.clean, p.high_pass);
    let m = fixtures::evaluate(&reference, &fx.noisy, &out, reported, MARGIN_MS);
    if let Some(d) = wav_dir {
        let f = format!(
            "{d}/{}.wav",
            label.replace([' ', '/', '+', '(', ')', ','], "_")
        );
        let _ = fixtures::write_wav_mono(std::path::Path::new(&f), &out[reported..], false);
    }
    let mut t: Vec<f64> = times.iter().map(|s| s * 1e6).collect();
    let mean = t.iter().sum::<f64>() / t.len() as f64;
    t.sort_by(|a, b| a.total_cmp(b));
    Row {
        label,
        latency_reported_ms: reported as f64 / 48.0,
        latency_measured_ms: measured as f64 / 48.0,
        mean_us: mean,
        p99_us: pct(&t, 0.99),
        max_us: *t.last().unwrap(),
        m,
    }
}

fn stage_cpu(name: &str, p: &mut dyn Processor, x: &[f32]) -> (String, f64, f64) {
    let mut buf = x.to_vec();
    let mut times = Vec::new();
    for b in buf.chunks_mut(BLOCK_FRAMES) {
        if b.len() != BLOCK_FRAMES {
            break;
        }
        let t = Instant::now();
        p.process(b);
        times.push(t.elapsed().as_secs_f64() * 1e6);
    }
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    times.sort_by(|a, b| a.total_cmp(b));
    (name.to_string(), mean, pct(&times, 0.99))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let wav_dir = args
        .iter()
        .position(|a| a == "--wav-dir")
        .and_then(|i| args.get(i + 1))
        .map(String::as_str);
    if let Some(d) = wav_dir {
        std::fs::create_dir_all(d).expect("create wav dir");
    }
    println!("# yeti-dsp offline benchmark\n");
    println!(
        "Build: {}; DFN compiled: {}; AEC compiled: {}; noise SNR {SNR_DB} dB (speech-active RMS vs noise RMS); noise-only frames exclude ±{MARGIN_MS} ms around speech.\n",
        if cfg!(debug_assertions) {
            "DEBUG (CPU numbers invalid)"
        } else {
            "release"
        },
        VoiceChain::dfn_available(),
        VoiceChain::aec_available()
    );

    // DFN model load time.
    #[cfg(feature = "dfn")]
    {
        let t = Instant::now();
        let _d = yeti_dsp::dfn::DfnSuppressor::new().expect("dfn");
        println!(
            "DFN model load: {:.0} ms\n",
            t.elapsed().as_secs_f64() * 1e3
        );
    }
    let t = Instant::now();
    let _c = VoiceChain::new(DspParams::default()).expect("chain");
    println!(
        "VoiceChain::new (all models): {:.0} ms\n",
        t.elapsed().as_secs_f64() * 1e3
    );

    let fxs = fixtures_list();
    let mut models = vec![("RNNoise", NoiseModel::RnNoise)];
    if VoiceChain::dfn_available() {
        models.insert(0, ("DFN", NoiseModel::DeepFilterNet));
    }

    for fx in &fxs {
        if let Some(d) = wav_dir {
            let base = fx.name.replace([' ', '/', '+'], "_");
            let _ = fixtures::write_wav_mono(
                std::path::Path::new(&format!("{d}/{base}__clean.wav")),
                &fx.clean,
                false,
            );
            let _ = fixtures::write_wav_mono(
                std::path::Path::new(&format!("{d}/{base}__noisy.wav")),
                &fx.noisy,
                false,
            );
        }
        println!("## {}\n", fx.name);
        println!(
            "SI-SDR reference = clean speech through the same rumble filter; it is only meaningful for the NS-only rows (EQ / auto-level / AEC band-splitting change the waveform on purpose).\n"
        );
        println!(
            "| Config | Latency reported / measured (ms) | Block time mean / p99 / max (µs) | CPU (1 core) | Noise-only reduction (dB) | Speech level Δ (dB) | SI-SDR in → out (dB) |"
        );
        println!("|---|---|---|---|---|---|---|");
        let mut rows = Vec::new();
        for (mn, m) in &models {
            rows.push(run(
                format!("{mn} NS-only (HPF+NS+gate)"),
                ns_only(*m),
                fx,
                wav_dir,
            ));
        }
        for (mn, m) in &models {
            rows.push(run(
                format!("{mn} Gaming preset"),
                preset(Preset::Gaming, *m),
                fx,
                wav_dir,
            ));
            rows.push(run(
                format!("{mn} Meeting preset"),
                preset(Preset::Meeting, *m),
                fx,
                wav_dir,
            ));
        }
        let (sf, nf) = (rows[0].m.speech_frames, rows[0].m.noise_frames);
        for r in rows {
            println!(
                "| {} | {:.1} / {:.1} | {:.0} / {:.0} / {:.0} | {:.1} % | {:.1} | {:+.1} | {:.1} → {:.1} |",
                r.label,
                r.latency_reported_ms,
                r.latency_measured_ms,
                r.mean_us,
                r.p99_us,
                r.max_us,
                r.mean_us / 10_000.0 * 100.0,
                r.m.noise_reduction_db,
                r.m.speech_level_delta_db,
                r.m.si_sdr_in_db,
                r.m.si_sdr_out_db
            );
        }
        println!("\n({sf} speech frames, {nf} noise-only frames of 10 ms)\n");
    }

    // Per-stage CPU on the first fixture.
    let x = &fxs[0].noisy;
    let mut stages: Vec<(String, f64, f64)> = Vec::new();
    let fs = 48_000;
    let d = DspParams::default();
    stages.push(stage_cpu(
        "HPF (4th-order)",
        &mut HighPass::new(d.high_pass, fs),
        x,
    ));
    stages.push(stage_cpu(
        "RNNoise",
        &mut yeti_dsp::rnnoise::RnnSuppressor::new(),
        x,
    ));
    #[cfg(feature = "dfn")]
    stages.push(stage_cpu(
        "DeepFilterNet3",
        &mut yeti_dsp::dfn::DfnSuppressor::new().unwrap(),
        x,
    ));
    stages.push(stage_cpu("Voice gate", &mut Gate::new(d.gate, fs), x));
    stages.push(stage_cpu("EQ + de-esser", &mut VoiceEq::new(d.eq, fs), x));
    let mut agc = AutoLevel::new(d.auto_level, fs);
    agc.voice_active = true;
    stages.push(stage_cpu("Auto-level", &mut agc, x));
    stages.push(stage_cpu("Limiter", &mut Limiter::new(d.limiter, fs), x));
    #[cfg(feature = "aec")]
    stages.push(stage_cpu(
        "AEC3 (no reference)",
        &mut yeti_dsp::aec::EchoCancel::new().unwrap(),
        x,
    ));
    println!("## Per-stage CPU (`{}`)\n", fxs[0].name);
    println!("| Stage | mean (µs / 10 ms block) | p99 (µs) | CPU (1 core) |\n|---|---|---|---|");
    for (n, mean, p99) in stages {
        println!("| {n} | {mean:.1} | {p99:.1} | {:.2} % |", mean / 100.0);
    }
    println!();

    #[cfg(feature = "aec")]
    aec_erle(&fxs);
}

#[cfg(feature = "aec")]
fn aec_erle(fxs: &[Fixture]) {
    use yeti_dsp::aec::EchoCancel;
    println!("## AEC (WebRTC AEC3) offline ERLE\n");
    println!(
        "Far end = speech fixture played through a synthetic room (pure delay + exponentially decaying noise IR, RT60 150 ms, −6 dB). Near mic = echo + −70 dBFS white noise (single talk). ERLE = 10·log10(E[mic] / E[AEC out]) over frames where the echo is within 35 dB of its peak, excluding the first 3 s (convergence). Double talk: a second (synthetic) talker at −23 dBFS is added to the mic; Δ = its level in the AEC output vs. clean, aligned by the APM's 430-frame delay.\n"
    );
    println!(
        "| Far-end speech | Echo delay (ms) | ERLE after 3 s (dB) | Double-talk near-speech Δ (dB) |\n|---|---|---|---|"
    );
    for (fi, fx) in fxs.iter().enumerate() {
        for delay_ms in [20.0, 60.0] {
            let far = &fx.clean;
            let ir = fixtures::room_ir(delay_ms, 150.0, -6.0, 5 + fi as u64);
            let echo = fixtures::convolve(far, &ir);
            let mut rng = fixtures::Rng::new(99);
            let floor = 10f64.powf(-70.0 / 20.0) * 3f64.sqrt();
            let mic: Vec<f32> = echo
                .iter()
                .map(|e| e + (rng.bipolar() * floor) as f32)
                .collect();
            let mut aec = EchoCancel::new().unwrap();
            let mut out = mic.clone();
            for (bi, blk) in out.chunks_mut(BLOCK_FRAMES).enumerate() {
                if blk.len() != BLOCK_FRAMES {
                    break;
                }
                let f = &far[bi * BLOCK_FRAMES..bi * BLOCK_FRAMES + BLOCK_FRAMES];
                aec.process_with_reference(blk, Some(f));
            }
            let erle = erle_db(&echo, &mic, &out, 3 * 48_000);
            // Double talk: near speech (different synthetic talker) + same echo.
            let near = fixtures::speech_like(far.len() as f64 / fixtures::FS, 0.5, 0.5, 1234);
            let near: Vec<f32> = near.iter().map(|v| v * 0.7).collect();
            let mic2: Vec<f32> = echo.iter().zip(&near).map(|(e, n)| e + n).collect();
            let mut aec2 = EchoCancel::new().unwrap();
            let mut out2 = mic2.clone();
            for (bi, blk) in out2.chunks_mut(BLOCK_FRAMES).enumerate() {
                if blk.len() != BLOCK_FRAMES {
                    break;
                }
                let f = &far[bi * BLOCK_FRAMES..bi * BLOCK_FRAMES + BLOCK_FRAMES];
                aec2.process_with_reference(blk, Some(f));
            }
            let m = fixtures::evaluate(
                &near,
                &mic2,
                &out2,
                yeti_dsp::aec::AEC_LATENCY_FRAMES,
                MARGIN_MS,
            );
            println!(
                "| {} | {delay_ms:.0} | {erle:.1} | {:+.1} |",
                fx.name.split(" +").next().unwrap_or(fx.name),
                m.speech_level_delta_db
            );
        }
    }
    println!();
}

#[cfg(feature = "aec")]
fn erle_db(echo: &[f32], mic: &[f32], out: &[f32], skip: usize) -> f64 {
    let frames: Vec<f64> = echo
        .chunks(480)
        .map(|c| c.iter().map(|v| (*v as f64).powi(2)).sum())
        .collect();
    let max = frames.iter().cloned().fold(0.0, f64::max);
    let (mut em, mut eo) = (0f64, 0f64);
    for (fi, e) in frames.iter().enumerate() {
        let s = fi * 480;
        if s < skip || *e < max * 10f64.powf(-3.5) || s + 480 > out.len() {
            continue;
        }
        for i in s..s + 480 {
            em += (mic[i] as f64).powi(2);
            eo += (out[i] as f64).powi(2);
        }
    }
    10.0 * (em / eo.max(1e-20)).log10()
}
