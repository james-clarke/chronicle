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
    /// Window edge stroke; a step brighter than SURFACE_2 so the card edge
    /// reads against an arbitrary desktop.
    pub const BORDER: Color32 = Color32::from_rgb(0x32, 0x34, 0x3e);
    /// Categorical identity hues (m25: eight, so unrelated projects stop
    /// sharing a colour on the band and in lanes); a project hashes to one,
    /// its tasks are shades of it. Status colors below stay out of this set.
    /// Ordered so neighbours are far apart in hue: projects are handed
    /// hues by position, and the first few must never look alike.
    pub const SERIES: [Color32; 8] = [
        Color32::from_rgb(0x5e, 0x87, 0xea), // blue
        Color32::from_rgb(0xd9, 0x80, 0x3c), // orange
        Color32::from_rgb(0x27, 0xa9, 0x7f), // green
        Color32::from_rgb(0x9b, 0x7b, 0xe6), // violet
        Color32::from_rgb(0x3a, 0xa9, 0xc4), // teal
        Color32::from_rgb(0xbd, 0x88, 0x27), // gold
        Color32::from_rgb(0xd4, 0x6a, 0x8a), // rose
        Color32::from_rgb(0x8f, 0xb3, 0x39), // lime
    ];
    pub const AMBER: Color32 = Color32::from_rgb(0xd9, 0xa4, 0x41);
    pub const ORANGE: Color32 = Color32::from_rgb(0xe0, 0x78, 0x4f);
    pub const RED: Color32 = Color32::from_rgb(0xe0, 0x6c, 0x75);
    pub const GREEN: Color32 = Color32::from_rgb(0x8f, 0xc7, 0x8f);
}

/// Proportional family with Inter Medium first; for headings and emphasis.
pub(super) const MEDIUM: &str = "inter-medium";
/// Family holding only the Phosphor subset; render its glyphs via [`glyph`].
pub(super) const ICONS: &str = "phosphor";

/// A Phosphor [`icon`] as text in the icon family; chain `.text_style` /
/// `.color` as for any label (size follows the text style).
pub(super) fn glyph(icon: &str) -> egui::RichText {
    egui::RichText::new(icon).family(egui::FontFamily::Name(ICONS.into()))
}

/// Corner radii: SM inputs/buttons/badges/chips, MD cards and menus.
pub(super) const RADIUS_SM: u8 = 6;
pub(super) const RADIUS_MD: u8 = 8;
/// Root window card (composited X11 only; square fallback otherwise).
pub(super) const RADIUS_WINDOW: u8 = 12;

/// Spacing rhythm (4 / 8 / 12 / 16); every gap/margin should be one of
/// these (12 is [`CARD_MARGIN`]).
pub(super) const SPACE_XS: f32 = 4.0;
pub(super) const SPACE_SM: f32 = 8.0;
pub(super) const SPACE_LG: f32 = 16.0;
/// Card chrome: inner margin, gap between sibling cards, gap between sections.
pub(super) const CARD_MARGIN: i8 = 12;
pub(super) const CARD_GAP: f32 = SPACE_SM;
pub(super) const SECTION_GAP: f32 = SPACE_LG;
/// Page inset: every view's root content (and the bottom panels flanking
/// it) sits inside `symmetric(PAGE_MARGIN, 8)` — the top bar's own margin,
/// so cards, rows and the bar's tabs share one left edge.
pub(super) const PAGE_MARGIN: i8 = 12;
/// Window width (points, shadow pad included) from which views spread out:
/// the timeline keeps its detail pane beside the cards, Home puts the feed
/// in a second column. Below it every view is the one-column widget.
pub(super) const WIDE_W: f32 = 720.0;

pub(super) fn wide(ctx: &egui::Context) -> bool {
    ctx.viewport_rect().width() >= WIDE_W
}

/// Row spacing on Home (Working on + feed): meta `ui_density`, read at
/// boot and written from Settings › Window & appearance. Comfortable pads
/// each [`ListRow::padded`] row by [`ROW_PAD`] top and bottom; compact
/// keeps rows flush.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Density {
    Comfortable,
    Compact,
}

impl Density {
    pub(super) const META_KEY: &'static str = "ui_density";

    pub(super) fn parse(s: &str) -> Option<Self> {
        match s {
            "comfortable" => Some(Self::Comfortable),
            "compact" => Some(Self::Compact),
            _ => None,
        }
    }

    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Comfortable => "comfortable",
            Self::Compact => "compact",
        }
    }
}

static DENSITY_COMPACT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub(super) fn set_density(d: Density) {
    DENSITY_COMPACT.store(d == Density::Compact, std::sync::atomic::Ordering::Relaxed);
}

