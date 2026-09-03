//! Timeline view: widget-style "Today" face — focus-total header, activity
//! band, task cards, detail pane.

use eframe::egui;
use jiff::{ToSpan, Zoned};

use super::{
    Action, EditState, SessionRow, SpanRow, TaskGroup, TimelineApp, WorkspaceEdit, fmt_dur, theme,
};

impl TimelineApp {
    pub(super) fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        // Filtered index sets; empty query keeps everything. Background
        // scraps leave the card list for the collapsed strip below it.
        let q = self.filter.trim().to_lowercase();
        let (bg_vis, group_vis): (Vec<usize>, Vec<usize>) = (0..self.groups.len())
            .filter(|&g| {
                let t = &self.groups[g];
                matches_filter(&q, &t.label, t.project.as_deref())
            })
            .partition(|&g| self.groups[g].background);
        let candidates = self.merge_candidates();

        let day_range = self.day_range_ms().ok();
        let day_start = self.day.to_zoned(self.tz.clone()).ok();

        let mut pending: Option<Action> = None;
        // Detail-pane fade-in. Registered every frame (not just while open):
        // animate_bool snaps on an id's first sighting, so a lazily created
        // id would never animate. Fade-out is invisible (the pane unmounts).
        let detail_t = ui.ctx().animate_bool_with_time(
            egui::Id::new("detail_pane_fade"),
            self.selected_task.is_some(),
            0.12,
        );
        // Detail pane for the selected task: side panel when wide, the whole
        // central panel when the window is widget-narrow.
        let narrow = !theme::wide(ui.ctx());
        if let Some(sel) = self.selected_task {
            match self.groups.iter().position(|g| g.task_id == sel) {
                Some(gi) => {
                    let group = &self.groups[gi];
                    let edit = &mut self.edit;
                    let ws_edit = &mut self.ws_edit;
                    let merge_pick = &mut self.merge_pick;
                    let spans = &self.spans;
                    let mut close_detail = false;
                    let color = theme::task_color(group.task_id, group.project.as_deref());
                    if narrow {
                        // Actions pinned to the widget's bottom edge; the
                        // bottom panel must be added before the CentralPanel.
                        let frame = theme::page_frame().fill(theme::palette::SURFACE);
                        egui::Panel::bottom("task_detail_actions")
                            .frame(frame)
                            .show(ui, |ui| {
                                ui.multiply_opacity(detail_t);
                                detail_actions(ui, group, edit, merge_pick, &mut pending);
                            });
                        theme::page().show(ui, |ui| {
                            ui.multiply_opacity(detail_t);
                            egui::ScrollArea::vertical()
                                .auto_shrink(false)
                                .show(ui, |ui| {
                                    close_detail = detail_ui(
                                        ui,
                                        group,
                                        color,
                                        edit,
                                        ws_edit,
                                        spans,
                                        merge_pick,
                                        &candidates,
                                        &mut pending,
                                    );
                                });
                        });
                        if close_detail {
                            self.selected_task = None;
                        }
                        if let Some(action) = pending {
                            self.apply_action(action);
                        }
                        return;
                    }
                    let frame = egui::Frame::new()
                        .fill(theme::palette::SURFACE)
                        .inner_margin(egui::Margin::same(14));
                    egui::Panel::right("task_detail")
                        .frame(frame)
                        .resizable(true)
                        .default_size(320.0)
                        .size_range(280.0..=420.0)
                        .show(ui, |ui| {
                            ui.multiply_opacity(detail_t);
                            close_detail = detail_ui(
                                ui,
                                group,
                                color,
                                edit,
                                ws_edit,
                                spans,
                                merge_pick,
                                &candidates,
                                &mut pending,
                            );
                            ui.add_space(10.0);
                            detail_actions(ui, group, edit, merge_pick, &mut pending);
                        });
                    if close_detail {
                        self.selected_task = None;
                    }
                }
                // Task left the day (merged away / reassigned): drop selection.
                None => self.selected_task = None,
            }
        }
        theme::page().show(ui, |ui| {
            if let Some(warning) = &self.warning {
                ui.colored_label(ui.visuals().warn_fg_color, warning);
            }
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
                return;
            }
            let groups = &self.groups;
            let spans = &self.spans;
            let edit = &mut self.edit;
            let merge_pick = &mut self.merge_pick;
            let selected_task = &mut self.selected_task;
            let show_background = &mut self.show_background;
            let show_unplaced = &mut self.show_unplaced;
            let unplaced = &self.unplaced;
            let band_mode = self.band_mode;
            // Foreground and background together, interval order restored,
            // for the activity band (the band stays honest).
            let mut band_vis: Vec<usize> = group_vis.iter().chain(&bg_vis).copied().collect();
            band_vis.sort_unstable();
            // Pinned once; every row below sizes from this instead of
            // re-querying available_width (see theme::content_width).
            let content_w = theme::content_width(ui);
            if groups.is_empty() || (group_vis.is_empty() && bg_vis.is_empty()) {
                today_header(ui, groups, spans);
                ui.add_space(48.0);
                if groups.is_empty() {
                    theme::empty_state(
                        ui,
                        "nothing derived yet",
                        "tasks appear here as the day is analyzed",
                    );
                } else {
                    theme::empty_state(
                        ui,
                        "no tasks match the filter",
                        "clear it from the \u{2026} menu",
                    );
                }
                return;
            }
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    today_header(ui, groups, spans);
                    if let (Some((lo, hi)), Some(day_start)) = (day_range, &day_start) {
                        activity_chart(
                            ui, content_w, groups, &band_vis, lo, hi, day_start, band_mode,
                        );
                    }
                    ui.add_space(4.0);
                    for &g in &group_vis {
                        task_card(
                            ui,
                            content_w,
                            &groups[g],
                            theme::task_color(groups[g].task_id, groups[g].project.as_deref()),
                            edit,
                            merge_pick,
                            selected_task,
                            &mut pending,
                        );
                        if *merge_pick == Some(groups[g].task_id)
                            && !merge_picker(
                                ui,
                                content_w,
                                theme::task_color(groups[g].task_id, groups[g].project.as_deref()),
                                groups[g].task_id,
                                &candidates,
                                &mut pending,
                            )
                        {
                            *merge_pick = None;
                        }
                    }
                    if !bg_vis.is_empty() {
                        let total: i64 = bg_vis.iter().map(|&g| groups[g].total_ms).sum();
                        ui.add_space(2.0);
                        theme::disclosure_header(
                            ui,
                            show_background,
                            &format!("background \u{b7} {}", fmt_dur(total)),
                            Some(bg_vis.len()),
                        )
                        .on_hover_text("short scattered sessions; declare one to promote it");
                        theme::fade_body(ui, "background_body", *show_background, |ui| {
                            ui.add_space(4.0);
                            for &g in &bg_vis {
                                task_card(
                                    ui,
                                    content_w,
                                    &groups[g],
                                    theme::task_color(
                                        groups[g].task_id,
                                        groups[g].project.as_deref(),
                                    ),
                                    edit,
                                    merge_pick,
                                    selected_task,
                                    &mut pending,
                                );
                                if *merge_pick == Some(groups[g].task_id)
                                    && !merge_picker(
                                        ui,
                                        content_w,
                                        theme::task_color(
                                            groups[g].task_id,
                                            groups[g].project.as_deref(),
                                        ),
                                        groups[g].task_id,
                                        &candidates,
                                        &mut pending,
                                    )
                                {
                                    *merge_pick = None;
                                }
                            }
                        });
                    }
                    // Activity no task claims (m22): a call between tasks,
                    // a PR reviewed in a gap. Same row shape as the
                    // detail pane's Activity section.
                    if !unplaced.is_empty() && q.is_empty() {
                        ui.add_space(2.0);
                        theme::disclosure_header(
                            ui,
                            show_unplaced,
                            "unplaced activity",
                            Some(unplaced.len()),
                        )
                        .on_hover_text("calls, PRs and sessions overlapping no task");
                        theme::fade_body(ui, "unplaced_body", *show_unplaced, |ui| {
                            ui.add_space(4.0);
                            for a in unplaced {
                                activity_row(
                                    ui,
                                    a.kind,
                                    &a.time.strftime("%H:%M").to_string(),
                                    a.duration_ms.map(super::fmt_dur).as_deref(),
                                    &a.summary,
                                );
                            }
                        });
                    }
                });
        });
        if let Some(action) = pending {
            self.apply_action(action);
        }
    }
}

