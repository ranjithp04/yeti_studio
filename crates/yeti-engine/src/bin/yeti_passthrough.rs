//! `yeti-passthrough`: run the real engine for N seconds and print counters, RMS, drift.
//!
//! ```text
//! yeti-passthrough [--seconds N] [--input NAME] [--output NAME|yeti|auto|default|none] [--audible]
//!                  [--monitor] [--bypass] [--reference] [--buffer FRAMES]
//!                  [--preset gaming|meeting] [--ns dfn|rnnoise] [--probe]
//! ```
//! Defaults: 10 s, Gaming preset, output = Yeti headphone jack with **software mute on** (the
//! full graph, rings and drift bridge run, but silence is written) so nothing can feed back
//! into the mic. `--audible` unmutes. Exit code 0 only if xruns (after the first 2 s)/overruns/underruns are all zero.
//!
//! `--probe`: live end-to-end latency measurement (see `yeti_engine::probe`). The mic signal is
//! replaced by an impulse train, the voice chain still runs (CPU), the output plays silence,
//! and the impulse's host-clock capture → playback time is reported, plus the chain's
//! algorithmic delay. CPU % is the DSP worker's share of one core (chain + resamplers).

use std::time::{Duration, Instant};
use yeti_engine::{Engine, EngineConfig, YetiEngine};
use yeti_types::EngineStatus;

#[cfg(debug_assertions)]
#[global_allocator]
static A: assert_no_alloc::AllocDisabler = assert_no_alloc::AllocDisabler;

fn arg(name: &str) -> Option<String> {
    let a: Vec<String> = std::env::args().collect();
    a.iter()
        .position(|x| x == name)
        .and_then(|i| a.get(i + 1).cloned())
}
/// Seconds after start during which stream xruns count as startup glitches.
const STARTUP_S: f64 = 2.0;

fn flag(name: &str) -> bool {
    std::env::args().any(|x| x == name)
}