pub(super) fn density() -> Density {
    if DENSITY_COMPACT.load(std::sync::atomic::Ordering::Relaxed) {
        Density::Compact
    } else {
        Density::Comfortable
    }
}

/// Vertical padding a padded row gets on each side under Comfortable.
const ROW_PAD: f32 = 5.0;

fn row_pad() -> f32 {
    match density() {
        Density::Comfortable => ROW_PAD,
        Density::Compact => 0.0,
    }
}

/// Fixed-width, right-aligned number column at the end of list rows, so
/// durations line up across rows whatever the title and chip widths.
pub(super) const NUM_COL: f32 = 64.0;

/// Display style (22 Medium): the one big number per view (day total,
/// headline stat). Registered in [`style`]; resolve via `.text_style`.
pub(super) fn display() -> egui::TextStyle {
    egui::TextStyle::Name("display".into())
}

/// Caption style (10.5): axis ticks, band labels, finest-grain metadata.
/// Pair with `TEXT_DIM` — caption text is always dim.
pub(super) fn caption() -> egui::TextStyle {
    egui::TextStyle::Name("caption".into())
}

/// Content width for a view, computed once at the top of `*_ui` and threaded
/// into all width math. Never re-query `available_width()` per child: one
/// over-wide sibling widens the parent Ui's max_rect for everything after it,
/// so later widths inflate past the 400pt window and get hard-clipped.
pub(super) fn content_width(ui: &egui::Ui) -> f32 {
    ui.available_width()
}

/// Root panel for a view: no fill (the window card is the ground), content
/// inset by [`PAGE_MARGIN`].
pub(super) fn page() -> egui::CentralPanel {
    egui::CentralPanel::default().frame(page_frame())
}

/// The page inset as a frame, for the bottom panels that flank a page.
pub(super) fn page_frame() -> egui::Frame {
    egui::Frame::new().inner_margin(egui::Margin::symmetric(PAGE_MARGIN, 8))
}

/// Centered empty-state: dim headline + weak hint, for any view with nothing
/// to show. Caller centers it vertically (or it just tops the panel).
pub(super) fn empty_state(ui: &mut egui::Ui, headline: &str, hint: &str) {
    ui.vertical_centered(|ui| {
        ui.label(
            egui::RichText::new(headline)
                .text_style(egui::TextStyle::Heading)
                .color(palette::TEXT_DIM),
        );
        ui.add_space(SPACE_XS);
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
    if wrap {
        ui.add(label.wrap());
    } else {
        truncated_label(ui, label.truncate(), text);
    }
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
    fonts.font_data.insert(
        "phosphor".into(),
        egui::FontData::from_static(include_bytes!("../../assets/fonts/Phosphor-subset.ttf"))
            .into(),
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
    // Icons are their own family, never a proportional fallback: first in
    // the chain the icon font's metrics would set every label's line height,
    // and behind Inter its glyphs are shadowed (Inter maps hundreds of
    // private-use codepoints — circled arrows and the like).
    fonts.families.insert(
        egui::FontFamily::Name(ICONS.into()),
        vec!["phosphor".into()],
    );
    ctx.set_fonts(fonts);
    ctx.set_theme(egui::ThemePreference::Dark);
    ctx.style_mut_of(egui::Theme::Dark, style);
}

fn style(style: &mut egui::Style) {
    use egui::{FontFamily, FontId, TextStyle};
    // m19 type scale: Display 22 / Heading 15 / Body 13 / Small 11.5 /
    // Caption 10.5. Call sites use text styles, never hand-rolled sizes.
    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(15.0, FontFamily::Name(MEDIUM.into())),
        ),
        (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(13.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(11.5, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(12.5, FontFamily::Monospace),
        ),
        (
            display(),
            FontId::new(22.0, FontFamily::Name(MEDIUM.into())),
        ),
        (caption(), FontId::new(10.5, FontFamily::Proportional)),
    ]
    .into();

    let spacing = &mut style.spacing;
    spacing.item_spacing = egui::vec2(6.0, 4.0);
    spacing.button_padding = egui::vec2(8.0, 4.0);
    spacing.window_margin = egui::Margin::same(14);
    spacing.menu_margin = egui::Margin::same(8);
    spacing.interact_size.y = 24.0;
    spacing.extra_text_line_spacing = 1.0;
    // No visible scrollbar at all: floating so it reserves no width, every
    // track and handle opacity zero. Wheel/drag scrolling still works.
    spacing.scroll = egui::style::ScrollStyle {
        floating: true,
        bar_width: 6.0,
        floating_width: 4.0,
        handle_min_length: 24.0,
        foreground_color: true,
        dormant_background_opacity: 0.0,
        active_background_opacity: 0.0,
        interact_background_opacity: 0.0,
        dormant_handle_opacity: 0.0,
        active_handle_opacity: 0.0,
        interact_handle_opacity: 0.0,
        ..egui::style::ScrollStyle::solid()
    };

    let v = &mut style.visuals;
    // Panels paint nothing: the root ui paints one rounded (or square) BG
    // card behind everything, so square panel fills can't overpaint the
    // rounded window corners. Explicit frames (top bar) still fill.
    v.panel_fill = Color32::TRANSPARENT;
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
    // Every state carries a 1px bg_stroke (transparent where it must not
    // show). Button folds the stroke width into its padding, but `small`
    // buttons zero the vertical padding, so a state without a stroke was 2px
    // shorter than one with — hovering pushed everything below the button.
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0, Color32::TRANSPARENT);
    v.widgets.hovered.fg_stroke.color = palette::TEXT;
    v.widgets.hovered.weak_bg_fill = Color32::from_rgb(0x2c, 0x2e, 0x38);
    v.widgets.hovered.bg_fill = Color32::from_rgb(0x2c, 0x2e, 0x38);
    v.widgets.hovered.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(0x3a, 0x3d, 0x4a));
    v.widgets.active.fg_stroke.color = palette::TEXT;
    v.widgets.active.weak_bg_fill = Color32::from_rgb(0x34, 0x36, 0x42);
    v.widgets.active.bg_fill = Color32::from_rgb(0x34, 0x36, 0x42);
    v.widgets.active.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(0x44, 0x47, 0x56));
    v.widgets.open.weak_bg_fill = palette::SURFACE_2;
}

