//! Offline renderer: run a WAV through the Yeti Studio voice chain.
//!
//! ```text
//! yeti-render IN.wav OUT.wav [--preset gaming|meeting] [--ns dfn|rnnoise|off]
//!             [--strength 0..1] [--far FAR.wav] [--keep-latency] [--float]
//! ```
//! - Input: any channel count (averaged to mono), int or float, **48 kHz** (resample first,
//!   e.g. `ffmpeg -i in.wav -ar 48000 in48.wav`).
//! - `--far`: far-end (loudspeaker) reference for echo cancel (Meeting, feature `aec`).
//! - Output: 48 kHz mono, 16-bit PCM (or 32-bit float with `--float`), time-aligned with the
//!   input (chain latency removed) unless `--keep-latency`.

use std::path::Path;
use std::process::ExitCode;
use yeti_dsp::{VoiceChain, fixtures};
use yeti_types::{DspParams, ENGINE_SAMPLE_RATE, NoiseModel, Preset};

const USAGE: &str = "usage: yeti-render IN.wav OUT.wav [--preset gaming|meeting] [--ns dfn|rnnoise|off] [--strength 0..1] [--far FAR.wav] [--keep-latency] [--float]";

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("yeti-render: {e}");
            ExitCode::FAILURE
        }
    }
}

fn read_48k(path: &str) -> Result<Vec<f32>, String> {
    let (x, sr) = fixtures::read_wav_mono(Path::new(path)).map_err(|e| format!("{path}: {e}"))?;
    if sr != ENGINE_SAMPLE_RATE {
        return Err(format!(
            "{path}: sample rate {sr} Hz; the chain runs at 48 kHz. Resample first: ffmpeg -i {path} -ar 48000 OUT.wav"
        ));
    }
    Ok(x)
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    let mut pos = Vec::new();
    let mut preset = Preset::Gaming;
    let mut ns: Option<&str> = None;
    let mut strength: Option<f32> = None;
    let mut far_path: Option<String> = None;
    let (mut keep_latency, mut float) = (false, false);
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut val = || {
            it.next()
                .cloned()
                .ok_or_else(|| format!("{a} needs a value\n{USAGE}"))
        };
        match a.as_str() {
            "--preset" => {
                preset = match val()?.as_str() {
                    "gaming" => Preset::Gaming,
                    "meeting" => Preset::Meeting,
                    o => return Err(format!("unknown preset '{o}' (gaming|meeting)")),
                }
            }
            "--ns" => {
                ns = Some(match val()?.as_str() {
                    "dfn" => "dfn",
                    "rnnoise" => "rnnoise",
                    "off" => "off",
                    o => return Err(format!("unknown --ns '{o}' (dfn|rnnoise|off)")),
                })
            }
            "--strength" => {
                let v: f32 = val()?
                    .parse()
                    .map_err(|_| "--strength needs a number".to_string())?;
                strength = Some(v.clamp(0.0, 1.0));
            }
            "--far" => far_path = Some(val()?),
            "--keep-latency" => keep_latency = true,
            "--float" => float = true,
            s if s.starts_with("--") => return Err(format!("unknown option {s}\n{USAGE}")),
            _ => pos.push(a.clone()),
        }
    }
    let [inp, outp] = pos.as_slice() else {
        return Err(USAGE.to_string());
    };

    let mut p = DspParams::for_preset(preset);
    match ns {
        Some("dfn") => p.noise.model = NoiseModel::DeepFilterNet,
        Some("rnnoise") => p.noise.model = NoiseModel::RnNoise,
        Some(_) => p.noise.enabled = false,
        None => {}
    }
    if let Some(s) = strength {
        p.noise.strength = s;
    }

    let input = read_48k(inp)?;
    let far = far_path.as_deref().map(read_48k).transpose()?;
    if p.echo_cancel.enabled && far.is_none() {
        eprintln!("note: echo cancel is on but no --far reference was given (AEC sees silence)");
    }
    if p.echo_cancel.enabled && !VoiceChain::aec_available() {
        eprintln!("note: built without the `aec` feature; echo cancel skipped");
    }

    let t = std::time::Instant::now();
    let mut chain = VoiceChain::new(p).map_err(|e| e.to_string())?;
    let load_ms = t.elapsed().as_secs_f64() * 1e3;
    if ns == Some("dfn") && chain.active_noise_model() != yeti_dsp::ActiveNs::DeepFilterNet {
        eprintln!(
            "note: DFN unavailable ({}), using RNNoise",
            chain.dfn_error().unwrap_or("?")
        );
    }
    let (out, times) = fixtures::render(&mut chain, &input, far.as_deref());
    let lat = chain.latency_frames();
    let start = if keep_latency { 0 } else { lat };
    let end = if keep_latency {
        out.len()
    } else {
        (lat + input.len()).min(out.len())
    };
    fixtures::write_wav_mono(Path::new(outp), &out[start..end], float)
        .map_err(|e| format!("{outp}: {e}"))?;

    let total: f64 = times.iter().sum();
    let max = times.iter().cloned().fold(0.0, f64::max);
    let audio_s = input.len() as f64 / ENGINE_SAMPLE_RATE as f64;
    eprintln!(
        "{inp} → {outp}: {audio_s:.2} s, preset {preset:?}, NS {:?}, AEC {}, latency {:.1} ms, \
         init {load_ms:.0} ms, process {:.0} ms (RTF {:.3}, mean {:.0} µs / max {:.0} µs per 10 ms block)",
        chain.active_noise_model(),
        chain.echo_cancel_active(),
        chain.latency_ms(),
        total * 1e3,
        total / audio_s,
        total / times.len() as f64 * 1e6,
        max * 1e6
    );
    Ok(())
}
