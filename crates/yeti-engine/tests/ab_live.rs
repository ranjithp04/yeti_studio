//! A/B recorder through the real engine with the synthetic test-tone input and no audio
//! outputs (no microphone, nothing audible). Builds the voice chain and real-time threads,
//! so it is ignored by default: `cargo test -p yeti-engine --test ab_live -- --ignored`.

use std::time::{Duration, Instant};
use yeti_engine::{Engine, EngineConfig, YetiEngine};
use yeti_types::{AbState, AbVariant, EngineEvent, EngineStatus};

fn wait_for(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let t = Instant::now();
    while !f() {
        assert!(t.elapsed() < Duration::from_secs(secs), "timed out: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
#[ignore]
fn ab_record_with_test_tone() {
    let e = YetiEngine::new();
    e.start(EngineConfig {
        input_device: Some("test-tone".into()),
        output_device: Some("none".into()),
        monitor_device: Some("none".into()),
        ..Default::default()
    })
    .unwrap();
    wait_for("running", 60, || e.status() == EngineStatus::Running);
    e.ab_record(1.0).unwrap();
    assert!(matches!(e.ab_state(), AbState::Recording { .. }));
    let mut ready_event = false;
    wait_for("take ready", 10, || {
        ready_event |= e.poll_events().contains(&EngineEvent::AbReady);
        matches!(e.ab_state(), AbState::Ready { .. })
    });
    assert_eq!(e.ab_state(), AbState::Ready { length_s: 1.0 });
    wait_for("AbReady event", 2, || {
        ready_event |= e.poll_events().contains(&EngineEvent::AbReady);
        ready_event
    });
    // No monitor device → playback is refused with an error, the take is kept.
    assert!(e.ab_play(AbVariant::Cleaned).is_err());
    assert_eq!(e.ab_state(), AbState::Ready { length_s: 1.0 });
    e.stop();
}