/// Case-insensitive substring match against a label and optional project.
/// `q` must already be trimmed and lowercased; empty matches everything.
pub(super) fn matches_filter(q: &str, label: &str, project: Option<&str>) -> bool {
    q.is_empty()
        || label.to_lowercase().contains(q)
        || project.is_some_and(|p| p.to_lowercase().contains(q))
}

/// Focus total left; away/switching totals right.
fn today_header(ui: &mut egui::Ui, groups: &[TaskGroup], spans: &[SpanRow]) {
    let total: i64 = groups.iter().map(|g| g.total_ms).sum();
    let mut away = 0i64;
    let mut switching = 0i64;
    for s in spans {
        let dur = s.end.timestamp().as_millisecond() - s.start.timestamp().as_millisecond();
        match s.kind.as_str() {
            "afk" => away += dur,
            "context-switching" => switching += dur,
            _ => {}
        }
    }
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(fmt_dur(total))
                .text_style(theme::display())
                .color(theme::palette::TEXT),
        );
        ui.label(
            egui::RichText::new("focused")
                .text_style(egui::TextStyle::Small)
                .color(theme::palette::TEXT_DIM),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if away > 0 || switching > 0 {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!(
                            "away {} \u{b7} switching {}",
                            fmt_dur(away),
                            fmt_dur(switching)
                        ))
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT_DIM),
                    )
                    .truncate(),
                );
            }
        });
    });
}

/// Day chart mode: the flat activity band, one lane per task, or per-hour
/// stacks. Cycled from the day bar; remembered in meta `ui_band_mode`.
#[derive(Clone, Copy, PartialEq, Eq, Default, Debug)]
pub(super) enum BandMode {
    #[default]
    Band,
    Lanes,
    Hours,
}

impl BandMode {
    pub(super) fn next(self) -> Self {
        match self {
            Self::Band => Self::Lanes,
            Self::Lanes => Self::Hours,
            Self::Hours => Self::Band,
        }
    }

    /// Meta value and toggle label.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Self::Band => "band",
            Self::Lanes => "lanes",
            Self::Hours => "hours",
        }
    }

    pub(super) fn parse(s: &str) -> Option<Self> {
        match s {
            "band" => Some(Self::Band),
            "lanes" => Some(Self::Lanes),
            "hours" => Some(Self::Hours),
            _ => None,
        }
    }
}

const HOUR_MS: i64 = 3_600_000;
/// Band-mode strip height (centred in the shared chart height).
const BAND_H: f32 = 22.0;
const LANE_H: f32 = 12.0;
const LANE_GAP: f32 = 2.0;
/// Lane label column, gap to the axis included.
const LANE_LABEL_W: f32 = 84.0;
const HOURS_H: f32 = 40.0;

/// One display session clamped to the shown day.
#[derive(Clone, Copy)]
struct Segment {
    lo: i64,
    hi: i64,
    group: usize,
    session: usize,
}

/// Segments of the visible groups plus the shown range padded out to whole
/// local hours. Sorted by start: a shorter session nested inside another
/// task's merged one paints on top, and adjacent pairs give the switches.
struct DayChart {
    segments: Vec<Segment>,
    lo: i64,
    hi: i64,
}

impl DayChart {
    fn new(groups: &[TaskGroup], vis: &[usize], lo: i64, hi: i64) -> Option<Self> {
        let mut segments: Vec<Segment> = Vec::new();
        let (mut act_lo, mut act_hi) = (i64::MAX, i64::MIN);
        for &g in vis {
            for (si, s) in groups[g].sessions.iter().enumerate() {
                let s_lo = ms(&s.start).max(lo);
                let s_hi = ms(&s.end).min(hi);
                if s_hi <= s_lo {
                    continue;
                }
                act_lo = act_lo.min(s_lo);
                act_hi = act_hi.max(s_hi);
                segments.push(Segment {
                    lo: s_lo,
                    hi: s_hi,
                    group: g,
                    session: si,
                });
            }
        }
        if segments.is_empty() {
            return None;
        }
        segments.sort_by_key(|s| (s.lo, s.hi));
        Some(Self {
            segments,
            lo: lo + (act_lo - lo) / HOUR_MS * HOUR_MS,
            hi: (lo + (act_hi - lo + HOUR_MS - 1) / HOUR_MS * HOUR_MS).min(hi),
        })
    }

    fn span(&self) -> f32 {
        (self.hi - self.lo).max(1) as f32
    }

    /// x of instant `t` on an axis given as (left, width).
    fn x_at(&self, axis: (f32, f32), t: i64) -> f32 {
        axis.0 + (t - self.lo) as f32 / self.span() * axis.1
    }

    /// Inverse of [`x_at`](Self::x_at), so a tooltip always describes the
    /// segment actually under the cursor.
    fn ms_at(&self, axis: (f32, f32), x: f32) -> i64 {
        self.lo + ((x - axis.0) / axis.1 * self.span()) as i64
    }

    /// Topmost (last-painted) segment covering `t` that passes `pred`.
    fn seg_at(&self, t: i64, pred: impl Fn(&Segment) -> bool) -> Option<usize> {
        self.segments
            .iter()
            .rposition(|s| t >= s.lo && t < s.hi && pred(s))
    }
}

fn seg_color(groups: &[TaskGroup], seg: &Segment, hovered: bool) -> egui::Color32 {
    let color = theme::task_color(
        groups[seg.group].task_id,
        groups[seg.group].project.as_deref(),
    );
    if hovered {
        color.gamma_multiply(1.2)
    } else {
        color
    }
}

fn segment_tooltip(resp: egui::Response, groups: &[TaskGroup], seg: &Segment) {
    let group = &groups[seg.group];
    let s = &group.sessions[seg.session];
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_ui_at_pointer(|ui| {
            ui.set_max_width(220.0);
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(
                    dot.center(),
                    3.0,
                    theme::task_color(group.task_id, group.project.as_deref()),
                );
                ui.add(egui::Label::new(&group.label).truncate());
            });
            ui.weak(format!(
                "{}\u{2013}{} \u{b7} {}",
                s.start.strftime("%H:%M"),
                s.end.strftime("%H:%M"),
                fmt_dur(seg.hi - seg.lo)
            ));
        });
}

/// The day's chart in `mode`, hour labels under its time axis, and (lanes,
/// hours) the switch caption. Nothing when no session touches the day.
#[allow(clippy::too_many_arguments)]
fn activity_chart(
    ui: &mut egui::Ui,
    width: f32,
    groups: &[TaskGroup],
    vis: &[usize],
    lo: i64,
    hi: i64,
    day_start: &Zoned,
    mode: BandMode,
) {
    let Some(chart) = DayChart::new(groups, vis, lo, hi) else {
        return;
    };
    // One height for every mode (the tallest of the three on this day), so
    // switching modes never shifts the cards below; the switch caption shows
    // in every mode for the same reason.
    let chart_h = HOURS_H.max(lanes_height(lane_count(groups, &chart)));
    // (offset from the row's left edge, width) of the time axis.
    let axis = match mode {
        BandMode::Band => activity_band(ui, width, chart_h, groups, &chart),
        BandMode::Lanes => activity_lanes(ui, width, chart_h, groups, &chart),
        BandMode::Hours => activity_hours(ui, width, chart_h, groups, &chart, day_start),
    };
    hour_labels(ui, width, axis, &chart, lo, day_start);
    switch_caption(ui, groups, &chart, day_start);
}

/// Lanes in first-appearance order: one per foreground task plus a shared
/// one when any background scrap is on the chart.
fn lane_list(groups: &[TaskGroup], chart: &DayChart) -> Vec<Option<usize>> {
    let lane_key = |seg: &Segment| (!groups[seg.group].background).then_some(seg.group);
    let mut lanes: Vec<Option<usize>> = Vec::new();
    for seg in &chart.segments {
        let key = lane_key(seg);
        if key.is_some() && !lanes.contains(&key) {
            lanes.push(key);
        }
    }
    if chart.segments.iter().any(|s| lane_key(s).is_none()) {
        lanes.push(None);
    }
    lanes
}

