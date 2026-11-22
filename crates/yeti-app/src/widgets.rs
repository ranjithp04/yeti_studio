//! Small custom widgets drawn with the mockup palette: cards, switches, meters, waveform, icons.
//! Every interactive widget reports an accessibility label (used by egui_kittest tests).

use crate::theme::Palette;
use eframe::egui::{
    self, Align2, Color32, CornerRadius, FontId, Margin, Pos2, Rect, Response, Role, Sense, Stroke,
    StrokeKind, Ui, Vec2, WidgetInfo, pos2, vec2,
};
use yeti_types::METER_FLOOR_DB;

/// Rounded panel card (`panel` fill, `border` stroke, radius 12).
pub fn card(p: &Palette) -> egui::Frame {
    egui::Frame::new()
        .fill(p.panel)
        .stroke(Stroke::new(1.0, p.border))
        .corner_radius(CornerRadius::same(12))
        .inner_margin(Margin::same(16))
}

/// Small uppercase section heading ("PRESET", "YETI HARDWARE").
pub fn section_label(ui: &mut Ui, p: &Palette, text: &str) {
    ui.label(egui::RichText::new(text).size(12.0).strong().color(p.muted));
}

/// Muted helper text.
pub fn muted(ui: &mut Ui, p: &Palette, text: impl Into<String>) -> Response {
    ui.label(egui::RichText::new(text.into()).size(12.5).color(p.muted))
}

/// iOS-style switch (36x20). Returns the response; flips `*on` when clicked.
pub fn switch(ui: &mut Ui, p: &Palette, on: &mut bool, label: &str, enabled: bool) -> Response {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(36.0, 20.0), Sense::click());
    if enabled && resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let state = *on;
    resp.widget_info(|| WidgetInfo::selected(Role::CheckBox, enabled, state, label));
    let t = ui.ctx().animate_bool_responsive(resp.id, state);
    let track = if state { p.accent } else { p.toggle_off };
    let alpha = if enabled { 1.0 } else { 0.4 };
    ui.painter()
        .rect_filled(rect, CornerRadius::same(10), track.gamma_multiply(alpha));
    let x = egui::lerp(rect.left() + 10.0..=rect.right() - 10.0, t);
    let knob = if state { Color32::WHITE } else { p.knob };
    ui.painter()
        .circle_filled(pos2(x, rect.center().y), 8.0, knob.gamma_multiply(alpha));
    if enabled {
        resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Horizontal level meter: dBFS mapped linearly from `METER_FLOOR_DB`..0 (display range −60..0).
pub fn meter_bar(ui: &mut Ui, p: &Palette, db: f32, fill: Color32, height: f32) -> Response {
    let w = ui.available_width().max(40.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, height), Sense::hover());
    let r = CornerRadius::same((height / 2.0) as u8);
    ui.painter().rect_filled(rect, r, p.track);
    let frac = db_to_frac(db);
    if frac > 0.0 {
        let mut f = rect;
        f.set_width(rect.width() * frac);
        ui.painter().rect_filled(f, r, fill);
    }
    resp
}

/// Map dBFS to 0..=1 for meters (−60 dB → 0, 0 dB → 1).
pub fn db_to_frac(db: f32) -> f32 {
    if !db.is_finite() || db <= METER_FLOOR_DB {
        return 0.0;
    }
    ((db + 60.0) / 60.0).clamp(0.0, 1.0)
}

/// Segmented meter (Mini window): `n` segments, lit up to the level.
pub fn segment_meter(ui: &mut Ui, p: &Palette, db: f32, n: usize) {
    let w = ui.available_width().max(60.0);
    let (rect, _) = ui.allocate_exact_size(vec2(w, 14.0), Sense::hover());
    let gap = 3.0;
    let seg_w = (rect.width() - gap * (n as f32 - 1.0)) / n as f32;
    let lit = (db_to_frac(db) * n as f32).round() as usize;
    for i in 0..n {
        let x = rect.left() + i as f32 * (seg_w + gap);
        let r = Rect::from_min_size(pos2(x, rect.top()), vec2(seg_w, rect.height()));
        let c = if i < lit { p.accent } else { p.track };
        ui.painter().rect_filled(r, CornerRadius::same(2), c);
    }
}

/// Raw-vs-cleaned waveform: grey bars = raw peak, accent bars = cleaned peak, mirrored around
/// the centre line. `raw`/`cleaned` are equal-length peak buckets (0..=1), oldest first;
/// they are decimated to at most `max_bars` bars.
pub fn waveform(ui: &mut Ui, p: &Palette, raw: &[f32], cleaned: &[f32], height: f32) -> Response {
    let w = ui.available_width().max(60.0);
    let (rect, resp) = ui.allocate_exact_size(vec2(w, height), Sense::hover());
    let painter = ui.painter_at(rect);
    let n = raw.len().min(cleaned.len());
    if n == 0 {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            "No audio yet",
            FontId::proportional(13.0),
            p.muted,
        );
        return resp;
    }
    let gap = 3.0;
    let max_bars = ((rect.width() + gap) / (4.0 + gap)).floor().max(1.0) as usize;
    let bars = n.min(max_bars);
    let bar_w = (rect.width() - gap * (bars as f32 - 1.0)) / bars as f32;
    let per = n as f32 / bars as f32;
    let cy = rect.center().y;
    for b in 0..bars {
        let lo = (b as f32 * per) as usize;
        let hi = (((b + 1) as f32 * per) as usize).clamp(lo + 1, n);
        let rv = raw[lo..hi].iter().copied().fold(0.0f32, f32::max);
        let cv = cleaned[lo..hi].iter().copied().fold(0.0f32, f32::max);
        let x = rect.left() + b as f32 * (bar_w + gap);
        let draw = |v: f32, c: Color32| {
            // perceptual-ish scale so quiet noise stays visible
            let h = (v.clamp(0.0, 1.0).sqrt() * rect.height()).max(2.0);
            let r = Rect::from_center_size(pos2(x + bar_w / 2.0, cy), vec2(bar_w, h));
            painter.rect_filled(r, CornerRadius::same(2), c);
        };
        draw(rv, p.raw_wave);
        draw(cv, p.accent);
    }
    resp
}

