// Release builds on Windows: GUI subsystem (no console window next to the app).
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
//! Yeti Studio app entry point.
//!
//! The UI depends only on `Arc<dyn yeti_engine::Engine>`. Default engine: the real
//! [`YetiEngine`]; pass `--mock` for [`MockEngine`] (UI development without audio).
//!
//! Flags: `--mock` (`--real` is accepted and is the default), `--mini` (open Mini window), `--hidden` (start in tray),
//! `--quit-after <secs>` (smoke test; normal Quit path), `--config <path>`,
//! `--no-tray`, `--no-hotkeys`, and engine routing overrides `--input NAME|test-tone`,
//! `--output NAME|default|none`, `--monitor NAME|default|none` (see `EngineConfig`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use yeti_app::shell::{MAIN_TITLE, ShellOptions};
use yeti_app::{AppConfig, DesktopApp};
use yeti_engine::{Engine, EngineConfig, MockEngine, YetiEngine};

fn main() -> eframe::Result<()> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let mut args = std::env::args().skip(1);
    let mut use_mock = false;
    let mut engine_cfg = EngineConfig::default();
    let mut opts = ShellOptions {
        config_path: AppConfig::default_path(),
        ..Default::default()
    };
    while let Some(a) = args.next() {
        match a.as_str() {
            "--mock" => use_mock = true,
            "--real" => use_mock = false,
            "--mini" => opts.open_mini = true,
            "--hidden" => opts.start_hidden = true,
            "--no-tray" => opts.no_tray = true,
            "--no-hotkeys" => opts.no_hotkeys = true,
            "--quit-after" => {
                let secs: f64 = args
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or_else(|| usage("--quit-after needs seconds"));
                opts.quit_after = Some(Duration::from_secs_f64(secs));
            }
            "--config" => {
                opts.config_path = Some(PathBuf::from(
                    args.next()
                        .unwrap_or_else(|| usage("--config needs a path")),
                ));
            }
            "--input" | "--output" | "--monitor" => {
                let v = args
                    .next()
                    .unwrap_or_else(|| usage(&format!("{a} needs a device name")));
                match a.as_str() {
                    "--input" => engine_cfg.input_device = Some(v),
                    "--output" => engine_cfg.output_device = Some(v),
                    _ => engine_cfg.monitor_device = Some(v),
                }
            }
            "-h" | "--help" => usage(""),
            other => usage(&format!("unknown argument {other}")),
        }
    }

    let config = opts
        .config_path
        .as_deref()
        .map(AppConfig::load_or_default)
        .unwrap_or_default();
    log::info!(
        "config: {}",
        opts.config_path
            .as_deref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "(none)".into())
    );

    let engine: Arc<dyn Engine> = if use_mock {
        log::info!("using MockEngine (--mock): no audio");
        Arc::new(MockEngine::new())
    } else {
        Arc::new(YetiEngine::new())
    };
    if let Err(e) = engine.start(engine_cfg) {
        log::warn!("engine start failed: {e}");
    }

    let native = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_title(MAIN_TITLE)
            .with_app_id("yeti-studio")
            // Fixed size per view: the UI fits the height to the current view's content.
            .with_inner_size([yeti_app::ui::MAIN_WIDTH, 640.0])
            .with_resizable(false),
        ..Default::default()
    };
    eframe::run_native(
        MAIN_TITLE,
        native,
        Box::new(move |cc| Ok(Box::new(DesktopApp::new(cc, engine, config, opts)))),
    )
}

fn usage(err: &str) -> ! {
    if !err.is_empty() {
        eprintln!("error: {err}");
    }
    eprintln!(
        "usage: yeti-studio [--mock] [--mini] [--hidden] [--quit-after SECS] [--config PATH] [--no-tray] [--no-hotkeys]\n       \
         [--input NAME|test-tone] [--output NAME|default|none] [--monitor NAME|default|none]"
    );
    std::process::exit(if err.is_empty() { 0 } else { 2 })
}
