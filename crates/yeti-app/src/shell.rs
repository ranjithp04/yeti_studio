//! Desktop shell: eframe `App` that owns the windows, tray icon and global hotkeys.
//!
//! Close-to-tray behaviour (PLAN.md Phase 3):
//! - Closing the **main window** hides it to the tray / menu bar (if a tray icon exists and
//!   `close_to_tray` is on). Audio keeps running. Without a tray icon, closing quits.
//! - Closing the **Mini window** closes only the Mini window.
//! - **Quit** (tray menu, or `--quit-after`) saves the config, stops the engine and exits.
//! - "Show Yeti Studio" (tray) / show-window hotkey brings the main window back.

use crate::config::AppConfig;
use crate::hotkeys::Hotkeys;
use crate::tray::Tray;
use crate::ui::{Command, YetiUi};
use eframe::egui::{self, ViewportBuilder, ViewportCommand, ViewportId};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};
use yeti_engine::Engine;

/// Title of the Mini window (also used by the window-level check script).
pub const MINI_TITLE: &str = "Yeti Studio Mini";
pub const MAIN_TITLE: &str = "Yeti Studio";
const SAVE_DEBOUNCE: Duration = Duration::from_millis(800);

/// Startup options (from the command line).
#[derive(Debug, Clone, Default)]
pub struct ShellOptions {
    /// Where the config lives (`None` = don't persist).
    pub config_path: Option<PathBuf>,
    /// Start with the main window hidden (tray only).
    pub start_hidden: bool,
    /// Open the Mini window at start (overrides config).
    pub open_mini: bool,
    /// Quit automatically after this long (smoke tests); goes through the normal Quit path.
    pub quit_after: Option<Duration>,
    pub no_tray: bool,
    pub no_hotkeys: bool,
}

pub fn mini_viewport_id() -> ViewportId {
    ViewportId::from_hash_of("yeti_studio_mini")
}

/// Mini window: small, always on top (NSFloatingWindowLevel on macOS / HWND_TOPMOST on Windows).
pub fn mini_viewport_builder() -> ViewportBuilder {
    ViewportBuilder::default()
        .with_title(MINI_TITLE)
        .with_inner_size([380.0, 240.0])
        .with_min_inner_size([340.0, 220.0])
        .with_resizable(false)
        .with_always_on_top()
}

pub struct DesktopApp {
    ui: YetiUi,
    rx: Receiver<Command>,
    tray: Option<Tray>,
    _hotkeys: Option<Hotkeys>,
    opts: ShellOptions,
    started: Instant,
    window_visible: bool,
    first_frame: bool,
    quitting: bool,
    pending_save: Option<(AppConfig, Instant)>,
}

