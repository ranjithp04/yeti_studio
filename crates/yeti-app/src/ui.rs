//! View logic: Simple (A), Pro (B) and Mini (C) surfaces, bound to `Arc<dyn Engine>`.
//!
//! Platform-free (no tray / hotkeys / windows) so it can be driven by `egui_kittest`.
//! The desktop shell (`shell.rs`) owns windows, tray and hotkeys and forwards
//! [`Command`]s here; window-level requests from the UI go back via [`YetiUi::take_requests`].

use crate::config::AppConfig;
use crate::theme::{self, Palette};
use crate::widgets::{self, card, choice_card, muted, section_label, switch};
use eframe::egui::{self, Align, Layout, RichText, Ui, vec2};
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};
use yeti_engine::Engine;
use yeti_types::{
    AbState, AbVariant, DeviceCommand, DeviceEvent, DeviceState, DspParams, EngineEvent,
    EngineStats, EngineStatus, MeterSnapshot, MonitorSettings, NoiseModel, Preset, Theme, ViewMode,
    WaveformSnapshot,
};

/// Actions from tray, hotkeys or the UI itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    ToggleMute,
    /// Push-to-talk key pressed (`true`) / released (`false`).
    PushToTalk(bool),
    SetPreset(Preset),
    /// Show and focus the main window.
    ShowWindow,
    /// Hide the main window to the tray.
    HideWindow,
    ToggleWindow,
    ToggleMini,
    SetMini(bool),
    Quit,
}

/// First-run tip text (shown until dismissed).
pub const FIRST_RUN_TIP: &str = "Before you start: turn OFF noise suppression in Krisp, Zoom \
(Settings > Audio > Suppress background noise: Low/Off) and Microsoft Teams (Noise suppression: Off), \
then pick \"Yeti Studio Mic\" as the microphone there. Double processing makes voices sound robotic.";

const MAX_NOTICES: usize = 3;
/// Raw input peak at or above this counts as clipping.
const CLIP_DBFS: f32 = -0.5;
/// How long the clipping warning stays after the last clipped block.
const CLIP_HOLD: Duration = Duration::from_secs(3);

/// All UI state. Engine is the source of truth for DSP params, mute, monitor and device;
/// `config` holds UI preferences and is refreshed from the engine before saving.
pub struct YetiUi {
    engine: Arc<dyn Engine>,
    pub config: AppConfig,
    /// Main-window view (Simple or Pro).
    pub view: ViewMode,
    pub mini_open: bool,
    /// Tray icon exists (closing hides instead of quitting).
    pub tray_available: bool,
    /// Hotkey registration problems, shown in Pro view.
    pub hotkey_errors: Vec<String>,
    notices: VecDeque<String>,
    requests: Vec<Command>,
    dirty: bool,
    applied_theme: Option<Theme>,

    // --- per-frame snapshot from the engine ---
    params: DspParams,
    preset: Preset,
    monitor: MonitorSettings,
    meters: MeterSnapshot,
    stats: EngineStats,
    status: EngineStatus,
    device: Option<DeviceState>,
    ab: AbState,
    wave: WaveformSnapshot,
    aec_available: bool,

    // --- input safety + speech-only metrics ---
    /// Allow the digital gain slider above 0 dB (session only).
    gain_unlocked: bool,
    /// Show the clipping warning until this instant.
    clip_until: Option<Instant>,
    /// Noise reduction smoothed over frames where the user is talking.
    speech_nr_db: Option<f32>,
}

impl YetiUi {
    /// Apply the persisted settings to the engine and build the UI state.
    pub fn new(engine: Arc<dyn Engine>, config: AppConfig) -> Self {
        let mut params = config.params;
        params.output_muted = false;
        engine.set_params(params);
        engine.set_monitor(config.monitor);
        let mut s = Self {
            view: if config.view == ViewMode::Pro {
                ViewMode::Pro
            } else {
                ViewMode::Simple
            },
            mini_open: config.mini_open,
            tray_available: false,
            hotkey_errors: Vec::new(),
            notices: VecDeque::new(),
            requests: Vec::new(),
            dirty: false,
            applied_theme: None,
            params,
            preset: engine.preset(),
            monitor: config.monitor,
            meters: MeterSnapshot::default(),
            stats: EngineStats::default(),
            status: engine.status(),
            device: engine.device_state(),
            ab: AbState::Idle,
            wave: WaveformSnapshot::default(),
            aec_available: engine.aec_available(),
            gain_unlocked: false,
            clip_until: None,
            speech_nr_db: None,
            config,
            engine,
        };
        s.refresh();
        s
    }

    pub fn engine(&self) -> &Arc<dyn Engine> {
        &self.engine
    }

    pub fn is_muted(&self) -> bool {
        self.params.output_muted
    }

    pub fn preset(&self) -> Preset {
        self.preset
    }

    pub fn notices(&self) -> impl Iterator<Item = &str> {
        self.notices.iter().map(String::as_str)
    }

    pub fn push_notice(&mut self, msg: impl Into<String>) {
        let msg = msg.into();
        if self.notices.back() == Some(&msg) {
            return;
        }
        if self.notices.len() == MAX_NOTICES {
            self.notices.pop_front();
        }
        self.notices.push_back(msg);
    }

    /// Window-level requests made from inside the UI (e.g. "Mini window" link).
    pub fn take_requests(&mut self) -> Vec<Command> {
        std::mem::take(&mut self.requests)
    }

    /// Config snapshot to persist, if something changed since the last call.
    pub fn take_dirty_config(&mut self) -> Option<AppConfig> {
        if !self.dirty {
            return None;
        }
        self.dirty = false;
        Some(self.snapshot_config())
    }

    /// Current settings as a config (mute never persisted).
    pub fn snapshot_config(&self) -> AppConfig {
        let mut c = self.config.clone();
        c.params = self.engine.params();
        c.params.output_muted = false;
        c.preset = self.engine.preset();
        c.monitor = self.engine.monitor();
        c.view = self.view;
        c.mini_open = self.mini_open;
        c
    }