/// Projects in first-seen order (see `storage::project_order`), refreshed on
/// every data reload: a project's hue is its position here, so the first
/// eight projects never share one. Empty until the first load, when names
/// fall back to hashing.
static PROJECT_ORDER: std::sync::RwLock<Vec<String>> = std::sync::RwLock::new(Vec::new());

pub(super) fn set_project_order(order: Vec<String>) {
    if let Ok(mut cur) = PROJECT_ORDER.write()
        && *cur != order
    {
        *cur = order;
    }
}

/// Identity hue for a project name: its first-seen position when known,
/// else hashed.
pub(super) fn project_hue(project: &str) -> Color32 {
    let known = PROJECT_ORDER
        .read()
        .ok()
        .and_then(|o| o.iter().position(|p| p == project));
    match known {
        Some(i) => series_color_for(i as i64),
        None => series_color_for_key(project),
    }
}

/// Identity color for a task, stable across views (dot, band, report bars):
/// the project's hue (see [`project_hue`]) in one of three shades picked by
/// task id, so tasks of one project read as a family and two projects never
/// share a swatch. Untagged tasks cycle the hues by id.
pub(super) fn task_color(task_id: i64, project: Option<&str>) -> Color32 {
    let hue = match project.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => project_hue(p),
        None => series_color_for(task_id),
    };
    match task_id.rem_euclid(3) {
        0 => hue,
        1 => blend(hue, Color32::WHITE, 0.18),
        _ => blend(hue, palette::BG, 0.22),
    }
}

/// Identity hue by id, cycling the series palette.
pub(super) fn series_color_for(task_id: i64) -> Color32 {
    palette::SERIES[task_id.rem_euclid(palette::SERIES.len() as i64) as usize]
}

/// Identity color for a name (app, project): hashed into the series palette
/// so it keeps its color across days and views.
pub(super) fn series_color_for_key(key: &str) -> Color32 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    key.hash(&mut h);
    series_color_for((h.finish() % 1024) as i64)
}

/// A window title as shown on cards and rows: Claude Code prefixes its
/// terminal title with a spinner/status glyph (✳ ◑ ☐ …) that means nothing
/// to a reader here, so the leading run of such glyphs is dropped. Stored
/// titles (and the digest) keep it.
pub(super) fn display_title(title: &str) -> &str {
    const STATUS_GLYPHS: &[char] = &[
        '\u{2733}', '\u{273b}', '\u{273d}', '\u{2736}', '\u{2722}', '\u{2749}', '\u{25d0}',
        '\u{25d1}', '\u{25d2}', '\u{25d3}', '\u{2610}', '\u{23fa}', '\u{00b7}', '\u{2731}',
        '\u{2732}',
    ];
    title.trim_start_matches(|c: char| c.is_whitespace() || STATUS_GLYPHS.contains(&c))
}

