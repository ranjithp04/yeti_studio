//! Phase 2 fixture tests: speech + keyboard/fan noise at a known SNR through the chain.
//!
//! Speech = macOS `say` TTS clip (`tests/fixtures/speech_tts.wav`); noise = synthesised
//! keyboard + fan (see `yeti_dsp::fixtures` for the limitation note). SNR 5 dB.
//! Acceptance (PLAN.md §4 Phase 2): noise-only segments ≥ 15 dB reduction, speech segments
//! within 3 dB, plus an SI-SDR improvement check.

use yeti_dsp::fixtures::{self, FixtureMetrics};
use yeti_dsp::{ActiveNs, VoiceChain};
use yeti_types::{DspParams, NoiseModel, Preset};

const SNR_DB: f64 = 5.0;
const MARGIN_MS: f64 = 200.0;

struct Fx {
    clean: Vec<f32>,
    noisy: Vec<f32>,
}

fn tts_fixture() -> Fx {
    let clean = fixtures::tts_speech().expect("tests/fixtures/speech_tts.wav");
    let noise = fixtures::keyboard_and_fan(clean.len() as f64 / fixtures::FS + 0.1, 21);
    let (noisy, _) = fixtures::mix_at_snr(&clean, &noise, SNR_DB);
    Fx { clean, noisy }
}

fn ns_only(model: NoiseModel) -> DspParams {
    let mut p = DspParams::for_preset(Preset::Gaming);
    p.noise.model = model;
    p.eq.enabled = false;
    p.auto_level.enabled = false;
    p.limiter.enabled = false;
    p
}

fn run(p: DspParams, fx: &Fx) -> (FixtureMetrics, usize, usize, ActiveNs) {
    let mut chain = VoiceChain::new(p).unwrap();
    let (out, _) = fixtures::render(&mut chain, &fx.noisy, None);
    let reported = chain.latency_frames();
    let reference = fixtures::highpassed(&fx.clean, p.high_pass);
    let m = fixtures::evaluate(&reference, &fx.noisy, &out, reported, MARGIN_MS);
    let start = fixtures::speech_mask(&fx.clean)
        .iter()
        .position(|v| *v)
        .unwrap();
    let measured = fixtures::estimate_delay(&reference, &out, start, 48_000, 3_000);
    (m, reported, measured, chain.active_noise_model())
}

fn check_ns_only(model: NoiseModel, want: ActiveNs) {
    let fx = tts_fixture();
    let (m, reported, measured, active) = run(ns_only(model), &fx);
    eprintln!("{model:?}: {m:?}, latency reported {reported} measured {measured}");
    assert_eq!(active, want);
    assert!(m.noise_frames > 200 && m.speech_frames > 500, "{m:?}");
    assert!(
        m.noise_reduction_db >= 15.0,
        "noise-only reduction {:.1} dB",
        m.noise_reduction_db
    );
    assert!(
        m.speech_level_delta_db.abs() <= 3.0,
        "speech level Δ {:.1} dB",
        m.speech_level_delta_db
    );
    assert!(
        m.si_sdr_out_db - m.si_sdr_in_db >= 10.0,
        "SI-SDR {:.1} → {:.1} dB",
        m.si_sdr_in_db,
        m.si_sdr_out_db
    );
    assert!(
        reported.abs_diff(measured) <= 24,
        "latency reported {reported} vs measured {measured}"
    );
}

#[test]
fn rnnoise_removes_keyboard_and_fan_keeps_speech() {
    check_ns_only(NoiseModel::RnNoise, ActiveNs::RnNoise);
}

#[cfg(feature = "dfn")]
#[test]
fn dfn_removes_keyboard_and_fan_keeps_speech() {
    check_ns_only(NoiseModel::DeepFilterNet, ActiveNs::DeepFilterNet);
}

