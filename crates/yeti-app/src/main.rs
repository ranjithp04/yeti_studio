//! Yeti Studio app entry point.
//!
//! Owner: **UI engineer** (Phase 3). The UI must depend only on `Arc<dyn yeti_engine::Engine>`;
//! pass `--mock` to run against [`yeti_engine::MockEngine`] (default until the real engine lands).

use std::sync::Arc;
use yeti_engine::{Engine, EngineConfig, MockEngine, YetiEngine};

fn main() {
    env_logger::init();
    let use_real = std::env::args().any(|a| a == "--real");
    let engine: Arc<dyn Engine> = if use_real {
        Arc::new(YetiEngine::new())
    } else {
        Arc::new(MockEngine::new())
    };
    if let Err(e) = engine.start(EngineConfig::default()) {
        eprintln!("engine start failed: {e}");
    }
    println!(
        "yeti-studio scaffold — engine status: {:?}, preset: {:?}, theme default: {:?}",
        engine.status(),
        engine.preset(),
        yeti_types::Theme::default()
    );
    println!("UI not implemented yet (Phase 3).");
    engine.stop();
}