    pub fn mark_dirty(&mut self) {
        self.dirty = true;
    }

    /// Engine-related commands (window ones are handled by the shell).
    pub fn apply_command(&mut self, cmd: Command) {
        match cmd {
            Command::ToggleMute => self.set_muted(!self.engine.is_muted()),
            Command::PushToTalk(down) => self.set_muted(!down),
            Command::SetPreset(p) => self.set_preset(p),
            Command::ToggleMini => self.set_mini(!self.mini_open),
            Command::SetMini(open) => self.set_mini(open),
            Command::ShowWindow | Command::HideWindow | Command::ToggleWindow | Command::Quit => {}
        }
    }

    pub fn set_mini(&mut self, open: bool) {
        if self.mini_open != open {
            self.mini_open = open;
            self.dirty = true;
        }
    }

    pub fn set_muted(&mut self, muted: bool) {
        self.engine.set_muted(muted);
        self.params.output_muted = muted;
    }

    pub fn set_preset(&mut self, preset: Preset) {
        if preset == Preset::Custom {
            return;
        }
        self.engine.set_preset(preset);
        self.params = self.engine.params();
        self.preset = self.engine.preset();
        self.dirty = true;
    }

    pub fn set_theme(&mut self, theme: Theme) {
        if self.config.theme != theme {
            self.config.theme = theme;
            self.dirty = true;
        }
    }

    fn set_params(&mut self, p: DspParams) {
        if p != self.params {
            self.engine.set_params(p);
            self.params = p;
            self.preset = self.engine.preset();
            self.dirty = true;
        }
    }

    fn set_monitor(&mut self, m: MonitorSettings) {
        if m != self.monitor {
            self.engine.set_monitor(m);
            self.monitor = m;
            self.dirty = true;
        }
    }

    fn device_command(&mut self, cmd: DeviceCommand) {
        if let Err(e) = self.engine.device_command(cmd) {
            self.push_notice(format!("Yeti: {e}"));
        }
        self.device = self.engine.device_state();
    }

    /// Pull telemetry and drain engine events. Cheap; call once per frame.
    pub fn refresh(&mut self) {
        for ev in self.engine.poll_events() {
            match ev {
                EngineEvent::Status(s) => {
                    if let EngineStatus::Error(e) = &s {
                        self.push_notice(format!("Engine error: {e}"));
                    }
                    self.status = s;
                }
                EngineEvent::Device(DeviceEvent::Disconnected) => {
                    self.push_notice("Yeti disconnected - waiting for it to come back.");
                }
                EngineEvent::Device(_) => {}
                EngineEvent::AbReady => {}
                EngineEvent::Warning(w) => self.push_notice(w),
            }
        }
        self.params = self.engine.params();
        self.preset = self.engine.preset();
        self.monitor = self.engine.monitor();
        self.meters = self.engine.meters();
        self.stats = self.engine.stats();
        self.status = self.engine.status();
        self.device = self.engine.device_state();
        self.ab = self.engine.ab_state();
        self.engine.waveform(&mut self.wave);
        log_stats_if_enabled(&self.stats);
        self.update_input_safety();
    }

    /// Apply the theme preference to the context when it changed.
    pub fn apply_theme(&mut self, ctx: &egui::Context) {
        if self.applied_theme != Some(self.config.theme) {
            theme::install(ctx, self.config.theme);
            self.applied_theme = Some(self.config.theme);
        }
    }

    fn schedule_repaint(&self, ctx: &egui::Context) {
        // live meters / waveform at ~30 fps; idle otherwise
        let ms = if matches!(self.status, EngineStatus::Running) {
            33
        } else {
            250
        };
        ctx.request_repaint_after(Duration::from_millis(ms));
    }

    // ------------------------------------------------------------------
    // Main window
    // ------------------------------------------------------------------

    /// Draw the main window (Simple or Pro) into `ui`.
    pub fn show_main(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.apply_theme(&ctx);
        self.refresh();
        let p = Palette::current(&ctx);
        match self.view {
            ViewMode::Pro => self.show_pro(ui, p),
            _ => self.show_simple(ui, p),
        }
        self.schedule_repaint(&ctx);
    }

    fn status_text(&self) -> (String, bool) {
        let dev = self.device.as_ref();
        let name = dev
            .filter(|d| !d.name.is_empty())
            .map(|d| d.name.clone())
            .unwrap_or_else(|| "Blue Yeti".into());
        let rate = dev
            .and_then(|d| d.sample_rate)
            .map(|r| format!(" · {} kHz", fmt_khz(r)))
            .unwrap_or_default();
        match &self.status {
            EngineStatus::Running => (format!("{name}{rate} · connected"), true),
            EngineStatus::Starting => (format!("{name} · starting…"), false),
            EngineStatus::WaitingForDevice => ("Waiting for Blue Yeti - plug it in".into(), false),
            EngineStatus::Stopped => (format!("{name} · stopped"), false),
            EngineStatus::Error(e) => (format!("Error: {e}"), false),
        }
    }