/// Cut `text` to whole sentences fitting `max_chars` (at least one). Returns
/// the cut text and whether anything was dropped.
pub(super) fn cap_sentences(text: &str, max_chars: usize) -> (&str, bool) {
    let text = text.trim();
    if text.chars().count() <= max_chars {
        return (text, false);
    }
    let mut end = 0;
    let mut prev = None;
    for (i, c) in text.char_indices() {
        if matches!(prev, Some('.' | '!' | '?')) && c.is_whitespace() {
            if text[..i].chars().count() > max_chars && end > 0 {
                break;
            }
            end = i;
        }
        prev = Some(c);
    }
    if end == 0 {
        // No sentence boundary inside the budget: cut at the last word.
        let byte = text
            .char_indices()
            .nth(max_chars)
            .map(|(b, _)| b)
            .unwrap_or(text.len());
        end = text[..byte].rfind(' ').unwrap_or(byte);
    }
    (text[..end].trim_end(), true)
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

/// Section header: Heading title, optional count, hairline below; controls
/// right-aligned on the header line (laid out right-to-left: add the
/// outermost first).
pub(super) fn section_header_with(
    ui: &mut egui::Ui,
    title: &str,
    count: Option<usize>,
    trailing: impl FnOnce(&mut egui::Ui),
) {
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
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), trailing);
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
    // Painted by hand rather than as a Frame around a label: inside a
    // horizontal layout a frame's content ui spans the row's full height,
    // so on a two-line row the chip stretched to two lines too.
    let pad = egui::vec2(6.0, 1.0);
    let font = egui::TextStyle::Small.resolve(ui.style());
    let max_w = (ui.available_width() - 2.0 * pad.x).max(16.0);
    let mut job = egui::text::LayoutJob::simple(text.to_owned(), font, color, f32::INFINITY);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_w);
    let galley = ui.fonts_mut(|f| f.layout_job(job));
    let elided = galley.elided;
    let (rect, resp) = ui.allocate_exact_size(galley.size() + 2.0 * pad, egui::Sense::hover());
    if ui.is_rect_visible(rect) {
        ui.painter().rect_filled(
            rect,
            egui::CornerRadius::same(RADIUS_SM),
            color.gamma_multiply(0.18),
        );
        ui.painter().galley(rect.min + pad, galley, color);
    }
    if elided {
        resp.on_hover_text(text.to_owned());
    }
}

/// The one card chrome: SURFACE fill, radius 8, inner margin 12. Accent
/// variants restyle fill/stroke on top of this, never the geometry.
pub(super) fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(palette::SURFACE)
        .corner_radius(egui::CornerRadius::same(RADIUS_MD))
        .inner_margin(egui::Margin::same(CARD_MARGIN))
}

/// Card with hover lift: [`card`] whose fill brightens slightly under the
/// pointer, on the same 0.08s fade the timeline/reports hovers use.
pub(super) fn hover_card<R>(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    add: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    let mut prepared = card().begin(ui);
    let r = add(&mut prepared.content_ui);
    let rect = prepared.content_ui.min_rect().expand(CARD_MARGIN as f32);
    let hovered = ui.rect_contains_pointer(rect);
    let t = ui
        .ctx()
        .animate_bool_with_time(ui.id().with(id_salt), hovered, 0.08);
    prepared.frame.fill = blend(palette::SURFACE, palette::TEXT, 0.035 * t);
    prepared.end(ui);
    r
}

/// Disclosure body: 0.12s opacity ramp in and out; while fully closed the
/// body isn't laid out at all. Pair with [`disclosure_header`].
pub(super) fn fade_body(
    ui: &mut egui::Ui,
    id_salt: impl std::hash::Hash + std::fmt::Debug,
    open: bool,
    add: impl FnOnce(&mut egui::Ui),
) {
    let t = ui
        .ctx()
        .animate_bool_with_time(ui.id().with(id_salt), open, 0.12);
    if t <= 0.0 {
        return;
    }
    ui.scope(|ui| {
        ui.multiply_opacity(t);
        add(ui);
    });
}

/// Disclosure row: chevron + label as one ghost toggle, optional count chip.
/// Flips `open` when clicked.
pub(super) fn disclosure_header(
    ui: &mut egui::Ui,
    open: &mut bool,
    label: &str,
    count: Option<usize>,
) -> egui::Response {
    ui.horizontal(|ui| {
        let arrow = if *open { "\u{25bc}" } else { "\u{25b6}" };
        let text = egui::RichText::new(format!("{arrow} {label}"))
            .text_style(egui::TextStyle::Small)
            .color(palette::TEXT_DIM);
        let resp = ghost_button(ui, text);
        if resp.clicked() {
            *open = !*open;
        }
        if let Some(n) = count {
            badge(ui, &n.to_string(), palette::TEXT_DIM);
        }
        resp
    })
    .inner
}

/// Add a truncating label; when it actually elides, hovering shows `full`.
pub(super) fn truncated_label(ui: &mut egui::Ui, label: egui::Label, full: &str) -> egui::Response {
    // egui shows its own tooltip for elided text; ours below is the only one.
    let resp = ui.add(label.show_tooltip_when_elided(false));
    if resp
        .intrinsic_size()
        .is_some_and(|s| s.x > resp.rect.width() + 0.5)
    {
        resp.clone().on_hover_text(full.to_owned());
    }
    resp
}

