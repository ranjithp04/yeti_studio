//! Global hotkeys (`global-hotkey`, keyboard only in v1). Created on the main thread after the
//! event loop started; the event handler forwards [`Command`]s and calls `request_repaint()` so
//! they are handled even while the window is hidden.

use crate::config::HotkeyConfig;
use crate::ui::Command;
use eframe::egui;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::collections::HashMap;
use std::sync::mpsc::Sender;

/// Which action a hotkey triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAction {
    MuteToggle,
    PushToTalk,
    ShowWindow,
    ToggleMini,
}

impl HotkeyAction {
    /// Command for a key event, or `None` if this action ignores that edge.
    pub fn command(self, pressed: bool) -> Option<Command> {
        match (self, pressed) {
            (Self::PushToTalk, down) => Some(Command::PushToTalk(down)),
            (Self::MuteToggle, true) => Some(Command::ToggleMute),
            (Self::ShowWindow, true) => Some(Command::ToggleWindow),
            (Self::ToggleMini, true) => Some(Command::ToggleMini),
            _ => None,
        }
    }
}

/// Parse the configured hotkeys. Empty strings are skipped; parse errors are returned
/// as human-readable messages.
pub fn parse(cfg: &HotkeyConfig) -> (Vec<(HotKey, HotkeyAction)>, Vec<String>) {
    let mut ok = Vec::new();
    let mut errs = Vec::new();
    for (s, action, name) in [
        (&cfg.mute_toggle, HotkeyAction::MuteToggle, "mute toggle"),
        (&cfg.push_to_talk, HotkeyAction::PushToTalk, "push-to-talk"),
        (&cfg.show_window, HotkeyAction::ShowWindow, "show window"),
        (&cfg.toggle_mini, HotkeyAction::ToggleMini, "mini window"),
    ] {
        if s.trim().is_empty() {
            continue;
        }
        match s.parse::<HotKey>() {
            Ok(hk) => ok.push((hk, action)),
            Err(e) => errs.push(format!("Hotkey {name} \"{s}\": {e}")),
        }
    }
    (ok, errs)
}

/// Registered hotkeys; unregistered when dropped (manager drop).
pub struct Hotkeys {
    _manager: GlobalHotKeyManager,
    pub registered: Vec<(HotKey, HotkeyAction)>,
}

impl Hotkeys {
    /// Register all configured hotkeys. Returns the live set plus per-key error messages
    /// (e.g. "already registered by another app").
    pub fn register(
        cfg: &HotkeyConfig,
        tx: Sender<Command>,
        ctx: egui::Context,
    ) -> Result<(Self, Vec<String>), String> {
        let manager = GlobalHotKeyManager::new().map_err(|e| e.to_string())?;
        let (parsed, mut errs) = parse(cfg);
        let mut registered = Vec::new();
        for (hk, action) in parsed {
            match manager.register(hk) {
                Ok(()) => registered.push((hk, action)),
                Err(e) => errs.push(format!("Hotkey {}: {e}", hk.into_string())),
            }
        }
        let map: HashMap<u32, HotkeyAction> =
            registered.iter().map(|(hk, a)| (hk.id(), *a)).collect();
        GlobalHotKeyEvent::set_event_handler(Some(move |ev: GlobalHotKeyEvent| {
            let Some(action) = map.get(&ev.id()) else {
                return;
            };
            if let Some(cmd) = action.command(ev.state() == HotKeyState::Pressed) {
                let _ = tx.send(cmd);
                ctx.request_repaint();
            }
        }));
        Ok((
            Self {
                _manager: manager,
                registered,
            },
            errs,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_parses_three_combos() {
        let (ok, errs) = parse(&HotkeyConfig::default());
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(ok.len(), 3);
        assert!(ok.iter().all(|(hk, _)| !hk.mods.is_empty()));
    }

    #[test]
    fn bad_hotkey_reports_error() {
        let cfg = HotkeyConfig {
            mute_toggle: "Ctrl+Nope".into(),
            ..Default::default()
        };
        let (_, errs) = parse(&cfg);
        assert_eq!(errs.len(), 1);
    }

    #[test]
    fn push_to_talk_uses_both_edges() {
        assert_eq!(
            HotkeyAction::PushToTalk.command(true),
            Some(Command::PushToTalk(true))
        );
        assert_eq!(
            HotkeyAction::PushToTalk.command(false),
            Some(Command::PushToTalk(false))
        );
        assert_eq!(HotkeyAction::MuteToggle.command(false), None);
    }
}