/// Microphone glyph (stroke icon from the mockup), drawn in `rect`.
pub fn mic_icon(ui: &Ui, rect: Rect, color: Color32, muted: bool) {
    let painter = ui.painter();
    let s = rect.width().min(rect.height()) / 24.0;
    let at = |x: f32, y: f32| {
        pos2(
            rect.center().x + (x - 12.0) * s,
            rect.center().y + (y - 12.0) * s,
        )
    };
    let stroke = Stroke::new(2.0 * s.max(0.75), color);
    let capsule = Rect::from_min_max(at(9.0, 2.0), at(15.0, 14.0));
    painter.rect_stroke(
        capsule,
        CornerRadius::same((3.0 * s) as u8),
        stroke,
        StrokeKind::Middle,
    );
    let arc: Vec<Pos2> = (0..=16)
        .map(|i| {
            let a = std::f32::consts::PI * i as f32 / 16.0;
            at(12.0 - 7.0 * a.cos(), 10.0 + 7.0 * a.sin())
        })
        .collect();
    painter.line(arc, stroke);
    painter.line_segment([at(12.0, 17.0), at(12.0, 22.0)], stroke);
    if muted {
        painter.line_segment(
            [at(3.0, 3.0), at(21.0, 21.0)],
            Stroke::new(stroke.width, color),
        );
    }
}

/// Big selectable card button (preset cards). Returns the click response.
pub fn choice_card(
    ui: &mut Ui,
    p: &Palette,
    selected: bool,
    title: &str,
    subtitle: Option<&str>,
    size: Vec2,
) -> Response {
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    resp.widget_info(|| WidgetInfo::selected(Role::Button, true, selected, title));
    let (fill, stroke) = if selected {
        (p.sel, Stroke::new(2.0, p.accent))
    } else if resp.hovered() {
        (p.panel, Stroke::new(1.0, p.border2))
    } else {
        (p.panel, Stroke::new(1.0, p.border))
    };
    let radius = CornerRadius::same(if size.y > 60.0 { 12 } else { 8 });
    ui.painter()
        .rect(rect, radius, fill, stroke, StrokeKind::Inside);
    let painter = ui.painter_at(rect);
    let pad = 16.0;
    match subtitle {
        Some(sub) => {
            painter.text(
                pos2(rect.left() + pad, rect.center().y - 4.0),
                Align2::LEFT_BOTTOM,
                title,
                FontId::proportional(17.0),
                p.text,
            );
            painter.text(
                pos2(rect.left() + pad, rect.center().y + 4.0),
                Align2::LEFT_TOP,
                sub,
                FontId::proportional(13.0),
                if selected { p.soft } else { p.muted },
            );
        }
        None => {
            painter.text(
                pos2(rect.left() + 14.0, rect.center().y),
                Align2::LEFT_CENTER,
                title,
                FontId::proportional(14.0),
                if selected { p.text } else { p.soft },
            );
        }
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}

/// Compact key-cap label ("Ctrl+Shift+M").
pub fn keycap(ui: &mut Ui, p: &Palette, text: &str) {
    egui::Frame::new()
        .stroke(Stroke::new(1.0, p.border2))
        .corner_radius(CornerRadius::same(6))
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.label(egui::RichText::new(text).size(12.5).color(p.text));
        });
}

/// Filled accent button (primary action).
pub fn primary_button(p: &Palette, text: &str) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text.to_owned())
            .strong()
            .color(p.on_accent),
    )
    .fill(p.accent)
    .min_size(vec2(0.0, 36.0))
}

/// Outlined secondary button.
pub fn secondary_button(p: &Palette, text: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_owned()).color(p.text))
        .fill(Color32::TRANSPARENT)
        .stroke(Stroke::new(1.0, p.border2))
        .min_size(vec2(0.0, 36.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn db_mapping() {
        assert_eq!(db_to_frac(METER_FLOOR_DB), 0.0);
        assert_eq!(db_to_frac(f32::NEG_INFINITY), 0.0);
        assert_eq!(db_to_frac(-60.0), 0.0);
        assert!((db_to_frac(-30.0) - 0.5).abs() < 1e-6);
        assert_eq!(db_to_frac(0.0), 1.0);
        assert_eq!(db_to_frac(6.0), 1.0);
    }
}