/// On/off switch: a pill track with a sliding knob, ACCENT when on. For
/// settings that flip a source or a behaviour (a checkbox reads as a tick
/// mark, not a state). Returns the response; `on` flips on click.
pub(super) fn toggle(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let size = egui::vec2(30.0, 16.0);
    let (rect, mut resp) = ui.allocate_exact_size(size, egui::Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    if ui.is_rect_visible(rect) {
        let t = ui.ctx().animate_bool(resp.id, *on);
        let track = if *on {
            blend(palette::SURFACE_2, palette::ACCENT, t)
        } else {
            palette::SURFACE_2
        };
        let track = if resp.hovered() {
            blend(track, Color32::WHITE, 0.06)
        } else {
            track
        };
        let radius = rect.height() / 2.0;
        ui.painter().rect_filled(rect, radius, track);
        let knob_r = radius - 3.0;
        let cx = egui::lerp((rect.left() + radius)..=(rect.right() - radius), t);
        let knob = if *on { palette::BG } else { palette::TEXT_DIM };
        ui.painter()
            .circle_filled(egui::pos2(cx, rect.center().y), knob_r, knob);
    }
    resp
}

/// Blend `c` toward `toward` by `t` (0..1). Hover/active shade math for the
/// button helpers, kept in gamma space on purpose — subtle steps, not physics.
fn blend(c: Color32, toward: Color32, t: f32) -> Color32 {
    let ch = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * t).round() as u8;
    Color32::from_rgb(
        ch(c.r(), toward.r()),
        ch(c.g(), toward.g()),
        ch(c.b(), toward.b()),
    )
}

/// Primary action: ACCENT fill, dark medium-weight text. At most one per
/// view — the thing the user came to do.
pub(super) fn primary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    primary_button_enabled(ui, true, label)
}

/// [`primary_button`] with an enabled gate (egui's disabled dimming applies).
pub(super) fn primary_button_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    label: &str,
) -> egui::Response {
    let text = egui::RichText::new(label)
        .family(egui::FontFamily::Name(MEDIUM.into()))
        .color(palette::BG);
    ui.scope(|ui| {
        let w = &mut ui.style_mut().visuals.widgets;
        w.inactive.weak_bg_fill = palette::ACCENT;
        w.inactive.bg_stroke = egui::Stroke::NONE;
        w.hovered.weak_bg_fill = blend(palette::ACCENT, palette::TEXT, 0.15);
        w.hovered.bg_stroke = egui::Stroke::NONE;
        w.active.weak_bg_fill = blend(palette::ACCENT, palette::BG, 0.2);
        w.active.bg_stroke = egui::Stroke::NONE;
        ui.add_enabled(enabled, egui::Button::new(text))
    })
    .inner
}

/// Secondary action: SURFACE fill with a 1px stroke. Real but not the point.
pub(super) fn secondary_button(ui: &mut egui::Ui, label: &str) -> egui::Response {
    ui.scope(|ui| {
        let w = &mut ui.style_mut().visuals.widgets;
        w.inactive.weak_bg_fill = palette::SURFACE;
        w.inactive.bg_stroke = egui::Stroke::new(1.0, Color32::from_rgb(0x3a, 0x3d, 0x4a));
        w.hovered.weak_bg_fill = palette::SURFACE_2;
        ui.add(egui::Button::new(label))
    })
    .inner
}

/// Ghost action: no chrome until hover. Incidental controls (…, dismiss, ×).
pub(super) fn ghost_button(
    ui: &mut egui::Ui,
    label: impl Into<egui::WidgetText>,
) -> egui::Response {
    ui.scope(|ui| {
        let w = &mut ui.style_mut().visuals.widgets;
        w.inactive.weak_bg_fill = Color32::TRANSPARENT;
        w.inactive.bg_stroke = egui::Stroke::new(1.0, Color32::TRANSPARENT);
        ui.add(egui::Button::new(label))
    })
    .inner
}

/// Tab / option toggle: no chrome until hover, accent tint when selected.
/// `Button::selectable` drops the whole frame — stroke included — while
/// inactive, which would undo the 1px-stroke geometry rule in [`style`];
/// keep the frame and blank its fill instead.
pub(super) fn selectable<'a>(
    ui: &mut egui::Ui,
    selected: bool,
    atoms: impl egui::IntoAtoms<'a>,
) -> egui::Response {
    ui.scope(|ui| {
        ui.style_mut().visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        ui.add(egui::Button::selectable(selected, atoms).frame_when_inactive(true))
    })
    .inner
}