fn main() {
    env_logger_init();
    let secs: f64 = arg("--seconds")
        .and_then(|s| s.parse().ok())
        .unwrap_or(10.0);
    let output = match arg("--output").as_deref() {
        None | Some("yeti") => Some(
            yeti_engine::pipeline::find_yeti_name(&cpal::default_host(), false)
                .unwrap_or_else(|| yeti_device::YETI_NAME.to_string()),
        ),
        // Automatic routing (as the app): virtual mic, or nothing if it is not installed.
        Some("auto") => None,
        Some(o) => Some(o.to_string()),
    };
    let cfg = EngineConfig {
        input_device: arg("--input"),
        output_device: output,
        monitor_device: if flag("--monitor") {
            None
        } else {
            Some("none".into())
        },
        buffer_frames: arg("--buffer").and_then(|s| s.parse().ok()),
        capture_reference: flag("--reference").then_some(true),
    };
    let e = YetiEngine::new();
    let preset = match arg("--preset").as_deref() {
        None | Some("gaming") => yeti_types::Preset::Gaming,
        Some("meeting") => yeti_types::Preset::Meeting,
        Some(o) => {
            eprintln!("unknown --preset {o:?} (gaming|meeting)");
            std::process::exit(2);
        }
    };
    let mut params = yeti_types::DspParams::for_preset(preset);
    match arg("--ns").as_deref() {
        None => {}
        Some("dfn") => params.noise.model = yeti_types::NoiseModel::DeepFilterNet,
        Some("rnnoise") => params.noise.model = yeti_types::NoiseModel::RnNoise,
        Some(o) => {
            eprintln!("unknown --ns {o:?} (dfn|rnnoise)");
            std::process::exit(2);
        }
    }
    e.set_params(params);
    e.set_bypass_chain(flag("--bypass"));
    e.set_muted(!flag("--audible"));
    let probe = flag("--probe");
    e.set_latency_probe(probe);
    println!(
        "preset {preset:?}, noise model {:?}, echo cancel {}, probe {probe}",
        params.noise.model, params.echo_cancel.enabled
    );
    if flag("--monitor") {
        e.set_monitor(yeti_types::MonitorSettings {
            enabled: true,
            volume: 0.5,
        });
    }
    println!(
        "device: {:?}",
        e.device_state()
            .map(|d| (d.name, d.sample_rate, d.input_gain_db))
    );
    e.start(cfg).expect("start");

    let t0 = Instant::now();
    while e.status() != EngineStatus::Running && t0.elapsed() < Duration::from_secs(90) {
        if let EngineStatus::Error(m) = e.status() {
            println!("engine error: {m}");
            std::process::exit(2);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    for ev in e.poll_events() {
        println!("event: {ev:?}");
    }
    if e.status() != EngineStatus::Running {
        println!("engine not running: {:?}", e.status());
        std::process::exit(2);
    }
    let d = e.diagnostics();
    println!("pipeline: {:#?}", d.info);
    let start = Instant::now();
    let mut max_rms = yeti_types::METER_FLOOR_DB;
    let mut sum_lin = 0.0f64;
    let mut n = 0u64;
    // xruns while the streams start (first STARTUP_S) are reported separately: a single
    // glitch when WASAPI/CoreAudio opens a stream is expected and not a steady-state fault.
    let mut startup_xruns: Option<u64> = None;
    while start.elapsed().as_secs_f64() < secs {
        std::thread::sleep(Duration::from_millis(100));
        if startup_xruns.is_none() && start.elapsed().as_secs_f64() >= STARTUP_S {
            startup_xruns = Some(e.stats().counters.stream_xruns);
        }
        let m = e.meters();
        max_rms = max_rms.max(m.input_rms_db);
        sum_lin += 10f64.powf(m.input_rms_db as f64 / 10.0);
        n += 1;
        if n.is_multiple_of(10) {
            let s = e.stats();
            let d = e.diagnostics();
            println!(
                "t={:>5.1}s in rms {:>6.1} dB peak {:>6.1} dB | out rms {:>6.1} | blocks {:>5} xrun {} ovr {} undr {} ref_undr {} out_ovr {} resync {} | drift {:.6} fill cap {:>5.0} out {:>5.0} | lat {:>5.1} ms cpu {:>4.1}%{}",
                start.elapsed().as_secs_f64(),
                m.input_rms_db,
                m.input_peak_db,
                m.output_rms_db,
                s.counters.blocks_processed,
                s.counters.stream_xruns,
                s.counters.capture_overruns,
                s.counters.output_underruns,
                s.counters.reference_underruns,
                d.output_overruns,
                d.output_resyncs,
                s.drift_ratio,
                d.capture_fill_frames,
                d.output_fill_frames,
                s.latency_ms,
                s.cpu_percent,
                if d.reference_active {
                    format!(
                        " | ref rms {:.1} dB max peak {:.1} dB drift {:.6} fill {:.0}",
                        d.reference_rms_db,
                        d.reference_max_peak_db,
                        d.reference_drift_ratio,
                        d.reference_fill_frames
                    )
                } else {
                    String::new()
                }
            );
        }
        for ev in e.poll_events() {
            println!("event: {ev:?}");
        }
    }
    let s = e.stats();
    let d = e.diagnostics();
    e.stop();
    std::thread::sleep(Duration::from_millis(300));
    let mean_rms = if n > 0 {
        10.0 * (sum_lin / n as f64).log10()
    } else {
        -90.0
    };
    println!("\n== SUMMARY ({secs:.0} s) ==");
    println!(
        "blocks processed     : {} (expected ~{:.0})",
        s.counters.blocks_processed,
        secs * 100.0
    );
    let startup = startup_xruns.unwrap_or(s.counters.stream_xruns);
    let steady_xruns = s.counters.stream_xruns.saturating_sub(startup);
    println!(
        "cpal xruns           : {steady_xruns} after startup ({startup} during the first {STARTUP_S:.0} s, ignored)"
    );
    println!("capture overruns     : {}", s.counters.capture_overruns);
    println!("output underruns     : {}", s.counters.output_underruns);
    println!("output overruns      : {}", d.output_overruns);
    println!(
        "output ring resyncs  : {} (startup fill excluded; expected 0)",
        d.output_resyncs
    );
    println!("reference underruns  : {}", s.counters.reference_underruns);
    println!("input RMS mean / max : {mean_rms:.1} / {max_rms:.1} dBFS");
    println!(
        "input max peak       : {:.1} dBFS ({})",
        d.max_input_peak_db,
        if d.max_input_peak_db > -89.0 {
            "non-silent data captured"
        } else {
            "SILENCE (no mic permission?)"
        }
    );
    println!("drift ratio (final)  : {:.6}", s.drift_ratio);
    println!(
        "latency est. / cpu   : {:.1} ms / {:.2} %",
        s.latency_ms, s.cpu_percent
    );
    if probe {
        let p = e.latency_probe();
        let chain = s.dsp_latency_ms as f64;
        println!(
            "MEASURED transport   : mean {:.1} ms, min {:.1}, max {:.1} (n={}, lost {})",
            p.mean_ms, p.min_ms, p.max_ms, p.count, p.lost
        );
        println!(
            "MEASURED end-to-end  : {:.1} ms (= transport {:.1} + chain algorithmic {:.1})",
            p.mean_ms + chain,
            p.mean_ms,
            chain
        );
    }
    if d.reference_active || flag("--reference") {
        println!(
            "reference max peak   : {:.1} dBFS (excludes own process: {})",
            d.reference_max_peak_db, d.reference_excludes_self
        );
    }
    let clean = steady_xruns == 0
        && s.counters.capture_overruns == 0
        && s.counters.output_underruns == 0
        && d.output_overruns == 0;
    println!(
        "RESULT: counters {}",
        if clean { "all zero" } else { "NON-ZERO" }
    );
    std::process::exit(if clean { 0 } else { 1 });
}

fn env_logger_init() {
    // Minimal stderr logger without extra deps: only warnings/errors.
    struct L;
    impl log::Log for L {
        fn enabled(&self, m: &log::Metadata) -> bool {
            m.level() <= log::Level::Info
        }
        fn log(&self, r: &log::Record) {
            if self.enabled(r.metadata()) {
                eprintln!("[{}] {}", r.level(), r.args());
            }
        }
        fn flush(&self) {}
    }
    static LOGGER: L = L;
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(log::LevelFilter::Info);
}