    fn theme_switch(&mut self, ui: &mut Ui, p: &Palette) {
        egui::Frame::new()
            .fill(p.panel)
            .stroke(egui::Stroke::new(1.0, p.border))
            .corner_radius(10)
            .inner_margin(3)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.x = 2.0;
                ui.horizontal(|ui| {
                    for (t, label) in [
                        (Theme::Light, "Light"),
                        (Theme::Dark, "Dark"),
                        (Theme::System, "System"),
                    ] {
                        let sel = self.config.theme == t;
                        let btn = egui::Button::new(
                            RichText::new(label).size(12.0).strong().color(if sel {
                                p.text
                            } else {
                                p.muted
                            }),
                        )
                        .fill(if sel {
                            p.sel
                        } else {
                            egui::Color32::TRANSPARENT
                        })
                        .stroke(egui::Stroke::new(
                            1.0,
                            if sel {
                                p.accent
                            } else {
                                egui::Color32::TRANSPARENT
                            },
                        ))
                        .corner_radius(7)
                        .min_size(vec2(0.0, 26.0))
                        .selected(sel);
                        if ui.add(btn).clicked() {
                            self.set_theme(t);
                        }
                    }
                });
            });
    }

    fn notices_ui(&mut self, ui: &mut Ui, p: &Palette) {
        if self.notices.is_empty() {
            return;
        }
        let mut dismiss = false;
        card(p)
            .stroke(egui::Stroke::new(1.0, p.warn))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    for n in &self.notices {
                        ui.label(RichText::new(n).color(p.warn));
                    }
                    if ui.small_button("Dismiss").clicked() {
                        dismiss = true;
                    }
                });
            });
        if dismiss {
            self.notices.clear();
        }
    }

    fn tips_ui(&mut self, ui: &mut Ui, p: &Palette) {
        if self.config.tips_dismissed {
            return;
        }
        card(p)
            .stroke(egui::Stroke::new(1.0, p.accent))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("First-time setup").strong().color(p.text));
                ui.label(RichText::new(FIRST_RUN_TIP).size(13.0).color(p.soft));
                if ui.add(widgets::secondary_button(p, "Got it")).clicked() {
                    self.config.tips_dismissed = true;
                    self.dirty = true;
                }
            });
    }

    fn top_bar(&mut self, ui: &mut Ui, p: &Palette) {
        egui::Panel::top("top_bar")
            .exact_size(56.0)
            .frame(
                egui::Frame::new()
                    .fill(p.bar)
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .inner_margin(egui::Margin::symmetric(20, 0)),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), egui::Sense::hover());
                    widgets::mic_icon(ui, r, p.accent, false);
                    ui.label(
                        RichText::new("Yeti Studio")
                            .size(15.0)
                            .strong()
                            .color(p.text),
                    );
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        self.theme_switch(ui, p);
                        ui.add_space(12.0);
                        let (text, ok) = self.status_text();
                        ui.label(RichText::new(text).size(13.0).color(p.muted));
                        let (r, _) = ui.allocate_exact_size(vec2(8.0, 8.0), egui::Sense::hover());
                        ui.painter().circle_filled(
                            r.center(),
                            4.0,
                            if ok { p.accent } else { p.warn },
                        );
                    });
                });
            });
    }

    fn show_simple(&mut self, ui: &mut Ui, p: &Palette) {
        self.top_bar(ui, p);
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(24))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    self.notices_ui(ui, p);
                    self.tips_ui(ui, p);
                    let total = ui.available_width();
                    let right_w = 260.0;
                    let left_w = (total - right_w - 20.0).max(300.0);
                    ui.horizontal_top(|ui| {
                        ui.spacing_mut().item_spacing.x = 20.0;
                        ui.allocate_ui_with_layout(
                            vec2(left_w, 0.0),
                            Layout::top_down(Align::Min),
                            |ui| {
                                ui.set_width(left_w);
                                ui.spacing_mut().item_spacing.y = 20.0;
                                self.simple_left(ui, p);
                            },
                        );
                        ui.allocate_ui_with_layout(
                            vec2(right_w, 0.0),
                            Layout::top_down(Align::Min),
                            |ui| {
                                ui.set_width(right_w);
                                ui.spacing_mut().item_spacing.y = 12.0;
                                self.simple_right(ui, p);
                            },
                        );
                    });
                });
            });
    }

    fn preset_cards(&mut self, ui: &mut Ui, p: &Palette) {
        let w = ui.available_width();
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 12.0;
            let cw = (w - 12.0) / 2.0;
            for (preset, title, sub) in [
                (
                    Preset::Gaming,
                    "Gaming",
                    "Lowest delay · strong noise removal",
                ),
                (Preset::Meeting, "Meeting", "Echo cancel · even loudness"),
            ] {
                let sel = self.preset == preset;
                if choice_card(ui, p, sel, title, Some(sub), vec2(cw, 88.0)).clicked() {
                    self.set_preset(preset);
                }
            }
        });
        if self.preset == Preset::Custom {
            muted(
                ui,
                p,
                "Custom settings (changed in Advanced controls). Pick a preset to reset.",
            );
        }
    }

    fn simple_left(&mut self, ui: &mut Ui, p: &Palette) {
        self.preset_cards(ui, p);

        card(p).inner_margin(20).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new("What your team hears").size(14.0).strong());
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    muted(ui, p, "Grey = raw mic · blue = cleaned");
                });
            });
            widgets::waveform(ui, p, &self.wave.raw, &self.wave.cleaned, 120.0);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 16.0;
                muted(
                    ui,
                    p,
                    format!("Noise removed while talking: {}", self.speech_nr_text()),
                );
                muted(ui, p, format!("Delay: {:.0} ms", self.stats.latency_ms));
            });
        });

        card(p).inner_margin(20).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let mut pr = self.params;
            let mut pct = (pr.noise.strength * 100.0).round();
            let lbl = ui
                .horizontal(|ui| {
                    let l = ui.label(RichText::new("Noise removal strength").size(14.0).strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(format!("{pct:.0}%")).strong().color(p.accent));
                    });
                    l
                })
                .inner;
            ui.spacing_mut().slider_width = ui.available_width();
            let r = ui
                .add(egui::Slider::new(&mut pct, 0.0..=100.0).show_value(false))
                .labelled_by(lbl.id);
            if r.changed() {
                pr.noise.strength = pct / 100.0;
                self.set_params(pr);
            }
            ui.horizontal(|ui| {
                muted(ui, p, "Natural");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    muted(ui, p, "Voice only");
                });
            });
        });
    }

    fn mute_button(&mut self, ui: &mut Ui, p: &Palette, size: egui::Vec2) {
        let is_muted = self.params.output_muted;
        let label = if is_muted {
            "Muted · click to unmute"
        } else {
            "Mic live · click to mute"
        };
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::Button, true, is_muted, label));
        let stroke = if is_muted {
            egui::Stroke::new(2.0, p.warn)
        } else if resp.hovered() {
            egui::Stroke::new(1.0, p.accent)
        } else {
            egui::Stroke::new(1.0, p.border)
        };
        ui.painter()
            .rect(rect, 12, p.panel, stroke, egui::StrokeKind::Inside);
        let icon = egui::Rect::from_center_size(rect.center() - vec2(0.0, 14.0), vec2(36.0, 36.0));
        widgets::mic_icon(ui, icon, if is_muted { p.warn } else { p.accent }, is_muted);
        ui.painter().text(
            rect.center() + vec2(0.0, 26.0),
            egui::Align2::CENTER_CENTER,
            label,
            egui::FontId::proportional(15.0),
            p.text,
        );
        if resp
            .on_hover_cursor(egui::CursorIcon::PointingHand)
            .clicked()
        {
            self.set_muted(!is_muted);
        }
    }

    fn gain_slider(&mut self, ui: &mut Ui, p: &Palette, label: &str) {
        let dev = self.device.clone().unwrap_or_default();
        let (lo, hi) = dev.input_gain_range_db.unwrap_or((-12.0, 30.0));
        let mut g = dev.input_gain_db.unwrap_or(0.0);
        // Above 0 dB the Yeti clips easily: cap the slider unless the user unlocks it.
        // Keep a value already above the cap visible (it is shown, not changed).
        let hi = if self.gain_unlocked {
            hi
        } else {
            hi.min(0.0).max(g).max(lo)
        };
        let lbl = ui
            .horizontal(|ui| {
                let l = muted(ui, p, label);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let v = dev
                        .input_gain_db
                        .map(|g| format!("{g:+.1} dB"))
                        .unwrap_or_else(|| "n/a".into());
                    ui.label(RichText::new(v).size(13.0));
                });
                l
            })
            .inner;
        ui.spacing_mut().slider_width = ui.available_width();
        let enabled = dev.connected && dev.input_gain_db.is_some();
        let r = ui
            .add_enabled(
                enabled,
                egui::Slider::new(&mut g, lo..=hi)
                    .show_value(false)
                    .step_by(0.5),
            )
            .labelled_by(lbl.id);
        if r.changed() {
            self.device_command(DeviceCommand::SetInputGainDb(g));
        }
        if enabled && dev.input_gain_range_db.is_none_or(|(_, h)| h > 0.0) {
            ui.horizontal(|ui| {
                let mut unlocked = self.gain_unlocked;
                if ui
                    .checkbox(
                        &mut unlocked,
                        RichText::new("Allow boost above 0 dB")
                            .size(12.0)
                            .color(p.muted),
                    )
                    .changed()
                {
                    self.gain_unlocked = unlocked;
                }
            });
            if g > 0.0 {
                ui.label(
                    RichText::new(
                        "Digital boost on: turn the GAIN knob up instead to avoid clipping.",
                    )
                    .size(12.0)
                    .color(p.warn),
                );
            }
        }
    }

    /// Track input clipping (raw peak ≥ −0.5 dBFS) and speech-only noise reduction.
    fn update_input_safety(&mut self) {
        let m = &self.meters;
        if m.input_peak_db >= CLIP_DBFS {
            self.clip_until = Some(Instant::now() + CLIP_HOLD);
        }
        if m.gate_open && m.voice_probability >= 0.5 {
            let v = m.noise_reduction_db.max(0.0);
            self.speech_nr_db = Some(match self.speech_nr_db {
                Some(x) => x + 0.05 * (v - x),
                None => v,
            });
        }
    }

    fn is_clipping(&self) -> bool {
        self.clip_until.is_some_and(|t| Instant::now() < t)
    }

    fn clip_warning(&self, ui: &mut Ui, p: &Palette) {
        if self.is_clipping() {
            ui.label(
                RichText::new("Input clipping: turn the Yeti GAIN knob down")
                    .size(12.0)
                    .strong()
                    .color(p.warn),
            );
        }
    }

    fn speech_nr_text(&self) -> String {
        self.speech_nr_db
            .map(|v| format!("{v:.0} dB"))
            .unwrap_or_else(|| "–".into())
    }

    fn monitor_switch(&mut self, ui: &mut Ui, p: &Palette) {
        let mut m = self.monitor;
        ui.horizontal(|ui| {
            muted(ui, p, "Hear myself");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                switch(ui, p, &mut m.enabled, "Hear myself", true);
            });
        });
        self.set_monitor(m);
    }

    fn simple_right(&mut self, ui: &mut Ui, p: &Palette) {
        self.mute_button(ui, p, vec2(ui.available_width(), 120.0));

        card(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            muted(ui, p, "Input level");
            widgets::meter_bar(
                ui,
                p,
                self.meters
                    .input_rms_db
                    .max(self.meters.input_peak_db - 6.0),
                p.accent,
                10.0,
            );
            self.clip_warning(ui, p);
            self.gain_slider(ui, p, "Mic gain");
        });

        card(p).show(ui, |ui| {
            ui.set_width(ui.available_width());
            let hk = self.config.hotkeys.clone();
            for (label, key) in [
                ("Push-to-talk", &hk.push_to_talk),
                ("Mute toggle", &hk.mute_toggle),
            ] {
                ui.horizontal(|ui| {
                    muted(ui, p, label);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        widgets::keycap(ui, p, &hotkey_display(key));
                    });
                });
            }
            self.monitor_switch(ui, p);
        });

        ui.vertical_centered(|ui| {
            if ui
                .link(RichText::new("Advanced controls").color(p.accent))
                .clicked()
            {
                self.view = ViewMode::Pro;
                self.dirty = true;
            }
            let mini = if self.mini_open {
                "Close mini window"
            } else {
                "Mini window"
            };
            if ui
                .link(RichText::new(mini).size(12.5).color(p.muted))
                .clicked()
            {
                self.requests.push(Command::ToggleMini);
            }
        });
    }

    // ------------------------------------------------------------------
    // Pro view
    // ------------------------------------------------------------------

    fn show_pro(&mut self, ui: &mut Ui, p: &Palette) {
        egui::Panel::left("pro_sidebar")
            .exact_size(260.0)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(p.bar)
                    .stroke(egui::Stroke::new(1.0, p.border))
                    .inner_margin(20),
            )
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.pro_sidebar(ui, p));
            });
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(p.bg).inner_margin(20))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.pro_main(ui, p));
            });
    }

    fn pro_sidebar(&mut self, ui: &mut Ui, p: &Palette) {
        ui.spacing_mut().item_spacing.y = 10.0;
        if ui
            .add(widgets::secondary_button(p, "< Simple view"))
            .clicked()
        {
            self.view = ViewMode::Simple;
            self.dirty = true;
        }
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(20.0, 20.0), egui::Sense::hover());
            widgets::mic_icon(ui, r, p.accent, false);
            ui.label(RichText::new("Yeti Studio").size(15.0).strong());
        });
        ui.add_space(6.0);

        section_label(ui, p, "PRESET");
        let w = ui.available_width();
        for (preset, title) in [(Preset::Gaming, "Gaming"), (Preset::Meeting, "Meeting")] {
            if choice_card(ui, p, self.preset == preset, title, None, vec2(w, 40.0)).clicked() {
                self.set_preset(preset);
            }
        }
        if self.preset == Preset::Custom {
            choice_card(ui, p, true, "Custom", None, vec2(w, 40.0));
        }
        ui.add_space(6.0);

        section_label(ui, p, "YETI HARDWARE");
        self.gain_slider(ui, p, "Digital gain");
        let dev = self.device.clone().unwrap_or_default();
        if let Some(mut m) = dev.input_muted {
            ui.horizontal(|ui| {
                muted(ui, p, "Hardware mute");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if switch(ui, p, &mut m, "Hardware mute", dev.connected).changed() {
                        self.device_command(DeviceCommand::SetInputMuted(m));
                    }
                });
            });
        }
        let mut hv = dev.headphone_volume.unwrap_or(0.0) * 100.0;
        let hp_lbl = ui
            .horizontal(|ui| {
                let l = muted(ui, p, "Headphones");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let t = dev
                        .headphone_volume
                        .map(|v| format!("{:.0}%", v * 100.0))
                        .unwrap_or_else(|| "n/a".into());
                    ui.label(RichText::new(t).size(13.0));
                });
                l
            })
            .inner;
        ui.spacing_mut().slider_width = ui.available_width();
        if ui
            .add_enabled(
                dev.connected && dev.headphone_volume.is_some(),
                egui::Slider::new(&mut hv, 0.0..=100.0).show_value(false),
            )
            .labelled_by(hp_lbl.id)
            .changed()
        {
            self.device_command(DeviceCommand::SetHeadphoneVolume(hv / 100.0));
        }
        if let Some(mut hm) = dev.headphone_muted {
            ui.horizontal(|ui| {
                muted(ui, p, "Headphone mute");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if switch(ui, p, &mut hm, "Headphone mute", dev.connected).changed() {
                        self.device_command(DeviceCommand::SetHeadphoneMuted(hm));
                    }
                });
            });
        }
        ui.horizontal(|ui| {
            let sr_lbl = muted(ui, p, "Sample rate");
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let cur = dev.sample_rate;
                let cur_text = cur
                    .map(|r| format!("{} kHz", fmt_khz(r)))
                    .unwrap_or_else(|| "n/a".into());
                if dev.sample_rate_settable
                    && dev.connected
                    && !dev.available_sample_rates.is_empty()
                {
                    let mut chosen = None;
                    egui::ComboBox::from_id_salt("sample_rate")
                        .selected_text(cur_text)
                        .width(90.0)
                        .show_ui(ui, |ui| {
                            for r in &dev.available_sample_rates {
                                if ui
                                    .selectable_label(
                                        cur == Some(*r),
                                        format!("{} kHz", fmt_khz(*r)),
                                    )
                                    .clicked()
                                {
                                    chosen = Some(*r);
                                }
                            }
                        })
                        .response
                        .labelled_by(sr_lbl.id);
                    if let Some(r) = chosen.filter(|r| Some(*r) != cur) {
                        self.device_command(DeviceCommand::SetSampleRate(r));
                    }
                } else {
                    ui.label(RichText::new(cur_text).size(13.0));
                }
            });
        });
        card(p).inner_margin(10).corner_radius(8).show(ui, |ui| {
            ui.set_width(ui.available_width());
            para(ui, p, "Tip: set the pattern knob to Cardioid and talk into the front. The analog gain knob is not software-controllable.");
        });
        ui.add_space(10.0);

        let row = |ui: &mut Ui, k: &str, v: String| {
            ui.horizontal(|ui| {
                muted(ui, p, k);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(RichText::new(v).size(12.5));
                });
            });
        };
        row(ui, "Delay", format!("{:.0} ms", self.stats.latency_ms));
        row(ui, "CPU", format!("{:.1} %", self.stats.cpu_percent));
        row(ui, "Output", output_device_name().into());
        let c = self.stats.counters;
        row(
            ui,
            "Glitches",
            format!(
                "{}",
                c.stream_xruns + c.capture_overruns + c.output_underruns
            ),
        );
        ui.add_space(6.0);
        self.theme_switch(ui, p);
        if !self.hotkey_errors.is_empty() {
            for e in &self.hotkey_errors {
                ui.label(RichText::new(e).size(12.0).color(p.warn));
            }
        }
    }

    fn pro_main(&mut self, ui: &mut Ui, p: &Palette) {
        self.notices_ui(ui, p);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Voice chain").size(18.0).strong());
            muted(ui, p, "Runs top-left to bottom-right");
        });
        ui.add_space(4.0);

        let mut pr = self.params;
        let aec = self.aec_available;
        ui.columns(3, |cols| {
            stage_card(
                &mut cols[0],
                p,
                1,
                "Rumble filter",
                "Cuts wind, breath blasts, fan and desk thumps below the voice.",
                &mut pr.high_pass.enabled,
                true,
                |ui| {
                    slider_row(
                        ui,
                        p,
                        "Cutoff",
                        &mut pr.high_pass.cutoff_hz,
                        40.0..=200.0,
                        |v| format!("{v:.0} Hz"),
                    );
                },
            );
            stage_card(
                &mut cols[1],
                p,
                2,
                "AI noise removal",
                "Removes keyboard, mouse, fans and room noise.",
                &mut pr.noise.enabled,
                true,
                |ui| {
                    let mut pct = pr.noise.strength * 100.0;
                    if slider_row(ui, p, "Strength", &mut pct, 0.0..=100.0, |v| {
                        format!("{v:.0}%")
                    }) {
                        pr.noise.strength = pct / 100.0;
                    }
                    ui.horizontal(|ui| {
                        muted(ui, p, "Model");
                        ui.selectable_value(
                            &mut pr.noise.model,
                            NoiseModel::DeepFilterNet,
                            "DeepFilterNet",
                        );
                        ui.selectable_value(&mut pr.noise.model, NoiseModel::RnNoise, "RNNoise");
                    });
                },
            );
            stage_card(
                &mut cols[2],
                p,
                3,
                "Voice gate",
                "Silences the mic when you are not talking.",
                &mut pr.gate.enabled,
                true,
                |ui| {
                    slider_row(
                        ui,
                        p,
                        "Threshold",
                        &mut pr.gate.threshold_db,
                        -80.0..=-20.0,
                        |v| format!("{v:.0} dB"),
                    );
                    slider_row(
                        ui,
                        p,
                        "Release",
                        &mut pr.gate.release_ms,
                        50.0..=500.0,
                        |v| format!("{v:.0} ms"),
                    );
                },
            );
        });
        ui.columns(3, |cols| {
            stage_card(
                &mut cols[0],
                p,
                4,
                "Voice EQ + de-esser",
                "Clearer voice through game and meeting codecs.",
                &mut pr.eq.enabled,
                true,
                |ui| {
                    slider_row(
                        ui,
                        p,
                        "Clarity",
                        &mut pr.eq.presence_gain_db,
                        -6.0..=9.0,
                        |v| format!("{v:+.1} dB"),
                    );
                    let mut amt = pr.eq.deesser_amount * 100.0;
                    if slider_row(ui, p, "De-esser", &mut amt, 0.0..=100.0, |v| {
                        format!("{v:.0}%")
                    }) {
                        pr.eq.deesser_amount = amt / 100.0;
                        pr.eq.deesser_enabled = amt > 0.0;
                    }
                },
            );
            stage_card(
                &mut cols[1],
                p,
                5,
                "Auto-level",
                "Keeps whispers and shouts at the same loudness.",
                &mut pr.auto_level.enabled,
                true,
                |ui| {
                    slider_row(
                        ui,
                        p,
                        "Target",
                        &mut pr.auto_level.target_db,
                        -30.0..=-10.0,
                        |v| format!("{v:.0} dB"),
                    );
                    let mut f = pr.auto_level.firmness * 100.0;
                    if slider_row(ui, p, "Firmness", &mut f, 0.0..=100.0, |v| {
                        format!("{v:.0}%")
                    }) {
                        pr.auto_level.firmness = f / 100.0;
                    }
                },
            );
            let desc = if aec {
                "Removes speaker sound picked up by the mic. Meeting preset."
            } else {
                "Not available in this build (needs the `aec` feature)."
            };
            let ec_text = if pr.echo_cancel.enabled { "On" } else { "Off" };
            let desc = format!("{desc}\nStatus: {ec_text}");
            stage_card(
                &mut cols[2],
                p,
                6,
                "Echo cancel",
                &desc,
                &mut pr.echo_cancel.enabled,
                aec,
                |_ui| {},
            );
        });
        self.set_params(pr);

        ui.add_space(4.0);
        ui.columns(2, |cols| {
            card(p).show(&mut cols[0], |ui| {
                ui.set_width(ui.available_width());
                self.levels_ui(ui, p);
            });
            card(p).show(&mut cols[1], |ui| {
                ui.set_width(ui.available_width());
                self.ab_ui(ui, p);
            });
        });
    }

    fn levels_ui(&mut self, ui: &mut Ui, p: &Palette) {
        ui.label(RichText::new("Levels").size(14.0).strong());
        self.clip_warning(ui, p);
        let m = self.meters;
        for (label, db, color) in [
            ("Mic in", m.input_rms_db, p.raw_meter),
            ("Cleaned", m.output_rms_db, p.accent),
        ] {
            ui.horizontal(|ui| {
                ui.add_sized(
                    vec2(64.0, 14.0),
                    egui::Label::new(RichText::new(label).size(12.0).color(p.muted)),
                );
                let w = ui.available_width() - 64.0;
                ui.allocate_ui(vec2(w.max(40.0), 14.0), |ui| {
                    widgets::meter_bar(ui, p, db, color, 10.0);
                });
                ui.label(RichText::new(fmt_db(db)).size(12.0));
            });
        }
        muted(
            ui,
            p,
            format!(
                "Noise removed while talking: {} · Auto-level: {:+.1} dB · Gate: {} · Voice: {:.0}%",
                self.speech_nr_text(),
                m.gain_change_db,
                if m.gate_open { "open" } else { "closed" },
                m.voice_probability * 100.0
            ),
        );
        ui.separator();
        let mut mon = self.monitor;
        ui.horizontal(|ui| {
            muted(ui, p, "Hear myself");
            switch(ui, p, &mut mon.enabled, "Hear myself", true);
            let mut vol = mon.volume * 100.0;
            ui.spacing_mut().slider_width = (ui.available_width() - 50.0).max(60.0);
            let r = ui.add_enabled(
                mon.enabled,
                egui::Slider::new(&mut vol, 0.0..=100.0).show_value(false),
            );
            let vl = ui.label(RichText::new(format!("{vol:.0}%")).size(12.0));
            if r.labelled_by(vl.id).changed() {
                mon.volume = vol / 100.0;
            }
        });
        self.set_monitor(mon);
    }

    fn ab_ui(&mut self, ui: &mut Ui, p: &Palette) {
        ui.label(RichText::new("A/B test").size(14.0).strong());
        let secs = self.config.ab_seconds;
        para(
            ui,
            p,
            format!(
                "Record {secs:.0} s while typing and talking, then flip between raw and cleaned (plays on your Yeti headphones)."
            ),
        );
        let ab = self.ab;
        let recording = matches!(ab, AbState::Recording { .. });
        let have_take = matches!(ab, AbState::Ready { .. } | AbState::Playing { .. });
        let mut result = Ok(());
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !recording,
                    widgets::primary_button(p, &format!("Record {secs:.0} s")),
                )
                .clicked()
            {
                result = self.engine.ab_record(secs);
            }
            if ui
                .add_enabled(have_take, widgets::secondary_button(p, "Play raw"))
                .clicked()
            {
                result = self.engine.ab_play(AbVariant::Raw);
            }
            if ui
                .add_enabled(have_take, widgets::secondary_button(p, "Play cleaned"))
                .clicked()
            {
                result = self.engine.ab_play(AbVariant::Cleaned);
            }
            let can_stop = recording || matches!(ab, AbState::Playing { .. });
            if ui
                .add_enabled(can_stop, widgets::secondary_button(p, "Stop"))
                .clicked()
            {
                self.engine.ab_stop();
            }
        });
        if let Err(e) = result {
            self.push_notice(format!("A/B test: {e}"));
        }
        let ab = self.engine.ab_state();
        self.ab = ab;
        match ab {
            AbState::Idle => muted(ui, p, "No take recorded yet."),
            AbState::Recording { elapsed_s, total_s } => {
                ui.add(
                    egui::ProgressBar::new((elapsed_s / total_s).clamp(0.0, 1.0))
                        .fill(p.accent)
                        .desired_height(6.0),
                );
                muted(ui, p, format!("Recording… {elapsed_s:.1} / {total_s:.0} s"))
            }
            AbState::Ready { length_s } => muted(ui, p, format!("Take ready ({length_s:.0} s).")),
            AbState::Playing {
                variant,
                position_s,
            } => muted(
                ui,
                p,
                format!(
                    "Playing {} · {position_s:.1} s",
                    if variant == AbVariant::Raw {
                        "raw"
                    } else {
                        "cleaned"
                    }
                ),
            ),
        };
    }

    // ------------------------------------------------------------------
    // Mini window
    // ------------------------------------------------------------------

    /// Draw the Mini widget (always-on-top window content).
    pub fn show_mini(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.apply_theme(&ctx);
        let p = Palette::current(&ctx);
        let dark = ctx.theme() == egui::Theme::Dark;
        egui::CentralPanel::default()
            .frame(
                egui::Frame::new()
                    .fill(if dark { p.bg } else { p.bar })
                    .inner_margin(16),
            )
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 14.0;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Yeti Studio").size(13.0).strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let label = if dark {
                            "Switch to light mode"
                        } else {
                            "Switch to dark mode"
                        };
                        let btn = egui::Button::new(
                            RichText::new(if dark { "Light" } else { "Dark" })
                                .size(11.0)
                                .color(p.muted),
                        )
                        .fill(p.panel)
                        .stroke(egui::Stroke::new(1.0, p.border));
                        if ui.add(btn).on_hover_text(label).clicked() {
                            self.set_theme(if dark { Theme::Light } else { Theme::Dark });
                        }
                        if ui
                            .link(RichText::new("Open app").size(11.0).color(p.muted))
                            .clicked()
                        {
                            self.requests.push(Command::ShowWindow);
                        }
                    });
                });
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 14.0;
                    let is_muted = self.params.output_muted;
                    let label = if is_muted {
                        "Unmute microphone"
                    } else {
                        "Mute microphone"
                    };
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(64.0, 64.0), egui::Sense::click());
                    resp.widget_info(|| {
                        egui::WidgetInfo::selected(egui::Role::Button, true, is_muted, label)
                    });
                    let c = if is_muted { p.warn } else { p.accent };
                    ui.painter()
                        .circle(rect.center(), 31.0, p.sel, egui::Stroke::new(2.0, c));
                    widgets::mic_icon(ui, rect.shrink(18.0), c, is_muted);
                    if resp
                        .on_hover_cursor(egui::CursorIcon::PointingHand)
                        .clicked()
                    {
                        self.set_muted(!is_muted);
                    }
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 8.0;
                        let status = if is_muted {
                            "Muted"
                        } else if matches!(self.status, EngineStatus::Running) {
                            "Live · clean voice on"
                        } else {
                            "Not running"
                        };
                        ui.label(RichText::new(status).size(15.0).strong());
                        widgets::segment_meter(ui, p, self.meters.output_rms_db, 16);
                    });
                });
                let w = ui.available_width();
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    for (preset, title) in
                        [(Preset::Gaming, "Gaming"), (Preset::Meeting, "Meeting")]
                    {
                        if choice_card(
                            ui,
                            p,
                            self.preset == preset,
                            title,
                            None,
                            vec2((w - 8.0) / 2.0, 40.0),
                        )
                        .clicked()
                        {
                            self.set_preset(preset);
                        }
                    }
                });
            });
    }
}