/// Numeric text — durations, clock times, ranges: Monospace at TEXT_DIM, so
/// digits share widths and columns of them line up.
pub(super) fn num(text: impl Into<String>) -> egui::RichText {
    egui::RichText::new(text)
        .text_style(egui::TextStyle::Monospace)
        .color(palette::TEXT_DIM)
}

/// Right-aligned cell of fixed `width` for a [`num`] (a row's number column).
pub(super) fn num_cell(ui: &mut egui::Ui, width: f32, text: egui::RichText) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, ui.spacing().interact_size.y),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            ui.set_width(width);
            ui.add(egui::Label::new(text).selectable(false));
        },
    );
}

/// One full-width list row: identity dot, a title that fills and truncates
/// (hover shows it whole), chips, the fixed [`NUM_COL`] number column, and
/// the trailing control pinned right. Rows built from the same `width` line
/// up column for column — Grid can't, it sizes columns to content.
pub(super) struct ListRow<'a> {
    title: &'a str,
    emphasis: bool,
    dot: Option<Color32>,
    chips: Vec<(String, Color32)>,
    num: Option<String>,
    lines: usize,
    bar: Option<Color32>,
    subtitle: Option<String>,
    padded: bool,
}

impl<'a> ListRow<'a> {
    pub(super) fn new(title: &'a str) -> Self {
        Self {
            title,
            emphasis: false,
            dot: None,
            chips: Vec::new(),
            num: None,
            lines: 1,
            bar: None,
            subtitle: None,
            padded: false,
        }
    }

    /// 3pt identity bar down the row's left edge (full row height,
    /// padding included) instead of the dot.
    pub(super) fn bar(mut self, color: Color32) -> Self {
        self.bar = Some(color);
        self
    }

    /// Small dim second line under the title, truncated with the whole
    /// text on hover. Chips and the trailing control stay on the title line.
    pub(super) fn subtitle(mut self, text: impl Into<String>) -> Self {
        let text = text.into();
        if !text.is_empty() {
            self.subtitle = Some(text);
        }
        self
    }

    /// Give the row the Home density padding ([`density`]).
    pub(super) fn padded(mut self) -> Self {
        self.padded = true;
        self
    }

    /// Let the title wrap onto up to `n` lines before it truncates (m25:
    /// chips and the number column used to leave a 30-character title).
    /// The row grows by whole text lines only when the title needs them.
    pub(super) fn lines(mut self, n: usize) -> Self {
        self.lines = n.max(1);
        self
    }

    /// Medium-weight title: the row is a primary item (an open task).
    pub(super) fn emphasis(mut self) -> Self {
        self.emphasis = true;
        self
    }

    /// 8pt identity dot before the title.
    pub(super) fn dot(mut self, color: Color32) -> Self {
        self.dot = Some(color);
        self
    }

    /// Tinted chip after the title; chips keep the order they are added in.
    pub(super) fn chip(mut self, text: impl Into<String>, color: Color32) -> Self {
        self.chips.push((text.into(), color));
        self
    }

    /// Text for the number column.
    pub(super) fn num(mut self, text: impl Into<String>) -> Self {
        self.num = Some(text.into());
        self
    }