fn lane_count(groups: &[TaskGroup], chart: &DayChart) -> usize {
    lane_list(groups, chart).len().max(1)
}

fn lanes_height(n: usize) -> f32 {
    n as f32 * LANE_H + (n.saturating_sub(1)) as f32 * LANE_GAP
}

/// Horizontal day strip: one colored segment per display session (task
/// identity color), background showing through = away.
fn activity_band(
    ui: &mut egui::Ui,
    width: f32,
    chart_h: f32,
    groups: &[TaskGroup],
    chart: &DayChart,
) -> (f32, f32) {
    let (row, resp) = ui.allocate_exact_size(egui::vec2(width, chart_h), egui::Sense::hover());
    let rect = egui::Rect::from_center_size(row.center(), egui::vec2(width, BAND_H));
    let axis = (rect.left(), rect.width());
    let hovered_seg = resp
        .hover_pos()
        .and_then(|p| chart.seg_at(chart.ms_at(axis, p.x), |_| true));
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::same(7), theme::palette::SURFACE);
    for (i, seg) in chart.segments.iter().enumerate() {
        let x0 = chart.x_at(axis, seg.lo);
        let x1 = chart.x_at(axis, seg.hi);
        let hovered = hovered_seg == Some(i);
        // Hovered segment brightens and grows into the band's 3px padding.
        let grow = 3.0
            * ui.ctx()
                .animate_bool_with_time(resp.id.with(i), hovered, 0.08);
        let block = egui::Rect::from_min_max(
            egui::pos2(x0, rect.top() + 4.0 - grow),
            egui::pos2(x1.max(x0 + 2.0), rect.bottom() - 4.0 + grow),
        );
        painter.rect_filled(
            block,
            egui::CornerRadius::same(3),
            seg_color(groups, seg, hovered),
        );
    }
    if let Some(i) = hovered_seg {
        segment_tooltip(resp, groups, &chart.segments[i]);
    }
    (0.0, rect.width())
}

/// One 12pt lane per task in order of first appearance (background scraps
/// share the last lane, each block still in its own task colour), blocks
/// at session times, a 1px tick across every lane where the task changes.
fn activity_lanes(
    ui: &mut egui::Ui,
    width: f32,
    chart_h: f32,
    groups: &[TaskGroup],
    chart: &DayChart,
) -> (f32, f32) {
    let lane_key = |seg: &Segment| (!groups[seg.group].background).then_some(seg.group);
    let lanes = lane_list(groups, chart);
    let lane_of = |seg: &Segment| lanes.iter().position(|&k| k == lane_key(seg)).unwrap_or(0);
    let n = lanes.len();
    let h = lanes_height(n);
    let (row, resp) = ui.allocate_exact_size(egui::vec2(width, chart_h), egui::Sense::hover());
    let rect = egui::Rect::from_center_size(row.center(), egui::vec2(width, h));
    let axis = (
        rect.left() + LANE_LABEL_W,
        (rect.width() - LANE_LABEL_W).max(1.0),
    );
    let lane_top = |l: usize| rect.top() + l as f32 * (LANE_H + LANE_GAP);
    let hovered_seg = resp.hover_pos().and_then(|p| {
        let l = (((p.y - rect.top()).max(0.0) / (LANE_H + LANE_GAP)) as usize).min(n - 1);
        chart.seg_at(chart.ms_at(axis, p.x), |s| lane_of(s) == l)
    });
    let painter = ui.painter();
    let font = theme::caption().resolve(ui.style());
    for (l, key) in lanes.iter().enumerate() {
        let lane =
            egui::Rect::from_min_size(egui::pos2(axis.0, lane_top(l)), egui::vec2(axis.1, LANE_H));
        painter.rect_filled(lane, egui::CornerRadius::same(3), theme::palette::SURFACE);
        let label = key.map_or("background", |g| groups[g].label.as_str());
        let mut job = egui::text::LayoutJob::simple_singleline(
            label.to_owned(),
            font.clone(),
            theme::palette::TEXT_DIM,
        );
        job.wrap = egui::text::TextWrapping::truncate_at_width(LANE_LABEL_W - 8.0);
        let galley = ui.ctx().fonts_mut(|f| f.layout_job(job));
        let y = lane.center().y - galley.size().y / 2.0;
        painter.galley(egui::pos2(rect.left(), y), galley, theme::palette::TEXT_DIM);
    }
    // Switch ticks under the blocks: task-identity transitions between
    // adjacent sessions, the same count the caption reports.
    let tick = egui::Stroke::new(1.0, theme::palette::TEXT_DIM.gamma_multiply(0.35));
    for pair in chart.segments.windows(2) {
        if pair[0].group != pair[1].group {
            painter.vline(chart.x_at(axis, pair[1].lo), rect.y_range(), tick);
        }
    }
    for (i, seg) in chart.segments.iter().enumerate() {
        let top = lane_top(lane_of(seg));
        let x0 = chart.x_at(axis, seg.lo);
        let x1 = chart.x_at(axis, seg.hi).max(x0 + 2.0);
        let block = egui::Rect::from_min_max(egui::pos2(x0, top), egui::pos2(x1, top + LANE_H));
        let hovered = hovered_seg == Some(i);
        painter.rect_filled(
            block,
            egui::CornerRadius::same(3),
            seg_color(groups, seg, hovered),
        );
    }
    if let Some(i) = hovered_seg {
        segment_tooltip(resp, groups, &chart.segments[i]);
    }
    (LANE_LABEL_W, axis.1)
}

/// One column per clock hour, task time stacked bottom-up in the lanes'
/// order; hover a column for its breakdown.
fn activity_hours(
    ui: &mut egui::Ui,
    width: f32,
    chart_h: f32,
    groups: &[TaskGroup],
    chart: &DayChart,
    day_start: &Zoned,
) -> (f32, f32) {
    let hours = ((chart.hi - chart.lo + HOUR_MS - 1) / HOUR_MS).max(1) as usize;
    let mut order: Vec<usize> = Vec::new();
    for seg in &chart.segments {
        if !order.contains(&seg.group) {
            order.push(seg.group);
        }
    }
    // cells[hour][order index] = ms of that task inside that hour.
    let mut cells: Vec<Vec<i64>> = vec![vec![0; order.len()]; hours];
    for seg in &chart.segments {
        let gi = order.iter().position(|&g| g == seg.group).unwrap_or(0);
        let first = ((seg.lo - chart.lo) / HOUR_MS).max(0) as usize;
        let last = (((seg.hi - 1 - chart.lo) / HOUR_MS) as usize).min(hours - 1);
        for (h, cell) in cells.iter_mut().enumerate().take(last + 1).skip(first) {
            let h_lo = chart.lo + h as i64 * HOUR_MS;
            cell[gi] += seg.hi.min(h_lo + HOUR_MS) - seg.lo.max(h_lo);
        }
    }
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, chart_h), egui::Sense::hover());
    let col_w = rect.width() / hours as f32;
    let hovered_col = resp
        .hover_pos()
        .map(|p| (((p.x - rect.left()) / col_w).max(0.0) as usize).min(hours - 1));
    let painter = ui.painter();
    for (h, cell) in cells.iter().enumerate() {
        let x0 = rect.left() + h as f32 * col_w + 1.0;
        let x1 = (rect.left() + (h + 1) as f32 * col_w - 1.0).max(x0 + 1.0);
        let col =
            egui::Rect::from_min_max(egui::pos2(x0, rect.top()), egui::pos2(x1, rect.bottom()));
        painter.rect_filled(col, egui::CornerRadius::same(2), theme::palette::SURFACE);
        let hovered = hovered_col == Some(h);
        // Overlapping display sessions (and the 1px floor per task) can
        // overfill the hour: scale the whole stack to the column instead of
        // dropping whoever stacks last.
        let heights: Vec<(usize, f32)> = order
            .iter()
            .enumerate()
            .filter(|&(gi, _)| cell[gi] > 0)
            .map(|(gi, &g)| {
                (
                    g,
                    (cell[gi] as f32 / HOUR_MS as f32 * rect.height()).max(1.0),
                )
            })
            .collect();
        let sum: f32 = heights.iter().map(|&(_, h)| h).sum();
        let scale = if sum > rect.height() {
            rect.height() / sum
        } else {
            1.0
        };
        let mut y = rect.bottom();
        for (g, h) in heights {
            let top = y - h * scale;
            let color = theme::task_color(groups[g].task_id, groups[g].project.as_deref());
            painter.rect_filled(
                egui::Rect::from_min_max(egui::pos2(x0, top), egui::pos2(x1, y)),
                0,
                if hovered {
                    color.gamma_multiply(1.2)
                } else {
                    color
                },
            );
            y = top;
        }
    }
    if let Some(h) = hovered_col {
        let mut rows: Vec<(usize, i64)> = order
            .iter()
            .enumerate()
            .filter(|&(gi, _)| cells[h][gi] > 0)
            .map(|(gi, &g)| (g, cells[h][gi]))
            .collect();
        if !rows.is_empty() {
            rows.sort_by_key(|&(_, m)| std::cmp::Reverse(m));
            let total: i64 = rows.iter().map(|&(_, m)| m).sum();
            let h_lo = chart.lo + h as i64 * HOUR_MS;
            let tz = day_start.time_zone().clone();
            let clock = |t: i64| {
                chronicle_core::types::ms_to_ts(t)
                    .to_zoned(tz.clone())
                    .strftime("%H:%M")
                    .to_string()
            };
            resp.on_hover_ui_at_pointer(|ui| {
                ui.set_max_width(220.0);
                ui.weak(format!(
                    "{}\u{2013}{} \u{b7} {}",
                    clock(h_lo),
                    clock(h_lo + HOUR_MS),
                    fmt_dur(total)
                ));
                for &(g, m) in rows.iter().take(4) {
                    ui.horizontal(|ui| {
                        let (dot, _) =
                            ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                        ui.painter().circle_filled(
                            dot.center(),
                            3.0,
                            theme::task_color(groups[g].task_id, groups[g].project.as_deref()),
                        );
                        ui.add(egui::Label::new(&groups[g].label).truncate());
                        ui.label(theme::num(fmt_dur(m)));
                    });
                }
            });
        }
    }
    (0.0, rect.width())
}

