//! Dark-only visual theme: palette, typography (embedded Inter), spacing, and
//! the confidence→color mapping. Applied once at startup via [`apply`].

use eframe::egui;
use egui::Color32;

pub(super) mod palette {
    use super::Color32;

    pub const BG: Color32 = Color32::from_rgb(0x13, 0x14, 0x18);
    pub const SURFACE: Color32 = Color32::from_rgb(0x1c, 0x1d, 0x23);
    pub const SURFACE_2: Color32 = Color32::from_rgb(0x24, 0x26, 0x2e);
    pub const INPUT_BG: Color32 = Color32::from_rgb(0x0e, 0x0f, 0x12);
    pub const TEXT: Color32 = Color32::from_rgb(0xe4, 0xe5, 0xea);
    pub const TEXT_DIM: Color32 = Color32::from_rgb(0xb8, 0xba, 0xc4);
    pub const ACCENT: Color32 = Color32::from_rgb(0x7a, 0x9e, 0xf5);
    /// Categorical task-identity colors (CVD-checked on BG); cycles by
    /// group index. Status colors below stay out of this set.
    pub const SERIES: [Color32; 3] = [
        Color32::from_rgb(0x5e, 0x87, 0xea),
        Color32::from_rgb(0x27, 0xa9, 0x7f),
        Color32::from_rgb(0xbd, 0x88, 0x27),
    ];
    pub const AMBER: Color32 = Color32::from_rgb(0xd9, 0xa4, 0x41);
    pub const ORANGE: Color32 = Color32::from_rgb(0xe0, 0x78, 0x4f);
    pub const RED: Color32 = Color32::from_rgb(0xe0, 0x6c, 0x75);
    pub const GREEN: Color32 = Color32::from_rgb(0x8f, 0xc7, 0x8f);
}

/// Proportional family with Inter Medium first; for headings and emphasis.
pub(super) const MEDIUM: &str = "inter-medium";

pub(super) fn apply(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "inter".into(),
        egui::FontData::from_static(include_bytes!("../../assets/fonts/Inter-Regular.ttf")).into(),
    );
    fonts.font_data.insert(
        MEDIUM.into(),
        egui::FontData::from_static(include_bytes!("../../assets/fonts/Inter-Medium.ttf")).into(),
    );
    let proportional = fonts
        .families
        .get_mut(&egui::FontFamily::Proportional)
        .expect("default proportional family");
    proportional.insert(0, "inter".into());
    // Medium falls back to the same chain so arrows/emoji keep rendering.
    let mut medium_chain = proportional.clone();
    medium_chain[0] = MEDIUM.into();
    fonts
        .families
        .insert(egui::FontFamily::Name(MEDIUM.into()), medium_chain);
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.style_mut_of(egui::Theme::Dark, style);
}

fn style(style: &mut egui::Style) {
    use egui::{FontFamily, FontId, TextStyle};
    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(16.5, FontFamily::Name(MEDIUM.into())),
        ),
        (TextStyle::Body, FontId::new(14.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(13.5, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(11.5, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(12.5, FontFamily::Monospace),
        ),
    ]
    .into();

    let spacing = &mut style.spacing;
    spacing.item_spacing = egui::vec2(8.0, 6.0);
    spacing.button_padding = egui::vec2(10.0, 5.0);
    spacing.window_margin = egui::Margin::same(14);
    spacing.menu_margin = egui::Margin::same(8);
    spacing.interact_size.y = 26.0;
    spacing.extra_text_line_spacing = 1.0;

    let v = &mut style.visuals;
    v.panel_fill = palette::BG;
    v.window_fill = palette::SURFACE;
    v.window_stroke = egui::Stroke::new(1.0, palette::SURFACE_2);
    v.extreme_bg_color = palette::INPUT_BG;
    v.text_edit_bg_color = Some(palette::INPUT_BG);
    v.faint_bg_color = palette::SURFACE_2;
    v.code_bg_color = palette::SURFACE_2;
    v.selection.bg_fill = palette::ACCENT.gamma_multiply(0.35);
    v.hyperlink_color = palette::ACCENT;
    v.warn_fg_color = palette::AMBER;
    v.error_fg_color = palette::RED;
    v.window_corner_radius = egui::CornerRadius::same(10);
    v.menu_corner_radius = egui::CornerRadius::same(8);
    v.striped = true;

    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = egui::CornerRadius::same(6);
    }
    v.widgets.noninteractive.fg_stroke.color = palette::TEXT_DIM;
    v.widgets.noninteractive.bg_stroke.color = palette::SURFACE_2;
    v.widgets.inactive.fg_stroke.color = palette::TEXT;
    v.widgets.inactive.weak_bg_fill = palette::SURFACE_2;
    v.widgets.inactive.bg_fill = palette::SURFACE_2;
    v.widgets.hovered.fg_stroke.color = palette::TEXT;
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x2c, 0x2e, 0x38);
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x2c, 0x2e, 0x38);
    v.widgets.active.fg_stroke.color = palette::TEXT;
    v.widgets.open.weak_bg_fill = palette::SURFACE_2;
}

/// Identity color for a task, stable across views (dot, band, report bars).
pub(super) fn series_color_for(task_id: i64) -> Color32 {
    palette::SERIES[task_id.rem_euclid(palette::SERIES.len() as i64) as usize]
}

/// Confidence bucket for an interval's task assignment.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(super) enum Band {
    High,
    Medium,
    Low,
}

pub(super) fn confidence_band(c: f64) -> Band {
    if c >= 0.80 {
        Band::High
    } else if c >= 0.55 {
        Band::Medium
    } else {
        Band::Low
    }
}

/// High confidence stays quiet (no tint); the % text remains either way, so
/// color is never the only signal.
pub(super) fn confidence_color(band: Band) -> Option<Color32> {
    match band {
        Band::High => None,
        Band::Medium => Some(palette::AMBER),
        Band::Low => Some(palette::ORANGE),
    }
}

/// Small tinted pill: dimmed fill of `color`, text in `color`.
pub(super) fn badge(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.18))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(text)
                    .color(color)
                    .text_style(egui::TextStyle::Small),
            );
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_boundaries() {
        assert_eq!(confidence_band(1.0), Band::High);
        assert_eq!(confidence_band(0.80), Band::High);
        assert_eq!(confidence_band(0.79), Band::Medium);
        assert_eq!(confidence_band(0.55), Band::Medium);
        assert_eq!(confidence_band(0.54), Band::Low);
        assert_eq!(confidence_band(0.0), Band::Low);
    }

    #[test]
    fn only_high_is_untinted() {
        assert!(confidence_color(Band::High).is_none());
        assert!(confidence_color(Band::Medium).is_some());
        assert!(confidence_color(Band::Low).is_some());
    }
}
