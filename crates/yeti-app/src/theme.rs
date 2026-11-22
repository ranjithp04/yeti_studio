//! Colour palette (from the approved mockups, PLAN.md §2) and egui visuals.
//!
//! Dark = pitch black `#000000` with blue accent `#3b9eff`; Light = `#f4f6f9` with `#1667d9`.
//! `Theme::System` maps to `egui::ThemePreference::System`, which eframe resolves from the OS
//! appearance (winit theme, updated live when the OS switches).

use eframe::egui::{self, Color32, CornerRadius, Stroke};
use yeti_types::Theme;

/// Mockup colour tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub bg: Color32,
    pub bar: Color32,
    pub panel: Color32,
    /// Selected card / button background.
    pub sel: Color32,
    pub border: Color32,
    pub border2: Color32,
    pub text: Color32,
    pub soft: Color32,
    pub muted: Color32,
    pub track: Color32,
    pub knob: Color32,
    /// Raw-mic bars in the waveform.
    pub raw_wave: Color32,
    /// Raw-mic meter fill (Pro levels).
    pub raw_meter: Color32,
    /// Toggle track when off.
    pub toggle_off: Color32,
    pub accent: Color32,
    pub on_accent: Color32,
    pub warn: Color32,
}

const fn hex(rgb: u32) -> Color32 {
    Color32::from_rgb((rgb >> 16) as u8, (rgb >> 8) as u8, rgb as u8)
}

pub const DARK: Palette = Palette {
    bg: hex(0x000000),
    bar: hex(0x000000),
    panel: hex(0x0c0d10),
    sel: hex(0x04162b),
    border: hex(0x1c1f25),
    border2: hex(0x2c323b),
    text: hex(0xf2f4f7),
    soft: hex(0xc4cad1),
    muted: hex(0x9aa3ad),
    track: hex(0x22262d),
    knob: hex(0xc4cad1),
    raw_wave: hex(0x2c323b),
    raw_meter: hex(0x5a626d),
    toggle_off: hex(0x2c323b),
    accent: hex(0x3b9eff),
    on_accent: hex(0x000814),
    warn: hex(0xffb340),
};

pub const LIGHT: Palette = Palette {
    bg: hex(0xf4f6f9),
    bar: hex(0xffffff),
    panel: hex(0xffffff),
    sel: hex(0xe6f0ff),
    border: hex(0xdde2e8),
    border2: hex(0xc5ccd5),
    text: hex(0x0f141a),
    soft: hex(0x3a434e),
    muted: hex(0x5a6472),
    track: hex(0xd5dbe2),
    knob: hex(0xffffff),
    raw_wave: hex(0xc5ccd5),
    raw_meter: hex(0x8a939d),
    toggle_off: hex(0xb9c1cb),
    accent: hex(0x1667d9),
    on_accent: hex(0xffffff),
    warn: hex(0x9a5b00),
};

impl Palette {
    pub fn of(theme: egui::Theme) -> &'static Palette {
        match theme {
            egui::Theme::Dark => &DARK,
            egui::Theme::Light => &LIGHT,
        }
    }

    /// Palette for the theme egui is currently rendering with (System already resolved).
    pub fn current(ctx: &egui::Context) -> &'static Palette {
        Self::of(ctx.theme())
    }
}

/// Map the persisted setting to egui's preference.
pub fn preference(theme: Theme) -> egui::ThemePreference {
    match theme {
        Theme::Light => egui::ThemePreference::Light,
        Theme::Dark => egui::ThemePreference::Dark,
        Theme::System => egui::ThemePreference::System,
    }
}

/// egui visuals built from a palette.
pub fn visuals(theme: egui::Theme) -> egui::Visuals {
    let p = Palette::of(theme);
    let mut v = match theme {
        egui::Theme::Dark => egui::Visuals::dark(),
        egui::Theme::Light => egui::Visuals::light(),
    };
    v.panel_fill = p.bg;
    v.window_fill = p.panel;
    v.window_stroke = Stroke::new(1.0, p.border);
    v.extreme_bg_color = p.track;
    v.faint_bg_color = p.panel;
    v.code_bg_color = p.panel;
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.accent;
    v.selection.stroke = Stroke::new(1.0, p.on_accent);
    v.slider_trailing_fill = true;
    v.warn_fg_color = p.warn;

    let r = CornerRadius::same(8);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = p.panel;
    w.noninteractive.weak_bg_fill = p.panel;
    w.noninteractive.bg_stroke = Stroke::new(1.0, p.border);
    w.noninteractive.fg_stroke = Stroke::new(1.0, p.text);
    w.noninteractive.corner_radius = r;

    w.inactive.bg_fill = p.track;
    w.inactive.weak_bg_fill = p.panel;
    w.inactive.bg_stroke = Stroke::new(1.0, p.border2);
    w.inactive.fg_stroke = Stroke::new(1.0, p.text);
    w.inactive.corner_radius = r;

    w.hovered.bg_fill = p.track;
    w.hovered.weak_bg_fill = p.sel;
    w.hovered.bg_stroke = Stroke::new(1.0, p.accent);
    w.hovered.fg_stroke = Stroke::new(1.5, p.text);
    w.hovered.corner_radius = r;

    w.active.bg_fill = p.accent;
    w.active.weak_bg_fill = p.sel;
    w.active.bg_stroke = Stroke::new(1.0, p.accent);
    w.active.fg_stroke = Stroke::new(2.0, p.text);
    w.active.corner_radius = r;

    w.open.weak_bg_fill = p.sel;
    w.open.bg_stroke = Stroke::new(1.0, p.accent);
    w.open.corner_radius = r;
    v
}

/// Install both palettes into the context (call once) and apply the preference.
pub fn install(ctx: &egui::Context, theme: Theme) {
    ctx.set_visuals_of(egui::Theme::Dark, visuals(egui::Theme::Dark));
    ctx.set_visuals_of(egui::Theme::Light, visuals(egui::Theme::Light));
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.button_padding = egui::vec2(12.0, 6.0);
        s.spacing.slider_width = 200.0;
    });
    ctx.set_theme(preference(theme));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_matches_plan() {
        assert_eq!(DARK.bg, Color32::BLACK);
        assert_eq!(DARK.accent, Color32::from_rgb(0x3b, 0x9e, 0xff));
        assert_eq!(LIGHT.accent, Color32::from_rgb(0x16, 0x67, 0xd9));
    }

    #[test]
    fn install_sets_pitch_black_dark_panel() {
        let ctx = egui::Context::default();
        install(&ctx, Theme::Dark);
        assert_eq!(ctx.theme(), egui::Theme::Dark);
        assert_eq!(
            ctx.style_of(egui::Theme::Dark).visuals.panel_fill,
            Color32::BLACK
        );
        install(&ctx, Theme::Light);
        assert_eq!(ctx.theme(), egui::Theme::Light);
    }
}