/// Hour tick labels under a chart's time axis, thinned to at most ~6.
fn hour_labels(
    ui: &mut egui::Ui,
    width: f32,
    (offset, axis_w): (f32, f32),
    chart: &DayChart,
    day_lo: i64,
    day_start: &Zoned,
) {
    let hours = ((chart.hi - chart.lo) / HOUR_MS).max(1);
    let step = (hours + 5) / 6;
    let (row, _) = ui.allocate_exact_size(egui::vec2(width, 14.0), egui::Sense::hover());
    let axis = (row.left() + offset, axis_w);
    let painter = ui.painter();
    let first_hour = (chart.lo - day_lo) / HOUR_MS;
    // Too narrow for even one label: clamp below would panic (min > max).
    let (label_min, label_max) = (axis.0 + 14.0, axis.0 + axis.1 - 14.0);
    if label_min > label_max {
        return;
    }
    for k in (0..=hours).step_by(step as usize) {
        let Ok(z) = day_start.checked_add((first_hour + k).hours()) else {
            continue;
        };
        let x = chart.x_at(axis, chart.lo + k * HOUR_MS);
        painter.text(
            egui::pos2(x.clamp(label_min, label_max), row.top()),
            egui::Align2::CENTER_TOP,
            z.strftime("%H:%M").to_string(),
            theme::caption().resolve(ui.style()),
            theme::palette::TEXT_DIM,
        );
    }
}

/// "N switches · fragmented hour HH:00" over the day's display sessions —
/// the same fold the reports use, so the numbers agree.
fn switch_caption(ui: &mut egui::Ui, groups: &[TaskGroup], chart: &DayChart, day_start: &Zoned) {
    let sessions: Vec<chronicle_core::insights::Session> = chart
        .segments
        .iter()
        .map(|s| chronicle_core::insights::Session {
            task_id: groups[s.group].task_id,
            start_ms: s.lo,
            end_ms: s.hi,
        })
        .collect();
    let m = chronicle_core::insights::focus_metrics(&sessions, day_start.time_zone());
    let mut text = match m.switch_count {
        0 => "no switches".to_owned(),
        1 => "1 switch".to_owned(),
        n => format!("{n} switches"),
    };
    if let Some(h) = m.most_fragmented_hour {
        text.push_str(&format!(" \u{b7} fragmented hour {h:02}:00"));
    }
    ui.add_space(2.0);
    ui.label(
        egui::RichText::new(text)
            .text_style(theme::caption())
            .color(theme::palette::TEXT_DIM),
    );
}

/// One task card: identity dot + label + duration, then project pill, time
/// range, and top evidence line. Actions live in the `…` menu; clicking the
/// card toggles its detail pane.
#[allow(clippy::too_many_arguments)]
fn task_card(
    ui: &mut egui::Ui,
    content_w: f32,
    group: &TaskGroup,
    color: egui::Color32,
    edit: &mut Option<EditState>,
    merge_pick: &mut Option<i64>,
    selected_task: &mut Option<i64>,
    pending: &mut Option<Action>,
) {
    let selected = *selected_task == Some(group.task_id);
    let stroke_color = if selected {
        color
    } else {
        theme::palette::SURFACE_2
    };
    // Hover fill from last frame's state (one-frame lag is invisible; the
    // frame's own fill has to be chosen before its response exists).
    let hover_key = egui::Id::new(("card_hover", group.task_id));
    let hovered = ui
        .ctx()
        .data(|d| d.get_temp::<bool>(hover_key))
        .unwrap_or(false);
    let fill = if hovered {
        theme::palette::SURFACE_2
    } else {
        theme::palette::SURFACE
    };
    // Sense on the container (registered before children) so the card is
    // clickable without stealing clicks from its own buttons/menus.
    let resp = ui
        .scope_builder(
            egui::UiBuilder::new()
                .id_salt(("task_card", group.task_id))
                .sense(egui::Sense::click()),
            |ui| {
                card_frame(
                    ui,
                    content_w,
                    group,
                    color,
                    stroke_color,
                    fill,
                    edit,
                    merge_pick,
                    pending,
                );
            },
        )
        .response;
    ui.ctx()
        .data_mut(|d| d.insert_temp(hover_key, resp.hovered()));
    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
    if resp.clicked() {
        *selected_task = if selected { None } else { Some(group.task_id) };
    }
    ui.add_space(2.0);
}