    /// Lay the row out over `width`. `trailing` fills the right-most slot
    /// and is laid out right-to-left: add the outermost control first.
    pub(super) fn show(
        self,
        ui: &mut egui::Ui,
        width: f32,
        trailing: impl FnOnce(&mut egui::Ui),
    ) -> egui::Response {
        let mut h = ui.spacing().interact_size.y;
        let font = if self.emphasis {
            egui::FontId::new(
                egui::TextStyle::Body.resolve(ui.style()).size,
                egui::FontFamily::Name(MEDIUM.into()),
            )
        } else {
            egui::TextStyle::Body.resolve(ui.style())
        };
        // Multi-line titles: the width left for the title is estimated up
        // front (chip and number widths from their text), and the galley is
        // laid out once at that width so the row can be allocated at its
        // real height — a taller child inside a 24pt row would overlap the
        // row above it.
        let title_galley = (self.lines > 1).then(|| {
            // 72 covers the trailing controls (a `…` menu plus a dot or a
            // ghost button); the real leftover width re-lays the galley
            // below when it turns out narrower.
            let mut reserved = 72.0 + self.num.as_ref().map_or(0.0, |_| NUM_COL + 6.0);
            if self.dot.is_some() {
                reserved += 14.0;
            }
            if self.bar.is_some() {
                reserved += BAR_INSET;
            }
            let small = egui::TextStyle::Small.resolve(ui.style());
            for (text, _) in &self.chips {
                let chip =
                    ui.fonts_mut(|f| f.layout_no_wrap(text.clone(), small.clone(), palette::TEXT));
                reserved += chip.size().x + 12.0 + 6.0;
            }
            let title_w = (width - reserved).max(60.0);
            let mut job = egui::text::LayoutJob::simple(
                self.title.to_owned(),
                font.clone(),
                palette::TEXT,
                title_w,
            );
            job.wrap.max_rows = self.lines;
            job.wrap.break_anywhere = false;
            job.wrap.overflow_character = Some('\u{2026}');
            let galley = ui.fonts_mut(|f| f.layout_job(job));
            let line_h = ui.text_style_height(&egui::TextStyle::Body);
            if galley.rows.len() > 1 {
                h += line_h * (galley.rows.len() as f32 - 1.0) + 2.0;
            }
            (galley, title_w)
        });
        let title_h = h;
        let sub_h = ui.text_style_height(&egui::TextStyle::Small);
        if self.subtitle.is_some() {
            h += sub_h + SUB_GAP;
        }
        let pad = if self.padded { row_pad() } else { 0.0 };
        // The row is allocated at its full height (padding included) so the
        // bar spans it; content lays out in the inset rect.
        let (rect, _) =
            ui.allocate_exact_size(egui::vec2(width, h + 2.0 * pad), egui::Sense::hover());
        let mut inner = rect.shrink2(egui::vec2(0.0, pad));
        if let Some(color) = self.bar {
            let bar = egui::Rect::from_min_size(rect.min, egui::vec2(BAR_W, rect.height()));
            ui.painter()
                .rect_filled(bar, egui::CornerRadius::same(1), color);
            inner.min.x += BAR_INSET;
        }
        let subtitle = self.subtitle;
        let dot = self.dot;
        // Title line (dot, title, chips, number, trailing) at `title_h`; the
        // subtitle underneath spans the whole row so it never fights the
        // chips for width.
        let title_line = |ui: &mut egui::Ui, trailing: Box<dyn FnOnce(&mut egui::Ui) + '_>| {
            ui.set_width(inner.width());
            ui.style_mut().interaction.selectable_labels = false;
            if let Some(color) = dot {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, color);
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                trailing(ui);
                if let Some(n) = &self.num {
                    num_cell(ui, NUM_COL, num(n.as_str()));
                }
                for (text, color) in self.chips.iter().rev() {
                    badge(ui, text, *color);
                }
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    match title_galley {
                        Some((galley, title_w)) => {
                            let actual_w = ui.available_width();
                            ui.set_max_width(title_w.min(actual_w));
                            // Estimate too generous: re-lay at the real
                            // width so the title truncates instead of
                            // running under the trailing controls.
                            let galley = if actual_w + 0.5 < title_w {
                                let mut job = egui::text::LayoutJob::simple(
                                    self.title.to_owned(),
                                    font.clone(),
                                    palette::TEXT,
                                    actual_w.max(20.0),
                                );
                                job.wrap.max_rows = galley.rows.len().max(1);
                                job.wrap.break_anywhere = false;
                                job.wrap.overflow_character = Some('\u{2026}');
                                ui.fonts_mut(|f| f.layout_job(job))
                            } else {
                                galley
                            };
                            let elided = galley.elided;
                            let resp = ui.add(egui::Label::new(galley).selectable(false));
                            if elided {
                                resp.on_hover_text(self.title.to_owned());
                            }
                        }
                        None => {
                            let mut text = egui::RichText::new(self.title).color(palette::TEXT);
                            if self.emphasis {
                                text = text.family(egui::FontFamily::Name(MEDIUM.into()));
                            }
                            truncated_label(ui, egui::Label::new(text).truncate(), self.title);
                        }
                    }
                });
            });
        };
        ui.scope_builder(
            egui::UiBuilder::new()
                .max_rect(inner)
                .layout(egui::Layout::top_down(egui::Align::Min)),
            |ui| {
                ui.spacing_mut().item_spacing.y = SUB_GAP;
                ui.allocate_ui_with_layout(
                    egui::vec2(inner.width(), title_h),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| title_line(ui, Box::new(trailing)),
                );
                if let Some(sub) = &subtitle {
                    // Under the title, past the dot when there is one.
                    ui.horizontal(|ui| {
                        if dot.is_some() {
                            ui.add_space(14.0);
                        }
                        ui.style_mut().interaction.selectable_labels = false;
                        let text = egui::RichText::new(sub.as_str())
                            .text_style(egui::TextStyle::Small)
                            .color(palette::TEXT_DIM);
                        truncated_label(ui, egui::Label::new(text).truncate(), sub);
                    });
                }
            },
        )
        .response
    }
}

/// Gap between a row's title line and its subtitle.
const SUB_GAP: f32 = 2.0;

/// Identity bar width and the gap it plus its margin take from the title.
const BAR_W: f32 = 3.0;
const BAR_INSET: f32 = BAR_W + 8.0;