/// One voice-chain card: number + switch, title, description, then the controls.
#[allow(clippy::too_many_arguments)]
fn stage_card(
    ui: &mut Ui,
    p: &Palette,
    n: u8,
    name: &str,
    desc: &str,
    enabled: &mut bool,
    available: bool,
    controls: impl FnOnce(&mut Ui),
) {
    // All voice-chain cards share the tallest card's height (measured last frame).
    let key = egui::Id::unique("yeti_stage_card_height");
    let frame = ui.ctx().cumulative_pass_nr();
    let (f0, cur0, last0) = ui
        .ctx()
        .data(|d| d.get_temp::<(u64, f32, f32)>(key))
        .unwrap_or((frame, 0.0, 160.0));
    let (cur, last) = if f0 == frame {
        (cur0, last0)
    } else {
        (0.0, cur0.max(160.0))
    };
    card(p).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.set_min_height(last);
        let h = ui
            .vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(n.to_string()).size(12.0).color(p.muted));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        switch(ui, p, enabled, name, available);
                    });
                });
                ui.label(RichText::new(name).size(15.0).strong());
                para(ui, p, desc);
                ui.add_enabled_ui(available && *enabled, controls);
            })
            .response
            .rect
            .height();
        ui.ctx()
            .data_mut(|d| d.insert_temp(key, (frame, cur.max(h), last)));
    });
}