#[expect(clippy::too_many_arguments)]
fn card_frame(
    ui: &mut egui::Ui,
    content_w: f32,
    group: &TaskGroup,
    color: egui::Color32,
    stroke_color: egui::Color32,
    fill: egui::Color32,
    edit: &mut Option<EditState>,
    merge_pick: &mut Option<i64>,
    pending: &mut Option<Action>,
) {
    egui::Frame::new()
        .fill(fill)
        .stroke(egui::Stroke::new(1.0, stroke_color))
        .corner_radius(egui::CornerRadius::same(theme::RADIUS_MD))
        .inner_margin(egui::Margin::symmetric(10, 8))
        .show(ui, |ui| {
            // Pinned from the view's content width, never available_width():
            // an over-wide sibling above would otherwise inflate this card
            // past the window and hard-clip its right edge.
            ui.set_width(content_w - 20.0);
            // Labels must not grab clicks for text selection, or the card's
            // container sense never sees them.
            ui.style_mut().interaction.selectable_labels = false;
            if edit.as_ref().is_some_and(|e| e.task_id == group.task_id) {
                // Two rows: label alone, then project + actions (376px total
                // content width leaves no room for a single row).
                {
                    let e = edit.as_mut().expect("checked above");
                    ui.add(
                        egui::TextEdit::singleline(&mut e.label)
                            .desired_width(ui.available_width()),
                    );
                }
                ui.horizontal(|ui| {
                    let e = edit.as_mut().expect("checked above");
                    ui.add(
                        egui::TextEdit::singleline(&mut e.project)
                            .desired_width(ui.available_width() - 110.0)
                            .hint_text("project"),
                    );
                    if ui.button("save").clicked()
                        && let Some(e) = edit.take()
                    {
                        *pending = Some(Action::Rename(e));
                    }
                    if ui.button("cancel").clicked() {
                        *edit = None;
                    }
                });
                return;
            }
            // Title row on the shared list_row grid: duration in the number
            // column, menu pinned right, so every card's numbers align.
            theme::ListRow::new(&group.label)
                .emphasis()
                .lines(2)
                .dot(color)
                .num(fmt_dur(group.total_ms))
                .show(ui, content_w - 20.0, |ui| {
                    card_menu(ui, group, edit, merge_pick, pending);
                    confidence_dot(ui, group);
                });
            ui.horizontal(|ui| {
                // The time range leads in a fixed column so the monospace
                // digits line up card to card; chips follow, 4pt apart.
                if let (Some(first), Some(last)) = (group.sessions.first(), group.sessions.last()) {
                    time_col(
                        ui,
                        RANGE_COL,
                        &format!(
                            "{}\u{2013}{}",
                            first.start.strftime("%H:%M"),
                            last.end.strftime("%H:%M")
                        ),
                    );
                }
                ui.spacing_mut().item_spacing.x = theme::SPACE_XS;
                if let Some(project) = &group.project {
                    theme::badge(ui, project, color);
                }
                if group.declared {
                    theme::badge(ui, "declared", theme::palette::TEXT_DIM);
                }
                ui.spacing_mut().item_spacing.x = 6.0;
                if let Some(e) = group.evidence.first() {
                    let title = theme::display_title(&e.top_title);
                    let text = if title.is_empty() {
                        e.app.clone()
                    } else {
                        format!("{} \u{b7} {title}", e.app)
                    };
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(text)
                                .text_style(egui::TextStyle::Small)
                                .color(theme::palette::TEXT_DIM),
                        )
                        .truncate(),
                    );
                }
            });
            theme::ai_summary_line(ui, group.ai_summary.as_deref(), false);
        });
}

/// Detail pane: identity, summary, session strips (with whole-session
/// move), per-app evidence bars, activity, checkpoint, journal, context.
/// Returns true to close.
#[allow(clippy::too_many_arguments)]
fn detail_ui(
    ui: &mut egui::Ui,
    group: &TaskGroup,
    color: egui::Color32,
    edit: &mut Option<EditState>,
    ws_edit: &mut Option<WorkspaceEdit>,
    spans: &[SpanRow],
    merge_pick: &mut Option<i64>,
    candidates: &[(i64, String)],
    pending: &mut Option<Action>,
) -> bool {
    let mut close = false;
    let content_w = theme::content_width(ui);
    ui.horizontal(|ui| {
        let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
        ui.painter().circle_filled(dot.center(), 4.0, color);
        let label_w = (ui.available_width() - 36.0).max(60.0);
        ui.allocate_ui_with_layout(
            egui::vec2(label_w, 20.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                theme::truncated_label(
                    ui,
                    egui::Label::new(
                        egui::RichText::new(&group.label)
                            .text_style(egui::TextStyle::Heading)
                            .color(theme::palette::TEXT),
                    )
                    .truncate(),
                    &group.label,
                );
            },
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if theme::ghost_button(ui, "\u{d7}").clicked() {
                close = true;
            }
        });
    });
    // Rhythm: title, 4, chips, 8, summary + duration line, 16, sections.
    ui.add_space(theme::SPACE_XS);
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = theme::SPACE_XS;
        if let Some(project) = &group.project {
            theme::badge(ui, project, color);
        }
        if let Some(external_ref) = &group.external_ref {
            theme::badge(ui, external_ref, theme::palette::TEXT_DIM);
        }
        if group.declared {
            theme::badge(ui, "declared", theme::palette::TEXT_DIM);
        }
        if group.stuck {
            theme::badge(ui, "stuck", theme::palette::AMBER);
        }
    });
    ui.add_space(theme::SPACE_SM);
    theme::ai_summary_line(ui, group.ai_summary.as_deref(), true);
    if group.ai_pending {
        ui.horizontal(|ui| {
            ui.add(egui::Spinner::new().size(12.0));
            ui.weak("writing description\u{2026}");
        });
    }
    let n = group.sessions.len();
    ui.weak(format!(
        "{} across {n} session{}",
        fmt_dur(group.total_ms),
        if n == 1 { "" } else { "s" }
    ));
    if edit.as_ref().is_some_and(|e| e.task_id == group.task_id) {
        {
            let e = edit.as_mut().expect("checked above");
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut e.label).desired_width(160.0));
                ui.add(
                    egui::TextEdit::singleline(&mut e.project)
                        .desired_width(80.0)
                        .hint_text("project"),
                );
            });
            ui.add(
                egui::TextEdit::multiline(&mut e.description)
                    .desired_rows(2)
                    .desired_width(ui.available_width())
                    .hint_text("description"),
            );
        }
        ui.horizontal(|ui| {
            if ui.button("save").clicked()
                && let Some(e) = edit.take()
            {
                *pending = Some(Action::Rename(e));
            }
            if ui.button("cancel").clicked() {
                *edit = None;
            }
        });
    }
    if *merge_pick == Some(group.task_id) {
        ui.add_space(theme::SPACE_SM);
        if !merge_picker(ui, content_w, color, group.task_id, candidates, pending) {
            *merge_pick = None;
        }
    }

    detail_section(ui, "Sessions", Some(n), |_| {});
    sessions_ui(ui, content_w, group, spans, candidates, pending);

    if !group.evidence.is_empty() {
        detail_section(ui, "Where the time went", None, |_| {});
        let max_ms = group
            .evidence
            .iter()
            .map(|e| e.ms)
            .max()
            .unwrap_or(1)
            .max(1);
        for e in group.evidence.iter().take(6) {
            ui.horizontal(|ui| {
                ui.add_sized(
                    [110.0, 16.0],
                    egui::Label::new(
                        egui::RichText::new(&e.app)
                            .text_style(egui::TextStyle::Small)
                            .color(theme::palette::TEXT),
                    )
                    .truncate(),
                );
                let dur_text = fmt_dur(e.ms);
                let bar_w = (ui.available_width() - 52.0).max(20.0);
                let (rect, resp) =
                    ui.allocate_exact_size(egui::vec2(bar_w, 8.0), egui::Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(rect, egui::CornerRadius::same(4), theme::palette::SURFACE_2);
                let frac = e.ms as f32 / max_ms as f32;
                let fill = egui::Rect::from_min_size(
                    rect.min,
                    egui::vec2((rect.width() * frac).max(2.0), rect.height()),
                );
                painter.rect_filled(fill, egui::CornerRadius::same(4), color);
                if !e.top_title.is_empty() {
                    resp.on_hover_text(theme::display_title(&e.top_title));
                }
                ui.label(
                    egui::RichText::new(dur_text)
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT_DIM),
                );
            });
        }
    }

    // Timestamped artefacts inside the task's day; one row shape for every
    // kind (commit, AI session, PR event, call).
    if !group.activity.is_empty() {
        detail_section(ui, "Activity", Some(group.activity.len()), |_| {});
        for a in &group.activity {
            activity_row(
                ui,
                a.kind,
                &a.time.strftime("%H:%M").to_string(),
                a.duration_ms.map(super::fmt_dur).as_deref(),
                &a.summary,
            );
        }
    }

    if let Some(cp) = &group.checkpoint {
        detail_section(ui, "Checkpoint", None, |_| {});
        let editing = matches!(ws_edit,
            Some(WorkspaceEdit::Checkpoint { task_id, .. }) if *task_id == group.task_id);
        if editing {
            if let Some(WorkspaceEdit::Checkpoint {
                state, next_steps, ..
            }) = ws_edit.as_mut()
            {
                ui.add(
                    egui::TextEdit::multiline(state)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY),
                );
                ui.add(
                    egui::TextEdit::multiline(next_steps)
                        .desired_rows(2)
                        .desired_width(f32::INFINITY)
                        .hint_text("next steps"),
                );
            }
            ui.horizontal(|ui| {
                if ui.small_button("save").clicked() {
                    *pending = Some(Action::SaveWorkspaceEdit(
                        ws_edit.take().expect("checked above"),
                    ));
                }
                if ui.small_button("cancel").clicked() {
                    *ws_edit = None;
                }
            });
        } else {
            // Click either line to correct the checkpoint in place.
            let state = ui
                .add(
                    egui::Label::new(
                        egui::RichText::new(&cp.state)
                            .text_style(egui::TextStyle::Small)
                            .color(theme::palette::TEXT),
                    )
                    .wrap()
                    .sense(egui::Sense::click()),
                )
                .on_hover_text("edit");
            let next = ui
                .add(
                    egui::Label::new(
                        egui::RichText::new(format!("Next: {}", cp.next_steps))
                            .text_style(egui::TextStyle::Small)
                            .color(theme::palette::TEXT_DIM),
                    )
                    .wrap()
                    .sense(egui::Sense::click()),
                )
                .on_hover_text("edit");
            if state.clicked() || next.clicked() {
                *ws_edit = Some(WorkspaceEdit::Checkpoint {
                    task_id: group.task_id,
                    state: cp.state.clone(),
                    next_steps: cp.next_steps.clone(),
                });
            }
        }
    }

    if !group.journal.is_empty() {
        detail_section(ui, "Journal", Some(group.journal.len()), |_| {});
        for j in &group.journal {
            let editing = matches!(ws_edit,
                Some(WorkspaceEdit::Journal { entry_id, .. }) if *entry_id == j.id);
            if editing {
                if let Some(WorkspaceEdit::Journal { text, .. }) = ws_edit.as_mut() {
                    ui.add(
                        egui::TextEdit::multiline(text)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                    );
                }
                ui.horizontal(|ui| {
                    if ui.small_button("save").clicked() {
                        *pending = Some(Action::SaveWorkspaceEdit(
                            ws_edit.take().expect("checked above"),
                        ));
                    }
                    if ui.small_button("cancel").clicked() {
                        *ws_edit = None;
                    }
                });
                continue;
            }
            ui.horizontal_top(|ui| {
                time_col(ui, DAYTIME_COL, &j.time);
                // Click an entry to correct it in place.
                let resp = ui
                    .add(
                        egui::Label::new(
                            egui::RichText::new(&j.entry)
                                .text_style(egui::TextStyle::Small)
                                .color(theme::palette::TEXT),
                        )
                        .wrap()
                        .sense(egui::Sense::click()),
                    )
                    .on_hover_text("edit");
                if resp.clicked() {
                    *ws_edit = Some(WorkspaceEdit::Journal {
                        entry_id: j.id,
                        text: j.entry.clone(),
                    });
                }
            });
        }
    }

    // Anchored tasks always get the section; the fetch controls live on
    // its header line (right-to-left: action, then the freshness).
    if group.external_ref.is_some() {
        detail_section(ui, "Context", None, |ui| {
            if group.context_pending {
                ui.add(egui::Spinner::new().size(12.0));
                ui.label(
                    egui::RichText::new("fetching\u{2026}")
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT_DIM),
                );
                return;
            }
            let label = if group.task_context.is_some() {
                "re-fetch"
            } else {
                "fetch"
            };
            if theme::ghost_button(ui, label).clicked() {
                *pending = Some(Action::FetchContext(group.task_id));
            }
            if let Some((fetched_ts, _)) = &group.task_context {
                ui.label(
                    egui::RichText::new(format!("fetched {}", ago(*fetched_ts)))
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT_DIM),
                );
            }
        });
        match &group.task_context {
            Some((_, content)) => context_ui(ui, content),
            None => {
                ui.weak("nothing fetched yet");
            }
        }
    }

    close
}