/// Full presets: noise-only reduction still ≥ 15 dB after auto-level make-up gain, output
/// never above the −1 dBFS limiter ceiling.
#[test]
fn presets_keep_noise_down_and_respect_ceiling() {
    let fx = tts_fixture();
    let mut models = vec![NoiseModel::RnNoise];
    if VoiceChain::dfn_available() {
        models.push(NoiseModel::DeepFilterNet);
    }
    for preset in [Preset::Gaming, Preset::Meeting] {
        for &model in &models {
            let mut p = DspParams::for_preset(preset);
            p.noise.model = model;
            let mut chain = VoiceChain::new(p).unwrap();
            let (out, _) = fixtures::render(&mut chain, &fx.noisy, None);
            let reference = fixtures::highpassed(&fx.clean, p.high_pass);
            let m = fixtures::evaluate(
                &reference,
                &fx.noisy,
                &out,
                chain.latency_frames(),
                MARGIN_MS,
            );
            eprintln!("{preset:?}/{model:?}: {m:?}");
            assert!(m.noise_reduction_db >= 15.0, "{preset:?}/{model:?}: {m:?}");
            let peak = out.iter().fold(0f32, |a, v| a.max(v.abs()));
            assert!(peak <= 10f32.powf(-1.0 / 20.0) + 1e-6, "peak {peak}");
        }
    }
}

/// Phase 4 offline criterion: ERLE ≥ 15 dB on a far-end + near-end (echo) mix.
#[cfg(feature = "aec")]
#[test]
fn aec_erle_at_least_15db() {
    use yeti_dsp::aec::EchoCancel;
    use yeti_types::BLOCK_FRAMES;
    let far = fixtures::tts_speech().unwrap();
    let ir = fixtures::room_ir(60.0, 150.0, -6.0, 5);
    let echo = fixtures::convolve(&far, &ir);
    let mut rng = fixtures::Rng::new(99);
    let mic: Vec<f32> = echo
        .iter()
        .map(|e| e + (rng.bipolar() * 5.5e-4) as f32)
        .collect();
    let mut aec = EchoCancel::new().unwrap();
    let mut out = mic.clone();
    for (bi, blk) in out.chunks_mut(BLOCK_FRAMES).enumerate() {
        if blk.len() == BLOCK_FRAMES {
            aec.process_with_reference(blk, Some(&far[bi * BLOCK_FRAMES..(bi + 1) * BLOCK_FRAMES]));
        }
    }
    // ERLE over echo-active frames after 3 s convergence.
    let (mut em, mut eo) = (0f64, 0f64);
    let frames: Vec<f64> = echo
        .chunks(480)
        .map(|c| c.iter().map(|v| (*v as f64).powi(2)).sum())
        .collect();
    let max = frames.iter().cloned().fold(0.0, f64::max);
    for (fi, e) in frames.iter().enumerate() {
        let s = fi * 480;
        if s < 3 * 48_000 || *e < max * 10f64.powf(-3.5) || s + 480 > out.len() {
            continue;
        }
        for i in s..s + 480 {
            em += (mic[i] as f64).powi(2);
            eo += (out[i] as f64).powi(2);
        }
    }
    let erle = 10.0 * (em / eo.max(1e-20)).log10();
    eprintln!("ERLE {erle:.1} dB");
    assert!(erle >= 15.0, "ERLE {erle:.1} dB");
}

/// Meeting preset through the whole chain with a far-end reference: echo-only output is
/// far below the mic level.
#[cfg(feature = "aec")]
#[test]
fn meeting_chain_cancels_echo_with_reference() {
    let far = fixtures::tts_speech().unwrap();
    let echo = fixtures::convolve(&far, &fixtures::room_ir(40.0, 150.0, -6.0, 8));
    let mut chain = VoiceChain::new(DspParams::for_preset(Preset::Meeting)).unwrap();
    assert!(chain.echo_cancel_active());
    let (out, _) = fixtures::render(&mut chain, &echo, Some(&far));
    let skip = 3 * 48_000;
    let e_in: f64 = echo[skip..].iter().map(|v| (*v as f64).powi(2)).sum();
    let e_out: f64 = out[skip..echo.len()]
        .iter()
        .map(|v| (*v as f64).powi(2))
        .sum();
    let red = 10.0 * (e_in / e_out.max(1e-20)).log10();
    eprintln!("Meeting chain echo reduction {red:.1} dB");
    assert!(red >= 15.0, "echo reduction {red:.1} dB");
}