impl DesktopApp {
    /// Called from eframe's app-creator: main thread, event loop running.
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        engine: Arc<dyn Engine>,
        config: AppConfig,
        opts: ShellOptions,
    ) -> Self {
        let ctx = cc.egui_ctx.clone();
        let mut ui = YetiUi::new(engine, config);
        ui.apply_theme(&ctx);
        if opts.open_mini {
            ui.mini_open = true;
        }
        let (tx, rx) = channel();

        let tray = if opts.no_tray {
            None
        } else {
            match Tray::new(tx.clone(), ctx.clone()) {
                Ok(t) => Some(t),
                Err(e) => {
                    log::warn!("tray icon unavailable: {e}");
                    ui.push_notice(format!(
                        "Tray icon unavailable ({e}); closing the window will quit."
                    ));
                    None
                }
            }
        };
        ui.tray_available = tray.is_some();

        let hotkeys = if opts.no_hotkeys {
            None
        } else {
            match Hotkeys::register(&ui.config.hotkeys, tx, ctx) {
                Ok((h, errs)) => {
                    for e in &errs {
                        log::warn!("{e}");
                    }
                    ui.hotkey_errors = errs;
                    Some(h)
                }
                Err(e) => {
                    log::warn!("global hotkeys unavailable: {e}");
                    ui.hotkey_errors = vec![format!("Global hotkeys unavailable: {e}")];
                    None
                }
            }
        };

        Self {
            ui,
            rx,
            tray,
            _hotkeys: hotkeys,
            opts,
            started: Instant::now(),
            window_visible: true,
            first_frame: true,
            quitting: false,
            pending_save: None,
        }
    }

    fn set_window_visible(&mut self, ctx: &egui::Context, visible: bool) {
        self.window_visible = visible;
        ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Visible(visible));
        if visible {
            ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Focus);
        }
        ctx.request_repaint();
    }

    fn handle(&mut self, ctx: &egui::Context, cmd: Command) {
        match cmd {
            Command::ShowWindow => self.set_window_visible(ctx, true),
            Command::HideWindow => {
                if self.tray.is_some() {
                    self.set_window_visible(ctx, false);
                }
            }
            Command::ToggleWindow => {
                let v = !self.window_visible || self.tray.is_none();
                self.set_window_visible(ctx, v);
            }
            Command::Quit => self.quit(ctx),
            other => {
                let was_open = self.ui.mini_open;
                self.ui.apply_command(other);
                if self.ui.mini_open && !was_open && !self.window_visible {
                    // The Mini viewport is created from the root's ui pass, which eframe skips
                    // while the root is hidden: ask for one pass so the Mini window appears.
                    ctx.send_viewport_cmd_to(
                        ViewportId::ROOT,
                        ViewportCommand::RequestPaintWhileHidden,
                    );
                }
                ctx.request_repaint();
            }
        }
    }

    fn quit(&mut self, ctx: &egui::Context) {
        if self.quitting {
            return;
        }
        log::info!("quit requested");
        self.quitting = true;
        self.save_now();
        ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::Close);
        ctx.request_repaint();
    }

    fn save_now(&mut self) {
        self.pending_save = None;
        let cfg = self.ui.snapshot_config();
        if let Some(path) = &self.opts.config_path
            && let Err(e) = cfg.save(path)
        {
            log::warn!("saving config: {e}");
        }
    }

    fn autosave(&mut self) {
        if let Some(cfg) = self.ui.take_dirty_config() {
            self.pending_save = Some((cfg, Instant::now()));
        }
        if let Some((_, since)) = &self.pending_save
            && since.elapsed() >= SAVE_DEBOUNCE
        {
            self.save_now();
        }
    }
}

impl eframe::App for DesktopApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.first_frame {
            self.first_frame = false;
            if self.opts.start_hidden && self.tray.is_some() {
                self.set_window_visible(ctx, false);
                if self.ui.mini_open {
                    ctx.send_viewport_cmd_to(
                        ViewportId::ROOT,
                        ViewportCommand::RequestPaintWhileHidden,
                    );
                }
            }
        }
        if let Some(after) = self.opts.quit_after {
            if self.started.elapsed() >= after {
                self.quit(ctx);
            } else {
                ctx.request_repaint_after(after.saturating_sub(self.started.elapsed()));
            }
        }

        while let Ok(cmd) = self.rx.try_recv() {
            self.handle(ctx, cmd);
        }

        // Close button on the main window → hide to tray (unless quitting / no tray).
        if ctx.input(|i| i.viewport().close_requested()) && !self.quitting {
            if self.tray.is_some() && self.ui.config.close_to_tray {
                ctx.send_viewport_cmd_to(ViewportId::ROOT, ViewportCommand::CancelClose);
                self.set_window_visible(ctx, false);
            } else {
                self.quitting = true;
                self.save_now();
            }
        }

        if let Some(tray) = &self.tray {
            tray.sync(self.ui.mini_open, self.ui.is_muted(), self.ui.preset());
        }
        self.autosave();
        if self.pending_save.is_some() {
            ctx.request_repaint_after(SAVE_DEBOUNCE);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.ui.show_main(ui);

        if self.ui.mini_open {
            let mut close = false;
            let yui = &mut self.ui;
            ctx.show_viewport_immediate(
                mini_viewport_id(),
                mini_viewport_builder(),
                |ui, _class| {
                    yui.show_mini(ui);
                    if ui.input(|i| i.viewport().close_requested()) {
                        close = true;
                    }
                },
            );
            if close {
                self.ui.set_mini(false);
            }
        }

        for cmd in self.ui.take_requests() {
            self.handle(&ctx, cmd);
        }
    }

    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }

    fn on_exit(&mut self) {
        self.save_now();
        let e = self.ui.engine();
        log::info!(
            "engine at exit: status {:?}, meters {:?}, stats {:?}",
            e.status(),
            e.meters(),
            e.stats()
        );
        e.stop();
        log::info!("exited cleanly");
    }
}