/// Section header on the detail pane's rhythm: 16 above, 8 below.
fn detail_section(
    ui: &mut egui::Ui,
    title: &str,
    count: Option<usize>,
    trailing: impl FnOnce(&mut egui::Ui),
) {
    ui.add_space(theme::SPACE_LG);
    theme::section_header_with(ui, title, count, trailing);
    ui.add_space(theme::SPACE_SM);
}

/// Time-range column ("09:04–10:03") of a session row.
const RANGE_COL: f32 = 88.0;
/// Clock-time column ("09:41") of an activity row.
const TIME_COL: f32 = 44.0;
/// Day + clock column ("Mon 09:41") of a journal row.
const DAYTIME_COL: f32 = 72.0;
const STRIP_H: f32 = 12.0;

/// Fixed-width, left-aligned [`theme::num`] column, exactly one text line
/// tall so the row centres it like any other label.
fn time_col(ui: &mut egui::Ui, width: f32, text: &str) {
    let h = ui.text_style_height(&egui::TextStyle::Monospace);
    ui.allocate_ui_with_layout(
        egui::vec2(width, h),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_width(width);
            ui.add(egui::Label::new(theme::num(text)).selectable(false));
        },
    );
}

fn ms(z: &Zoned) -> i64 {
    z.timestamp().as_millisecond()
}

/// Sessions shorter than this fold into one "short sessions" row when
/// there are two or more of them.
const SHORT_SESSION_MS: i64 = 2 * 60_000;

/// Session strips: time column, a strip sized against the longest session
/// and filled with per-app focus segments (confidence as opacity; commit
/// and journal ticks), the duration, and the whole-session move in the
/// row's `…` menu. Hover a segment for app · title · duration. Short
/// sessions fold into one summary row (m25) so seven rows of one-minute
/// slivers do not bury the real ones.
fn sessions_ui(
    ui: &mut egui::Ui,
    content_w: f32,
    group: &TaskGroup,
    spans: &[SpanRow],
    candidates: &[(i64, String)],
    pending: &mut Option<Action>,
) {
    let longest = group
        .sessions
        .iter()
        .map(|s| ms(&s.end) - ms(&s.start))
        .max()
        .unwrap_or(1)
        .max(1);
    // Room left after the range column, duration and menu (+ gaps).
    let strip_max = (content_w - RANGE_COL - 44.0 - 48.0 - 3.0 * 6.0).max(40.0);
    let short: Vec<&SessionRow> = group
        .sessions
        .iter()
        .filter(|s| ms(&s.end) - ms(&s.start) < SHORT_SESSION_MS)
        .collect();
    let fold = short.len() >= 2;
    let move_menu = |ui: &mut egui::Ui, interval_ids: &[i64], pending: &mut Option<Action>| {
        ui.menu_button("\u{2026}", |ui| {
            ui.menu_button("move to", |ui| {
                for (task_id, label) in candidates {
                    if *task_id == group.task_id {
                        continue;
                    }
                    if ui.button(label).clicked() {
                        *pending = Some(Action::ReassignSession {
                            interval_ids: interval_ids.to_vec(),
                            to_task: *task_id,
                        });
                        ui.close();
                    }
                }
            });
        });
    };
    for s in &group.sessions {
        let (lo, hi) = (ms(&s.start), ms(&s.end));
        let dur = (hi - lo).max(1);
        if fold && dur < SHORT_SESSION_MS {
            continue;
        }
        ui.horizontal(|ui| {
            time_col(
                ui,
                RANGE_COL,
                &format!(
                    "{}\u{2013}{}",
                    s.start.strftime("%H:%M"),
                    s.end.strftime("%H:%M")
                ),
            );
            let w = (strip_max * dur as f32 / longest as f32).max(4.0);
            let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, STRIP_H), egui::Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(rect, egui::CornerRadius::same(3), theme::palette::SURFACE_2);
            let alpha = 0.45 + 0.55 * s.confidence.clamp(0.0, 1.0) as f32;
            let x_at = |t: i64| rect.left() + rect.width() * (t - lo) as f32 / dur as f32;
            let hover_x = resp.hover_pos().map(|p| p.x);
            let mut hover: Option<String> = None;
            for sp in spans.iter().filter(|sp| sp.kind == "focus") {
                let (a, b) = (ms(&sp.start).max(lo), ms(&sp.end).min(hi));
                if b <= a {
                    continue;
                }
                let seg = egui::Rect::from_min_max(
                    egui::pos2(x_at(a), rect.top()),
                    egui::pos2(x_at(b).max(x_at(a) + 1.0), rect.bottom()),
                );
                painter.rect_filled(
                    seg,
                    0,
                    theme::series_color_for_key(&sp.app).gamma_multiply(alpha),
                );
                if hover_x.is_some_and(|x| seg.x_range().contains(x)) {
                    hover = Some(format!(
                        "{} \u{b7} {} \u{b7} {}",
                        sp.app,
                        theme::display_title(&sp.title),
                        fmt_dur(b - a)
                    ));
                }
            }
            let tick = |t: i64, color: egui::Color32| {
                if (lo..=hi).contains(&t) {
                    let x = x_at(t);
                    painter.line_segment(
                        [
                            egui::pos2(x, rect.top() - 2.0),
                            egui::pos2(x, rect.bottom() + 2.0),
                        ],
                        egui::Stroke::new(1.0, color),
                    );
                }
            };
            for a in &group.activity {
                tick(ms(&a.time), theme::palette::TEXT);
            }
            for j in &group.journal {
                tick(j.ts, theme::palette::TEXT_DIM);
            }
            resp.on_hover_text(hover.unwrap_or_else(|| {
                format!(
                    "{} \u{b7} confidence {:.0}%",
                    fmt_dur(dur),
                    s.confidence * 100.0
                )
            }));
            ui.label(theme::num(fmt_dur(dur)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                move_menu(ui, &s.interval_ids, pending);
            });
        });
    }
    if fold {
        let total: i64 = short.iter().map(|s| ms(&s.end) - ms(&s.start)).sum();
        let ids: Vec<i64> = short
            .iter()
            .flat_map(|s| s.interval_ids.iter().copied())
            .collect();
        ui.horizontal(|ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("and {} short sessions", short.len()))
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT_DIM),
                )
                .selectable(false),
            )
            .on_hover_text(format!(
                "sessions under {}, {} together",
                fmt_dur(SHORT_SESSION_MS),
                fmt_dur(total)
            ));
            ui.label(theme::num(fmt_dur(total)));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                move_menu(ui, &ids, pending);
            });
        });
    }
}

