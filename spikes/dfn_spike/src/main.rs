//! Phase 0 DFN spike: render data/noisy.wav (48 kHz mono) through DeepFilterNet (libDF, tract)
//! into data/enhanced.wav and print timing + RMS stats.
use df::tract::{DfParams, DfTract, RuntimeParams};
use ndarray::Array2;
use std::time::Instant;

fn rms(x: &[f32]) -> f32 {
    (x.iter().map(|v| v * v).sum::<f32>() / x.len().max(1) as f32).sqrt()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let inp = args.get(1).map(String::as_str).unwrap_or("data/noisy.wav");
    let out = args
        .get(2)
        .map(String::as_str)
        .unwrap_or("data/enhanced.wav");
    let mut r = hound::WavReader::open(inp).expect("open input");
    let spec = r.spec();
    assert_eq!(spec.sample_rate, 48_000, "spike expects 48 kHz");
    assert_eq!(spec.channels, 1, "spike expects mono");
    let input: Vec<f32> = r
        .samples::<i16>()
        .map(|s| s.unwrap() as f32 / 32768.0)
        .collect();

    let t_load = Instant::now();
    let mut model = DfTract::new(DfParams::default(), &RuntimeParams::default_with_ch(1))
        .expect("load DFN model");
    let load_ms = t_load.elapsed().as_secs_f64() * 1e3;
    let hop = model.hop_size;
    println!(
        "model loaded in {load_ms:.1} ms; sr={} hop={hop} lookahead={}",
        model.sr, model.lookahead
    );

    let mut output = Vec::with_capacity(input.len());
    let mut noisy = Array2::<f32>::zeros((1, hop));
    let mut enh = Array2::<f32>::zeros((1, hop));
    let t = Instant::now();
    let mut frames = 0usize;
    for chunk in input.chunks(hop) {
        noisy.fill(0.0);
        for (d, s) in noisy.iter_mut().zip(chunk) {
            *d = *s;
        }
        model
            .process(noisy.view(), enh.view_mut())
            .expect("process");
        output.extend(enh.iter().take(chunk.len()));
        frames += 1;
    }
    let proc_s = t.elapsed().as_secs_f64();
    let audio_s = input.len() as f64 / 48_000.0;
    println!(
        "processed {frames} frames ({audio_s:.2} s audio) in {:.1} ms -> RTF {:.3}, {:.3} ms/frame",
        proc_s * 1e3,
        proc_s / audio_s,
        proc_s * 1e3 / frames as f64
    );
    // Noise-only tail: last 1.5 s of the file (speech ends before ~6 s).
    let tail = input.len() - 72_000;
    println!(
        "RMS in: total {:.4} tail {:.4} | out: total {:.4} tail {:.4}",
        rms(&input),
        rms(&input[tail..]),
        rms(&output),
        rms(&output[tail..])
    );
    let mut w = hound::WavWriter::create(
        out,
        hound::WavSpec {
            channels: 1,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        },
    )
    .expect("create output");
    for s in &output {
        w.write_sample((s.clamp(-1.0, 1.0) * 32767.0) as i16)
            .unwrap();
    }
    w.finalize().unwrap();
    println!("wrote {out}");
}
