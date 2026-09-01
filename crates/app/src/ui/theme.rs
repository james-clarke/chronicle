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

/// Corner radii: SM inputs/buttons/badges, MD menus/cards-within-cards,
/// LG cards/panels.
pub(super) const RADIUS_SM: u8 = 6;
pub(super) const RADIUS_MD: u8 = 8;
pub(super) const RADIUS_LG: u8 = 10;

/// Content width for a view, computed once at the top of `*_ui` and threaded
/// into all width math. Never re-query `available_width()` per child: one
/// over-wide sibling widens the parent Ui's max_rect for everything after it,
/// so later widths inflate past the 400pt window and get hard-clipped.
pub(super) fn content_width(ui: &egui::Ui) -> f32 {
    ui.available_width()
}

/// Centered empty-state: dim headline + weak hint, for any view with nothing
/// to show. Caller centers it vertically (or it just tops the panel).
pub(super) fn empty_state(ui: &mut egui::Ui, headline: &str, hint: &str) {
    ui.vertical_centered(|ui| {
        ui.label(
            egui::RichText::new(headline)
                .size(15.0)
                .family(egui::FontFamily::Name(MEDIUM.into()))
                .color(palette::TEXT_DIM),
        );
        ui.add_space(4.0);
        ui.label(
            egui::RichText::new(hint)
                .text_style(egui::TextStyle::Small)
                .color(palette::TEXT_DIM.gamma_multiply(0.75)),
        );
    });
}

/// One dim italic line for an AI-written task summary; renders nothing while
/// no summary exists. `wrap` for the roomy detail pane, truncate on cards.
pub(super) fn ai_summary_line(ui: &mut egui::Ui, text: Option<&str>, wrap: bool) {
    let Some(text) = text else { return };
    let rich = egui::RichText::new(text)
        .text_style(egui::TextStyle::Small)
        .italics()
        .color(palette::TEXT_DIM);
    let label = egui::Label::new(rich);
    ui.add(if wrap { label.wrap() } else { label.truncate() });
}

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
    spacing.item_spacing = egui::vec2(6.0, 4.0);
    spacing.button_padding = egui::vec2(8.0, 4.0);
    spacing.window_margin = egui::Margin::same(14);
    spacing.menu_margin = egui::Margin::same(8);
    spacing.interact_size.y = 24.0;
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

/// Section header: heading text, optional weak count, hairline underneath.
pub(super) fn section_header(ui: &mut egui::Ui, title: &str, count: Option<usize>) {
    let resp = ui
        .horizontal(|ui| {
            ui.label(
                egui::RichText::new(title)
                    .text_style(egui::TextStyle::Heading)
                    .color(palette::TEXT),
            );
            if let Some(n) = count {
                ui.weak(format!("\u{b7} {n}"));
            }
        })
        .response;
    ui.painter().hline(
        ui.max_rect().x_range(),
        resp.rect.bottom() + 2.0,
        egui::Stroke::new(1.0, palette::SURFACE_2),
    );
}

/// Small tinted pill: dimmed fill of `color`, text in `color`. Text
/// truncates against the containing cell so a long project name can never
/// widen a grid column past the 400pt window.
pub(super) fn badge(ui: &mut egui::Ui, text: &str, color: Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.18))
        .corner_radius(egui::CornerRadius::same(RADIUS_SM))
        .inner_margin(egui::Margin::symmetric(6, 1))
        .show(ui, |ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(text)
                        .color(color)
                        .text_style(egui::TextStyle::Small),
                )
                .truncate(),
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