fn activity_glyph(kind: chronicle_core::types::ActivityKind) -> &'static str {
    use chronicle_core::types::ActivityKind as K;
    match kind {
        K::Checkout | K::Commit => theme::icon::GIT_COMMIT,
        K::AiSession => theme::icon::TERMINAL_WINDOW,
        K::PrAuthored | K::PrReviewed => theme::icon::GIT_PULL_REQUEST,
        K::Call => theme::icon::PHONE_CALL,
        K::Meeting => theme::icon::CALENDAR,
        K::Edit => theme::icon::CODE,
        K::Shell => theme::icon::TERMINAL,
    }
}

/// One activity row: kind glyph, clock time, optional duration, summary
/// (truncates).
/// What an activity glyph stands for, for the hover on the glyph itself —
/// the row's only legend.
fn activity_kind_name(kind: chronicle_core::types::ActivityKind) -> &'static str {
    use chronicle_core::types::ActivityKind as K;
    match kind {
        K::Checkout => "branch checkout",
        K::Commit => "commit",
        K::AiSession => "AI session",
        K::PrAuthored => "pull request you opened",
        K::PrReviewed => "pull request you reviewed",
        K::Call => "call",
        K::Meeting => "calendar event",
        K::Edit => "editor time",
        K::Shell => "shell commands",
    }
}

fn activity_row(
    ui: &mut egui::Ui,
    kind: chronicle_core::types::ActivityKind,
    time: &str,
    duration: Option<&str>,
    summary: &str,
) {
    ui.horizontal(|ui| {
        ui.add(egui::Label::new(
            theme::glyph(activity_glyph(kind))
                .text_style(egui::TextStyle::Small)
                .color(theme::palette::TEXT_DIM),
        ))
        .on_hover_text(activity_kind_name(kind));
        time_col(ui, TIME_COL, time);
        if let Some(d) = duration {
            ui.label(
                egui::RichText::new(d)
                    .text_style(egui::TextStyle::Small)
                    .color(theme::palette::TEXT_DIM),
            );
        }
        ui.add(
            egui::Label::new(
                egui::RichText::new(summary)
                    .text_style(egui::TextStyle::Small)
                    .color(theme::palette::TEXT),
            )
            .truncate(),
        );
    });
}

/// "just now" / "12m ago" / "2h ago" / "3d ago".
pub(super) fn ago(ts_ms: i64) -> String {
    let mins = (jiff::Timestamp::now().as_millisecond() - ts_ms).max(0) / 60_000;
    if mins < 1 {
        "just now".to_owned()
    } else if mins < 60 {
        format!("{mins}m ago")
    } else if mins < 24 * 60 {
        format!("{}h ago", mins / 60)
    } else {
        format!("{}d ago", mins / (24 * 60))
    }
}

/// One `### source.tool` block of the stored context bundle.
struct ContextBlock {
    source: Option<String>,
    tool: Option<String>,
    body: String,
}

/// Split the bundle on its `### ` tool headers ("jira.jira_get_issue" →
/// source "jira", tool "jira_get_issue"); a bundle without headers is one
/// unnamed block.
fn context_blocks(content: &str) -> Vec<ContextBlock> {
    let mut blocks: Vec<ContextBlock> = Vec::new();
    for line in content.lines() {
        if let Some(head) = line.strip_prefix("### ") {
            let head = head.trim();
            let (source, tool) = match head.split_once('.') {
                Some((s, t)) => (Some(s.to_owned()), Some(t.to_owned())),
                None => (None, Some(head.to_owned())),
            };
            blocks.push(ContextBlock {
                source,
                tool,
                body: String::new(),
            });
            continue;
        }
        if blocks.is_empty() {
            blocks.push(ContextBlock {
                source: None,
                tool: None,
                body: String::new(),
            });
        }
        let body = &mut blocks.last_mut().expect("pushed above").body;
        body.push_str(line);
        body.push('\n');
    }
    blocks
}

/// The bundle as sections: a source chip + tool name per block, then its
/// body — a JSON object field by field (the identifying keys first, long
/// strings as paragraphs), or plain text as headers / bullets / paragraphs.
fn context_ui(ui: &mut egui::Ui, content: &str) {
    for (i, block) in context_blocks(content).iter().enumerate() {
        if i > 0 {
            ui.add_space(theme::SPACE_SM);
        }
        if block.source.is_some() || block.tool.is_some() {
            ui.horizontal(|ui| {
                if let Some(source) = &block.source {
                    theme::badge(ui, source, theme::palette::ACCENT);
                }
                if let Some(tool) = &block.tool {
                    ui.label(
                        egui::RichText::new(tool)
                            .text_style(egui::TextStyle::Small)
                            .family(egui::FontFamily::Name(theme::MEDIUM.into()))
                            .color(theme::palette::TEXT),
                    );
                }
            });
        }
        match serde_json::from_str::<serde_json::Value>(block.body.trim()) {
            Ok(serde_json::Value::Object(map)) => json_fields_ui(ui, &map),
            _ => text_lines_ui(ui, &block.body),
        }
    }
}

/// Keys shown first, in this order; the rest follow alphabetically.
const CONTEXT_KEYS_FIRST: [&str; 10] = [
    "key",
    "summary",
    "title",
    "status",
    "assignee",
    "reporter",
    "priority",
    "created",
    "updated",
    "description",
];
/// Longest string value rendered before eliding.
const CONTEXT_VALUE_MAX: usize = 1500;

fn json_fields_ui(ui: &mut egui::Ui, map: &serde_json::Map<String, serde_json::Value>) {
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort_by_key(|k| {
        (
            CONTEXT_KEYS_FIRST
                .iter()
                .position(|f| f == k)
                .unwrap_or(CONTEXT_KEYS_FIRST.len()),
            k.as_str(),
        )
    });
    for key in keys {
        let value = &map[key];
        match value {
            serde_json::Value::Null => {}
            serde_json::Value::String(s) if s.contains('\n') || s.len() > 80 => {
                ui.label(
                    egui::RichText::new(key)
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT_DIM),
                );
                text_lines_ui(ui, s);
            }
            serde_json::Value::String(s) => field_row(ui, key, s),
            serde_json::Value::Array(_) | serde_json::Value::Object(_) => {
                field_row(ui, key, &clip(&value.to_string(), 160));
            }
            other => field_row(ui, key, &other.to_string()),
        }
    }
}