/// Card title row: Heading/TEXT title flush with the card body, an optional
/// disclosure caret (the whole row toggles `open`; the caret brightens on
/// hover), trailing controls right-aligned (added right-to-left). For the
/// cards themselves; [`disclosure_header`] stays the Small ghost toggle for
/// incidental rows.
pub(super) fn card_header(
    ui: &mut egui::Ui,
    title: &str,
    open: Option<&mut bool>,
    trailing: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    let is_open = open.as_deref().copied();
    let sense = if is_open.is_some() {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let resp = ui
        .scope_builder(egui::UiBuilder::new().sense(sense), |ui| {
            let hovered = ui.response().hovered();
            ui.style_mut().interaction.selectable_labels = false;
            ui.horizontal(|ui| {
                if let Some(is_open) = is_open {
                    let caret = if is_open {
                        icon::CARET_DOWN
                    } else {
                        icon::CARET_RIGHT
                    };
                    let color = if hovered {
                        palette::TEXT
                    } else {
                        palette::TEXT_DIM
                    };
                    ui.label(
                        glyph(caret)
                            .text_style(egui::TextStyle::Heading)
                            .color(color),
                    );
                }
                ui.label(
                    egui::RichText::new(title)
                        .text_style(egui::TextStyle::Heading)
                        .color(palette::TEXT),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), trailing);
            });
        })
        .response;
    if let Some(open) = open
        && resp.clicked()
    {
        *open = !*open;
    }
    resp
}

/// Phosphor Regular (icons v2.1.0) glyphs in the bundled subset
/// `assets/fonts/Phosphor-subset.ttf`, rendered through [`glyph`]. This list is the
/// subset manifest — to add one, append its codepoint here and regenerate
/// from the full `Phosphor.ttf` (egui-phosphor 0.13.0, `res/`):
/// `uvx --from fonttools pyftsubset Phosphor.ttf --unicodes=<every U+ below, comma-separated> --name-IDs='*' --output-file=crates/app/assets/fonts/Phosphor-subset.ttf`
#[allow(dead_code)] // consumed progressively by the m20 chunks (tiles, activity rows, sources)
pub(super) mod icon {
    pub const TIMER: &str = "\u{E492}";
    pub const BRAIN: &str = "\u{E74E}";
    pub const ARROWS_LEFT_RIGHT: &str = "\u{E0A0}";
    pub const CLOCK_COUNTDOWN: &str = "\u{ED2C}";
    pub const FIRE: &str = "\u{E242}";
    pub const CALENDAR_CHECK: &str = "\u{E712}";
    pub const CALENDAR_BLANK: &str = "\u{E10A}";
    pub const TREND_UP: &str = "\u{E4AE}";
    pub const TREND_DOWN: &str = "\u{E4AC}";
    pub const EQUALS: &str = "\u{E21C}";
    pub const GIT_COMMIT: &str = "\u{E27A}";
    pub const NOTE_PENCIL: &str = "\u{E34C}";
    pub const TICKET: &str = "\u{E490}";
    pub const SLACK_LOGO: &str = "\u{E5A8}";
    pub const CARET_RIGHT: &str = "\u{E13A}";
    pub const CARET_DOWN: &str = "\u{E136}";
    pub const ARROW_CLOCKWISE: &str = "\u{E036}";
    pub const SPARKLE: &str = "\u{E6A2}";
    pub const CHAT_CIRCLE: &str = "\u{E168}";
    pub const APP_WINDOW: &str = "\u{E5DA}";
    pub const PULSE: &str = "\u{E000}";
    pub const HOURGLASS_HIGH: &str = "\u{E2B4}";
    pub const ARROWS_MERGE: &str = "\u{ED3E}";
    pub const DOTS_THREE: &str = "\u{E1FE}";
    pub const X: &str = "\u{E4F6}";
    pub const PENCIL_SIMPLE: &str = "\u{E3B4}";
    pub const GIT_BRANCH: &str = "\u{E278}";
    pub const GIT_PULL_REQUEST: &str = "\u{E282}";
    pub const PHONE_CALL: &str = "\u{E3BA}";
    pub const TERMINAL_WINDOW: &str = "\u{EAE8}";
    pub const LIGHTNING: &str = "\u{E2DE}";
    pub const TARGET: &str = "\u{E47C}";
    pub const CHECK: &str = "\u{E182}";
    pub const WARNING: &str = "\u{E4E0}";
    pub const CALENDAR: &str = "\u{E108}";
    pub const CODE: &str = "\u{E1BC}";
    pub const TERMINAL: &str = "\u{E47E}";
    pub const COPY: &str = "\u{E1CA}";
    pub const PAPER_PLANE_TILT: &str = "\u{E398}";
    pub const PUSH_PIN: &str = "\u{E3E2}";
    pub const FLAG: &str = "\u{E244}";
    pub const SIGN_IN: &str = "\u{E428}";
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
