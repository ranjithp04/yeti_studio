//! egui_kittest view-logic tests: Simple / Pro / Mini against `MockEngine`.

use eframe::egui;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use std::sync::Arc;
use yeti_app::AppConfig;
use yeti_app::ui::{Command, YetiUi};
use yeti_engine::{Engine, EngineConfig, MockEngine};
use yeti_types::{AbState, Preset, Theme, ViewMode};

fn engine() -> Arc<MockEngine> {
    let e = Arc::new(MockEngine::new());
    e.start(EngineConfig::default()).unwrap();
    e
}

fn ui_state(e: &Arc<MockEngine>, cfg: AppConfig) -> YetiUi {
    let dyn_e: Arc<dyn Engine> = e.clone();
    YetiUi::new(dyn_e, cfg)
}

fn main_harness(state: YetiUi) -> Harness<'static, YetiUi> {
    Harness::builder()
        .with_size(egui::vec2(1200.0, 900.0))
        .build_ui_state(|ui, s: &mut YetiUi| s.show_main(ui), state)
}

fn mini_harness(state: YetiUi) -> Harness<'static, YetiUi> {
    Harness::builder()
        .with_size(egui::vec2(380.0, 240.0))
        .build_ui_state(|ui, s: &mut YetiUi| s.show_mini(ui), state)
}

/// Settle the UI: our views request repaints every ~33 ms (live meters), so `run()` would
/// never reach a steady state; a few fixed steps are enough for clicks to apply.
fn settle(h: &mut Harness<'_, YetiUi>) {
    h.run_steps(4);
}

#[test]
fn simple_is_default_and_advanced_toggles_pro_and_back() {
    let e = engine();
    let mut h = main_harness(ui_state(&e, AppConfig::default()));
    settle(&mut h);
    assert_eq!(h.state().view, ViewMode::Simple);
    h.get_by_label("What your team hears");
    h.get_by_label("Noise removal strength");

    h.get_by_label("Advanced controls").click();
    settle(&mut h);
    assert_eq!(h.state().view, ViewMode::Pro);
    h.get_by_label("Voice chain");
    for card in [
        "Rumble filter",
        "AI noise removal",
        "Voice gate",
        "Voice EQ + de-esser",
        "Auto-level",
        "Echo cancel",
    ] {
        assert!(
            !h.get_all_by_label(card).collect::<Vec<_>>().is_empty(),
            "missing card {card}"
        );
    }

    h.get_by_label("< Simple view").click();
    settle(&mut h);
    assert_eq!(h.state().view, ViewMode::Simple);
    // view change is persisted
    assert_eq!(
        h.state_mut().take_dirty_config().unwrap().view,
        ViewMode::Simple
    );
}

#[test]
fn preset_cards_drive_engine() {
    let e = engine();
    let mut h = main_harness(ui_state(&e, AppConfig::default()));
    settle(&mut h);
    assert_eq!(e.preset(), Preset::Gaming);
    h.get_by_label("Meeting").click();
    settle(&mut h);
    assert_eq!(e.preset(), Preset::Meeting);
    assert!(e.params().echo_cancel.enabled);
    h.get_by_label("Gaming").click();
    settle(&mut h);
    assert_eq!(e.preset(), Preset::Gaming);
    assert!(!e.params().echo_cancel.enabled);
}

#[test]
fn mute_button_toggles_engine_mute() {
    let e = engine();
    let mut h = main_harness(ui_state(&e, AppConfig::default()));
    settle(&mut h);
    assert!(!e.is_muted());
    h.get_by_label("Mic live · click to mute").click();
    settle(&mut h);
    assert!(e.is_muted());
    h.get_by_label("Muted · click to unmute").click();
    settle(&mut h);
    assert!(!e.is_muted());
}

#[test]
fn theme_switch_sets_config_and_egui_theme() {
    let e = engine();
    let mut h = main_harness(ui_state(&e, AppConfig::default()));
    settle(&mut h);
    assert_eq!(h.state().config.theme, Theme::System);

    h.get_by_label("Dark").click();
    settle(&mut h);
    assert_eq!(h.state().config.theme, Theme::Dark);
    assert_eq!(h.ctx.theme(), egui::Theme::Dark);
    assert_eq!(
        h.ctx.style_of(egui::Theme::Dark).visuals.panel_fill,
        egui::Color32::BLACK,
        "dark mode is pitch black"
    );

    h.get_by_label("Light").click();
    settle(&mut h);
    assert_eq!(h.state().config.theme, Theme::Light);
    assert_eq!(h.ctx.theme(), egui::Theme::Light);

    h.get_by_label("System").click();
    settle(&mut h);
    assert_eq!(h.state().config.theme, Theme::System);
    assert_eq!(
        h.ctx.options(|o| o.theme_preference),
        egui::ThemePreference::System
    );
}

#[test]
fn first_run_tips_shown_until_dismissed() {
    let e = engine();
    let mut h = main_harness(ui_state(&e, AppConfig::default()));
    settle(&mut h);
    h.get_by_label("First-time setup");
    assert!(h.query_by_label_contains("Krisp").is_some());
    h.get_by_label("Got it").click();
    settle(&mut h);
    assert!(h.state().config.tips_dismissed);
    assert!(h.query_by_label("First-time setup").is_none());

    // and stays dismissed on next start
    let cfg = h.state().snapshot_config();
    let h2 = main_harness(ui_state(&e, cfg));
    assert!(h2.query_by_label("First-time setup").is_none());
}