/// `key  value` on one line; the value wraps against the key column.
fn field_row(ui: &mut egui::Ui, key: &str, value: &str) {
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::vec2(DAYTIME_COL, 0.0),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                ui.set_width(DAYTIME_COL);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(key)
                            .text_style(egui::TextStyle::Small)
                            .color(theme::palette::TEXT_DIM),
                    )
                    .truncate(),
                );
            },
        );
        ui.add(
            egui::Label::new(
                egui::RichText::new(value)
                    .text_style(egui::TextStyle::Small)
                    .color(theme::palette::TEXT),
            )
            .wrap(),
        );
    });
}

/// Plain text as lines: `#` headers, `-`/`*` bullets, paragraphs; blank
/// lines become 4pt gaps. Elides past [`CONTEXT_VALUE_MAX`].
fn text_lines_ui(ui: &mut egui::Ui, text: &str) {
    let text = clip(text, CONTEXT_VALUE_MAX);
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            ui.add_space(theme::SPACE_XS);
        } else if let Some(h) = line
            .strip_prefix("### ")
            .or_else(|| line.strip_prefix("## "))
            .or_else(|| line.strip_prefix("# "))
        {
            ui.label(
                egui::RichText::new(h)
                    .text_style(egui::TextStyle::Small)
                    .family(egui::FontFamily::Name(theme::MEDIUM.into()))
                    .color(theme::palette::TEXT),
            );
        } else if let Some(item) = line.strip_prefix("- ").or_else(|| line.strip_prefix("* ")) {
            ui.horizontal_top(|ui| {
                ui.add_space(theme::SPACE_SM);
                ui.label(
                    egui::RichText::new("\u{b7}")
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT_DIM),
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(item)
                            .text_style(egui::TextStyle::Small)
                            .color(theme::palette::TEXT),
                    )
                    .wrap(),
                );
            });
        } else {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(line)
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT),
                )
                .wrap(),
            );
        }
    }
}

/// First `max` chars, with an ellipsis when anything was cut.
fn clip(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if out.len() < text.len() {
        out.push('\u{2026}');
    }
    out
}

/// Correction actions for the selected task; rendered pinned to the widget's
/// bottom edge in narrow mode, inline at the pane's end when wide. Ghost
/// actions left, the destructive one right; merge sits in the `…` menu.
fn detail_actions(
    ui: &mut egui::Ui,
    group: &TaskGroup,
    edit: &mut Option<EditState>,
    merge_pick: &mut Option<i64>,
    pending: &mut Option<Action>,
) {
    ui.horizontal(|ui| {
        if theme::ghost_button(ui, "chat").clicked() {
            *pending = Some(Action::ChatAboutTask(group.task_id));
        }
        if theme::ghost_button(ui, "rename").clicked() {
            *edit = Some(EditState {
                task_id: group.task_id,
                label: group.label.clone(),
                project: group.project.clone().unwrap_or_default(),
                description: group.ai_summary.clone().unwrap_or_default(),
            });
        }
        ui.menu_button("\u{2026}", |ui| {
            merge_item(ui, group.task_id, merge_pick);
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Destructive action: tinted, and kept apart on the right.
            let close_btn =
                egui::Button::new(egui::RichText::new("close task").color(theme::palette::RED))
                    .fill(theme::palette::RED.gamma_multiply(0.12));
            if ui.add(close_btn).clicked() {
                *pending = Some(Action::Close(group.task_id));
            }
        });
    });
}

fn confidence_dot(ui: &mut egui::Ui, group: &TaskGroup) {
    let (mut num, mut den) = (0.0f64, 0.0f64);
    for iv in &group.intervals {
        let dur =
            (iv.end.timestamp().as_millisecond() - iv.start.timestamp().as_millisecond()) as f64;
        num += iv.confidence * dur;
        den += dur;
    }
    if den <= 0.0 {
        return;
    }
    let conf = num / den;
    if let Some(color) = theme::confidence_color(theme::confidence_band(conf)) {
        let (dot, resp) = ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
        ui.painter().circle_filled(dot.center(), 3.0, color);
        resp.on_hover_text(format!("confidence {:.0}%", conf * 100.0));
    }
}

/// `⋯` actions: rename, merge into, close.
fn card_menu(
    ui: &mut egui::Ui,
    group: &TaskGroup,
    edit: &mut Option<EditState>,
    merge_pick: &mut Option<i64>,
    pending: &mut Option<Action>,
) {
    ui.menu_button("\u{2026}", |ui| {
        if ui.button("rename").clicked() {
            *edit = Some(EditState {
                task_id: group.task_id,
                label: group.label.clone(),
                project: group.project.clone().unwrap_or_default(),
                description: group.ai_summary.clone().unwrap_or_default(),
            });
            ui.close();
        }
        merge_item(ui, group.task_id, merge_pick);
        if ui.button("close task").clicked() {
            *pending = Some(Action::Close(group.task_id));
            ui.close();
        }
    });
}

/// "merge into…" menu item: opens the inline [`merge_picker`] under the
/// card/row. A hover submenu opened over the card and merged on the first
/// click that landed on a candidate; now nothing merges until a pick.
pub(super) fn merge_item(ui: &mut egui::Ui, self_task: i64, merge_pick: &mut Option<i64>) {
    if ui.button("merge into\u{2026}").clicked() {
        *merge_pick = Some(self_task);
        ui.close();
    }
}

/// Inline "merge into" chooser: the other tasks as full-width rows under
/// the card, cancel on the header line (or Escape). A pick folds this task
/// into the chosen one (a 'merge' correction — see storage::merge_task).
/// Returns false once it should close.
pub(super) fn merge_picker(
    ui: &mut egui::Ui,
    content_w: f32,
    color: egui::Color32,
    self_task: i64,
    candidates: &[(i64, String)],
    pending: &mut Option<Action>,
) -> bool {
    let mut keep = !ui.input(|i| i.key_pressed(egui::Key::Escape));
    egui::Frame::new()
        .fill(theme::palette::SURFACE_2)
        .stroke(egui::Stroke::new(1.0, color))
        .corner_radius(egui::CornerRadius::same(theme::RADIUS_MD))
        .inner_margin(egui::Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(content_w - 16.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("merge into")
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT_DIM),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::ghost_button(ui, "cancel").clicked() {
                        keep = false;
                    }
                });
            });
            let mut any = false;
            ui.scope(|ui| {
                let w = &mut ui.style_mut().visuals.widgets;
                w.inactive.weak_bg_fill = egui::Color32::TRANSPARENT;
                w.inactive.bg_stroke = egui::Stroke::new(1.0, egui::Color32::TRANSPARENT);
                for (task_id, label) in candidates {
                    if *task_id == self_task {
                        continue;
                    }
                    any = true;
                    let row = egui::Button::new(label.as_str())
                        .truncate()
                        .min_size(egui::vec2(ui.available_width(), 0.0));
                    if ui.add(row).clicked() {
                        *pending = Some(Action::Merge {
                            from_task: self_task,
                            to_task: *task_id,
                        });
                        keep = false;
                    }
                }
            });
            if !any {
                ui.weak("no other task to merge into");
            }
        });
    ui.add_space(2.0);
    keep
}

#[cfg(test)]
mod tests {
    use super::matches_filter;

    #[test]
    fn empty_query_matches_everything() {
        assert!(matches_filter("", "anything", None));
        assert!(matches_filter("", "", Some("proj")));
    }

    #[test]
    fn matches_label_and_project_case_insensitive() {
        assert!(matches_filter("chron", "Chronicle m13", None));
        assert!(matches_filter("play", "review PR", Some("Contoso")));
        assert!(!matches_filter("jira", "review PR", Some("Contoso")));
        assert!(!matches_filter("jira", "review PR", None));
    }
}