/// Wrapped muted paragraph, left-aligned (not stretched to the full width).
fn para(ui: &mut Ui, p: &Palette, text: impl Into<String>) {
    ui.with_layout(Layout::top_down(Align::Min), |ui| {
        muted(ui, p, text);
    });
}

/// Label + value row with a full-width slider underneath. Returns `true` if changed.
fn slider_row(
    ui: &mut Ui,
    p: &Palette,
    label: &str,
    v: &mut f32,
    range: std::ops::RangeInclusive<f32>,
    fmt: impl Fn(f32) -> String,
) -> bool {
    let lbl = ui
        .horizontal(|ui| {
            let l = muted(ui, p, label);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(fmt(*v)).size(12.0));
            });
            l
        })
        .inner;
    ui.spacing_mut().slider_width = ui.available_width();
    ui.add(egui::Slider::new(v, range).show_value(false))
        .labelled_by(lbl.id)
        .changed()
}

fn fmt_khz(rate: u32) -> String {
    if rate.is_multiple_of(1000) {
        format!("{}", rate / 1000)
    } else {
        format!("{:.1}", rate as f32 / 1000.0)
    }
}

fn fmt_db(db: f32) -> String {
    if db <= yeti_types::METER_FLOOR_DB || !db.is_finite() {
        "silent".into()
    } else {
        format!("{db:.0} dB")
    }
}