#[test]
fn pro_card_toggle_marks_custom_and_preset_resets() {
    let e = engine();
    let cfg = AppConfig {
        view: ViewMode::Pro,
        tips_dismissed: true,
        ..Default::default()
    };
    let mut h = main_harness(ui_state(&e, cfg));
    settle(&mut h);
    assert!(e.params().high_pass.enabled);
    // the switch carries the card name as its accessible label; the title label is the other node
    let switches: Vec<_> = h.get_all_by_label("Rumble filter").collect();
    assert_eq!(switches.len(), 2);
    h.get_by_role_and_label(egui::Role::CheckBox, "Rumble filter")
        .click();
    settle(&mut h);
    assert!(!e.params().high_pass.enabled);
    assert_eq!(e.preset(), Preset::Custom);
    h.get_by_label("Custom");

    h.get_by_label("Gaming").click();
    settle(&mut h);
    assert_eq!(e.preset(), Preset::Gaming);
    assert!(e.params().high_pass.enabled);
}

#[test]
fn hear_myself_switch_sets_monitor() {
    let e = engine();
    let mut h = main_harness(ui_state(
        &e,
        AppConfig {
            tips_dismissed: true,
            ..Default::default()
        },
    ));
    settle(&mut h);
    assert!(!e.monitor().enabled);
    h.get_by_role_and_label(egui::Role::CheckBox, "Hear myself")
        .click();
    settle(&mut h);
    assert!(e.monitor().enabled);
}

#[test]
fn ab_test_record_then_play() {
    let e = engine();
    let cfg = AppConfig {
        view: ViewMode::Pro,
        tips_dismissed: true,
        ab_seconds: 1.0,
        ..Default::default()
    };
    let mut h = main_harness(ui_state(&e, cfg));
    settle(&mut h);
    h.get_by_label("Record 1 s").click();
    settle(&mut h);
    assert!(matches!(e.ab_state(), AbState::Recording { .. }));
    std::thread::sleep(std::time::Duration::from_millis(1100));
    settle(&mut h);
    assert!(matches!(e.ab_state(), AbState::Ready { .. }));
    h.get_by_label("Play cleaned").click();
    settle(&mut h);
    assert!(matches!(
        e.ab_state(),
        AbState::Playing {
            variant: yeti_types::AbVariant::Cleaned,
            ..
        }
    ));
    h.get_by_label("Stop").click();
    settle(&mut h);
    assert!(matches!(e.ab_state(), AbState::Ready { .. }));
}

#[test]
fn mini_window_mute_preset_and_theme() {
    let e = engine();
    let cfg = AppConfig {
        theme: Theme::Dark,
        ..Default::default()
    };
    let mut h = mini_harness(ui_state(&e, cfg));
    settle(&mut h);
    h.get_by_label("Live · clean voice on");
    h.get_by_label("Mute microphone").click();
    settle(&mut h);
    assert!(e.is_muted());
    h.get_by_label("Muted");
    h.get_by_label("Unmute microphone").click();
    settle(&mut h);
    assert!(!e.is_muted());

    h.get_by_label("Meeting").click();
    settle(&mut h);
    assert_eq!(e.preset(), Preset::Meeting);

    h.get_by_label("Light").click(); // sun button in dark mode switches to light
    settle(&mut h);
    assert_eq!(h.state().config.theme, Theme::Light);

    h.get_by_label("Open app").click();
    settle(&mut h);
    assert_eq!(h.state_mut().take_requests(), vec![Command::ShowWindow]);
}

#[test]
fn mini_link_requests_mini_and_commands_apply() {
    let e = engine();
    let mut h = main_harness(ui_state(
        &e,
        AppConfig {
            tips_dismissed: true,
            ..Default::default()
        },
    ));
    settle(&mut h);
    h.get_by_label("Mini window").click();
    settle(&mut h);
    assert_eq!(h.state_mut().take_requests(), vec![Command::ToggleMini]);

    // tray / hotkey commands
    let s = h.state_mut();
    s.apply_command(Command::ToggleMini);
    assert!(s.mini_open);
    s.apply_command(Command::ToggleMute);
    assert!(e.is_muted());
    s.apply_command(Command::PushToTalk(true));
    assert!(!e.is_muted());
    s.apply_command(Command::PushToTalk(false));
    assert!(e.is_muted());
    s.apply_command(Command::SetPreset(Preset::Meeting));
    assert_eq!(e.preset(), Preset::Meeting);
    // mute is never persisted
    assert!(!s.snapshot_config().params.output_muted);
}

#[test]
fn hot_unplug_shows_waiting_status_and_notice() {
    let e = engine();
    let mut h = main_harness(ui_state(
        &e,
        AppConfig {
            tips_dismissed: true,
            ..Default::default()
        },
    ));
    settle(&mut h);
    assert!(h.query_by_label_contains("connected").is_some());
    e.simulate_connected(false);
    settle(&mut h);
    assert!(h.query_by_label_contains("Waiting for Blue Yeti").is_some());
    assert!(h.state().notices().any(|n| n.contains("disconnected")));
}
