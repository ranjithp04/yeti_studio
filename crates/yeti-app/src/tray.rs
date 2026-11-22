//! Tray / menu-bar icon (`tray-icon`). Must be created on the main thread **after** the event
//! loop started (eframe's app-creator callback satisfies both). Menu clicks are forwarded as
//! [`Command`]s through a channel and wake the UI with `request_repaint()`.

use crate::ui::Command;
use eframe::egui;
use std::sync::mpsc::Sender;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use yeti_types::Preset;

const ID_SHOW: &str = "show";
const ID_HIDE: &str = "hide";
const ID_MINI: &str = "mini";
const ID_MUTE: &str = "mute";
const ID_GAMING: &str = "gaming";
const ID_MEETING: &str = "meeting";
const ID_QUIT: &str = "quit";

/// Map a menu item id to a command.
pub fn command_for_menu_id(id: &str) -> Option<Command> {
    Some(match id {
        ID_SHOW => Command::ShowWindow,
        ID_HIDE => Command::HideWindow,
        ID_MINI => Command::ToggleMini,
        ID_MUTE => Command::ToggleMute,
        ID_GAMING => Command::SetPreset(Preset::Gaming),
        ID_MEETING => Command::SetPreset(Preset::Meeting),
        ID_QUIT => Command::Quit,
        _ => return None,
    })
}

/// Live tray icon + the check items we keep in sync with app state.
pub struct Tray {
    icon: TrayIcon,
    mini: CheckMenuItem,
    mute: CheckMenuItem,
    gaming: CheckMenuItem,
    meeting: CheckMenuItem,
}

impl Tray {
    /// Build the tray icon and install the menu-event handler.
    pub fn new(tx: Sender<Command>, ctx: egui::Context) -> Result<Self, String> {
        let show = MenuItem::with_id(ID_SHOW, "Show Yeti Studio", true, None);
        let hide = MenuItem::with_id(ID_HIDE, "Hide window", true, None);
        let mini = CheckMenuItem::with_id(ID_MINI, "Mini window", true, false, None);
        let mute = CheckMenuItem::with_id(ID_MUTE, "Mute mic", true, false, None);
        let gaming = CheckMenuItem::with_id(ID_GAMING, "Gaming preset", true, true, None);
        let meeting = CheckMenuItem::with_id(ID_MEETING, "Meeting preset", true, false, None);
        let quit = MenuItem::with_id(ID_QUIT, "Quit Yeti Studio", true, None);
        let menu = Menu::with_items(&[
            &show,
            &hide,
            &mini,
            &PredefinedMenuItem::separator(),
            &mute,
            &gaming,
            &meeting,
            &PredefinedMenuItem::separator(),
            &quit,
        ])
        .map_err(|e| e.to_string())?;

        MenuEvent::set_event_handler(Some(move |ev: MenuEvent| {
            if let Some(cmd) = command_for_menu_id(ev.id.as_ref()) {
                let _ = tx.send(cmd);
                ctx.request_repaint();
            }
        }));

        let icon = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_tooltip("Yeti Studio")
            .with_icon(tray_icon_image(false))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self {
            icon,
            mini,
            mute,
            gaming,
            meeting,
        })
    }

    /// Reflect app state in the menu (cheap; muda ignores unchanged values poorly, so guard).
    pub fn sync(&self, mini_open: bool, muted: bool, preset: Preset) {
        set_if(&self.mini, mini_open);
        if self.mute.is_checked() != muted {
            self.mute.set_checked(muted);
            let _ = self.icon.set_icon(Some(tray_icon_image(muted)));
        }
        set_if(&self.gaming, preset == Preset::Gaming);
        set_if(&self.meeting, preset == Preset::Meeting);
    }
}

fn set_if(item: &CheckMenuItem, v: bool) {
    if item.is_checked() != v {
        item.set_checked(v);
    }
}

/// 32x32 RGBA mic glyph: accent blue when live, amber with a slash when muted.
pub fn tray_icon_rgba(muted: bool) -> Vec<u8> {
    const N: i32 = 32;
    let (r, g, b) = if muted {
        (0xff, 0xb3, 0x40)
    } else {
        (0x3b, 0x9e, 0xff)
    };
    let mut px = vec![0u8; (N * N * 4) as usize];
    let mut put = |x: i32, y: i32| {
        if (0..N).contains(&x) && (0..N).contains(&y) {
            let i = ((y * N + x) * 4) as usize;
            px[i..i + 4].copy_from_slice(&[r, g, b, 0xff]);
        }
    };
    for y in 0..N {
        for x in 0..N {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            // capsule 12..20 x 3..19
            let cap = {
                let cy = fy.clamp(7.0, 15.0);
                ((fx - 16.0).powi(2) + (fy - cy).powi(2)).sqrt() <= 4.5
            };
            // U-shaped holder ring radius 9 around (16,15), lower half
            let d = ((fx - 16.0).powi(2) + (fy - 15.0).powi(2)).sqrt();
            let ring = fy >= 15.0 && (8.0..=10.0).contains(&d);
            let stem = (15.0..=17.0).contains(&fx) && (24.0..=28.0).contains(&fy);
            let base = (11.0..=21.0).contains(&fx) && (27.0..=29.0).contains(&fy);
            let slash = muted && (fx - fy).abs() <= 1.3;
            if cap || ring || stem || base || slash {
                put(x, y);
            }
        }
    }
    px
}

fn tray_icon_image(muted: bool) -> Icon {
    Icon::from_rgba(tray_icon_rgba(muted), 32, 32).expect("static 32x32 icon is valid")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn menu_ids_map_to_commands() {
        assert_eq!(command_for_menu_id("quit"), Some(Command::Quit));
        assert_eq!(command_for_menu_id("mini"), Some(Command::ToggleMini));
        assert_eq!(command_for_menu_id("show"), Some(Command::ShowWindow));
        assert_eq!(command_for_menu_id("nope"), None);
    }

    #[test]
    fn icon_has_pixels() {
        let px = tray_icon_rgba(false);
        assert_eq!(px.len(), 32 * 32 * 4);
        assert!(px.chunks(4).filter(|p| p[3] == 0xff).count() > 50);
    }
}