/// Human-readable hotkey ("CmdOrCtrl+Shift+M" → "Cmd+Shift+M" on macOS). Empty → "Off".
pub fn hotkey_display(s: &str) -> String {
    if s.trim().is_empty() {
        return "Off".into();
    }
    let cmd = if cfg!(target_os = "macos") {
        "Cmd"
    } else {
        "Ctrl"
    };
    s.split('+')
        .map(|t| {
            let t = t.trim();
            match t.to_ascii_lowercase().as_str() {
                "cmdorctrl" | "commandorcontrol" | "commandorctrl" | "cmdorcontrol" => {
                    cmd.to_string()
                }
                "alt" | "option" if cfg!(target_os = "macos") => "Opt".to_string(),
                _ => t
                    .strip_prefix("Key")
                    .or_else(|| t.strip_prefix("Digit"))
                    .unwrap_or(t)
                    .to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("+")
}

/// Where the processed voice goes on this platform.
pub fn output_device_name() -> &'static str {
    if cfg!(target_os = "windows") {
        "CABLE Input (VB-CABLE)"
    } else {
        "Yeti Studio Mic"
    }
}

/// Diagnostics: with `YETI_STATS_LOG=1`, log engine stats/counters every 2 s (only while the UI refreshes).
fn log_stats_if_enabled(stats: &yeti_types::EngineStats) {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
    use std::time::Instant;
    static ENABLED: OnceLock<bool> = OnceLock::new();
    static START: OnceLock<Instant> = OnceLock::new();
    static LAST: AtomicU64 = AtomicU64::new(0);
    if !*ENABLED.get_or_init(|| std::env::var_os("YETI_STATS_LOG").is_some()) {
        return;
    }
    let secs = START.get_or_init(Instant::now).elapsed().as_secs();
    if secs >= LAST.load(Relaxed) + 2 {
        LAST.store(secs, Relaxed);
        log::info!(
            "stats t={secs}s lat={:.1}ms cpu={:.1}% drift={:.6} counters={:?}",
            stats.latency_ms,
            stats.cpu_percent,
            stats.drift_ratio,
            stats.counters
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_display_formats() {
        assert_eq!(hotkey_display(""), "Off");
        let s = hotkey_display("CmdOrCtrl+Shift+KeyM");
        assert!(s == "Cmd+Shift+M" || s == "Ctrl+Shift+M", "{s}");
    }

    fn ui() -> YetiUi {
        let e = std::sync::Arc::new(yeti_engine::MockEngine::new());
        YetiUi::new(e, AppConfig::default())
    }

    #[test]
    fn clipping_warning_holds_then_clears() {
        let mut u = ui();
        u.meters.input_peak_db = -6.0;
        u.update_input_safety();
        assert!(!u.is_clipping());
        u.meters.input_peak_db = -0.2;
        u.update_input_safety();
        assert!(u.is_clipping());
        u.clip_until = Some(Instant::now() - Duration::from_millis(1));
        assert!(!u.is_clipping());
    }

    #[test]
    fn noise_removed_only_counts_speech() {
        let mut u = ui();
        assert_eq!(u.speech_nr_text(), "–");
        // Gate closed (silence): huge reduction must be ignored.
        u.meters.gate_open = false;
        u.meters.voice_probability = 0.0;
        u.meters.noise_reduction_db = 80.0;
        u.update_input_safety();
        assert_eq!(u.speech_nr_text(), "–");
        // Talking: tracked.
        u.meters.gate_open = true;
        u.meters.voice_probability = 0.9;
        u.meters.noise_reduction_db = 6.0;
        u.update_input_safety();
        assert_eq!(u.speech_nr_text(), "6 dB");
    }

    #[test]
    fn khz() {
        assert_eq!(fmt_khz(48_000), "48");
        assert_eq!(fmt_khz(44_100), "44.1");
    }
}
