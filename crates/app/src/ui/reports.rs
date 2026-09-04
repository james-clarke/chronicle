//! Reports view: stacked per-day chart, focus tiles + app share bar, AI
//! narrative, and week totals grouped by project.

use eframe::egui;
use jiff::Zoned;

use super::{Action, TimelineApp, WeekInsights, fmt_dur, theme};
use chronicle_core::report::{ProjectTotal, RangeReport, TaskRow, UNTAGGED};

/// Legend/project-header columns: fixed so the elided name width, the share
/// bar's start x, and the duration/percent numbers line up across the
/// app-share legend, the project-mix legend and the project header rows
/// below them — one table, not three.
const LEGEND_NAME_COL: f32 = 100.0;
const LEGEND_PCT_COL: f32 = 34.0;

impl TimelineApp {
    pub(super) fn reports_ui(&mut self, ui: &mut egui::Ui) {
        let mut pending: Option<Action> = None;
        // Task row click: open that task's detail on its busiest day.
        let mut jump: Option<(i64, jiff::civil::Date)> = None;
        let week_insights = &self.week_insights;
        let narrative_busy = self.narrative_job.is_some();
        let narrative_blocked = !self.can_run("narrative");
        theme::page().show(ui, |ui| {
            if let Some(warning) = &self.warning {
                ui.colored_label(ui.visuals().warn_fg_color, warning);
            }
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
                return;
            }
            let Some(r) = &self.report else {
                ui.weak("loading\u{2026}");
                return;
            };
            let today = Zoned::now().with_time_zone(self.tz.clone()).date();
            let content_w = theme::content_width(ui);
            let day_totals: Vec<i64> = (0..r.days.len())
                .map(|d| r.tasks.iter().map(|t| t.by_day[d]).sum())
                .collect();
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    week_chart(ui, content_w, r, &today, &day_totals);
                    if !r.projects.is_empty() && r.grand_total_ms > 0 {
                        ui.add_space(theme::SPACE_SM);
                        project_mix(ui, content_w, r);
                    }
                    // Focus tiles only once the week has something in it
                    // (six zeros are not a report).
                    if let Some(wi) = week_insights
                        && r.grand_total_ms > 0
                    {
                        ui.add_space(theme::SECTION_GAP);
                        theme::section_header_with(ui, "Focus", None, |ui| {
                            narrative_control(
                                ui,
                                wi,
                                narrative_busy,
                                narrative_blocked,
                                &mut pending,
                            );
                        });
                        ui.add_space(theme::SPACE_SM);
                        if let Some(text) = &wi.narrative {
                            narrative_card(ui, text);
                            ui.add_space(theme::CARD_GAP);
                        }
                        focus_tiles(ui, content_w, wi, r, &day_totals);
                        if !wi.top_apps.is_empty() {
                            ui.add_space(theme::SPACE_SM);
                            apps_bar(ui, content_w, wi);
                        }
                    }
                    ui.add_space(theme::SECTION_GAP);
                    theme::section_header_with(ui, "Tasks", Some(r.tasks.len()), |ui| {
                        if r.grand_total_ms > 0 {
                            ui.label(theme::num(fmt_dur(r.grand_total_ms)));
                        }
                    });
                    if r.tasks.is_empty() {
                        ui.add_space(theme::SECTION_GAP);
                        theme::empty_state(
                            ui,
                            "nothing this week",
                            "tasks appear once a day is analyzed",
                        );
                        return;
                    }
                    for p in &r.projects {
                        ui.add_space(theme::SPACE_SM);
                        project_group(ui, content_w, r, p, &mut jump);
                    }
                });
        });
        if let Some((task_id, day)) = jump {
            self.selected_task = Some(task_id);
            self.set_day(day);
            self.loaded_at = None;
            self.view = super::View::Timeline;
        }
        if let Some(action) = pending {
            self.apply_action(action);
        }
    }
}

/// Focus header control: spinner while the narrative job runs, otherwise
/// the generate/update ghost when no fresh summary is cached. Explicit
/// trigger only — never auto-queued.
fn narrative_control(
    ui: &mut egui::Ui,
    wi: &WeekInsights,
    busy: bool,
    blocked: bool,
    pending: &mut Option<Action>,
) {
    if busy {
        ui.weak("writing summary\u{2026}");
        ui.add(egui::Spinner::new().size(12.0));
        return;
    }
    if blocked || wi.narrative.is_some() {
        return;
    }
    let label = if wi.narrative_stale {
        "update summary"
    } else {
        "generate summary"
    };
    if theme::ghost_button(ui, label).clicked() {
        *pending = Some(Action::GenerateNarrative);
    }
}

fn narrative_card(ui: &mut egui::Ui, text: &str) {
    theme::hover_card(ui, "narrative_card", |ui| {
        ui.set_width(ui.available_width());
        ui.add(
            egui::Label::new(
                egui::RichText::new(text)
                    .text_style(egui::TextStyle::Small)
                    .italics()
                    .color(theme::palette::TEXT_DIM),
            )
            .wrap(),
        );
    });
}

/// Six stat tiles, three (two when narrow) across: Display value over icon +
/// caption.
fn focus_tiles(
    ui: &mut egui::Ui,
    width: f32,
    wi: &WeekInsights,
    r: &RangeReport,
    day_totals: &[i64],
) {
    use theme::icon;
    let m = &wi.metrics;
    let busiest = day_totals
        .iter()
        .enumerate()
        .max_by_key(|&(d, &ms)| (ms, std::cmp::Reverse(d)))
        .filter(|&(_, &ms)| ms > 0)
        .map(|(d, _)| r.days[d].strftime("%a").to_string());
    let (delta_icon, delta) = match &wi.delta {
        Some(d) if d.grand_total_delta_ms > 0 => (
            icon::TREND_UP,
            format!("+{}", fmt_dur(d.grand_total_delta_ms)),
        ),
        Some(d) if d.grand_total_delta_ms < 0 => (
            icon::TREND_DOWN,
            format!("\u{2212}{}", fmt_dur(-d.grand_total_delta_ms)),
        ),
        Some(_) => (icon::EQUALS, "same".to_owned()),
        None => (icon::EQUALS, "\u{b7}".to_owned()),
    };
    let dash = || "\u{b7}".to_owned();
    let tiles: [(&str, String, &str); 6] = [
        (icon::TIMER, fmt_dur(m.longest_block_ms), "longest block"),
        (icon::BRAIN, fmt_dur(m.deep_work_ms), "deep work"),
        (
            icon::ARROWS_LEFT_RIGHT,
            m.switch_count.to_string(),
            "switches",
        ),
        (
            icon::CLOCK_COUNTDOWN,
            m.most_fragmented_hour
                .map_or_else(dash, |h| format!("{h:02}:00")),
            "fragmented hour",
        ),
        (icon::FIRE, busiest.unwrap_or_else(dash), "busiest day"),
        (delta_icon, delta, "vs prior week"),
    ];
    // Zoom 1.15 leaves ~348pt of window: Display values no longer fit
    // three across, so reflow to two.
    let cols = if width < 360.0 { 2 } else { 3 };
    let tile_w = (width - (cols as f32 - 1.0) * theme::SPACE_SM) / cols as f32;
    for row in tiles.chunks(cols) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = theme::SPACE_SM;
            for (glyph, value, caption) in row {
                tile(ui, tile_w, glyph, value, caption);
            }
        });
        ui.add_space(theme::SPACE_SM);
    }
}

fn tile(ui: &mut egui::Ui, width: f32, glyph: &str, value: &str, caption: &str) {
    const PAD: i8 = 8;
    let inner = width - 2.0 * PAD as f32;
    theme::card()
        .inner_margin(egui::Margin::symmetric(PAD, 8))
        .show(ui, |ui| {
            // The frame inherits the tile row's horizontal layout: pin the
            // width both ways and stack explicitly.
            ui.set_width(inner);
            ui.set_max_width(inner);
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 2.0;
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(value)
                            .text_style(theme::display())
                            .color(theme::palette::TEXT),
                    )
                    .truncate(),
                );
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    ui.label(
                        theme::glyph(glyph)
                            .text_style(theme::caption())
                            .color(theme::palette::TEXT_DIM),
                    );
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(caption)
                                .text_style(theme::caption())
                                .color(theme::palette::TEXT_DIM),
                        )
                        .truncate(),
                    );
                });
            });
        });
}

/// Top apps as shares of the week's app focus time: a segmented bar (hover
/// a segment for the app) over a [`legend`], biggest first.
fn apps_bar(ui: &mut egui::Ui, width: f32, wi: &WeekInsights) {
    let total = wi.apps_total_ms.max(1) as f32;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 8.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::same(2), theme::palette::SURFACE_2);
    let hover_x = resp.hover_pos().map(|p| p.x);
    let mut hovered: Option<(&str, i64)> = None;
    let mut x = rect.left();
    for (app, ms) in &wi.top_apps {
        let w = rect.width() * *ms as f32 / total;
        let seg = egui::Rect::from_min_max(
            egui::pos2(x, rect.top()),
            egui::pos2((x + w - 1.0).max(x + 1.0), rect.bottom()),
        );
        painter.rect_filled(
            seg,
            egui::CornerRadius::same(2),
            theme::series_color_for_key(app),
        );
        if hover_x.is_some_and(|hx| seg.x_range().contains(hx)) {
            hovered = Some((app, *ms));
        }
        x += w;
    }
    if let Some((app, ms)) = hovered {
        resp.on_hover_text(format!(
            "{app} \u{b7} {} \u{b7} {}%",
            fmt_dur(ms),
            (ms as f32 / total * 100.0).round()
        ));
    }
    ui.add_space(theme::SPACE_XS);
    let entries: Vec<(&str, egui::Color32, i64)> = wi
        .top_apps
        .iter()
        .map(|(app, ms)| (app.as_str(), theme::series_color_for_key(app), *ms))
        .collect();
    legend(ui, width, wi.apps_total_ms, &entries);
}

/// A report task's identity colour: its project's hue, shaded by id.
fn task_color(t: &TaskRow) -> egui::Color32 {
    let project = (t.project != UNTAGGED).then_some(t.project.as_str());
    theme::task_color(t.task_id, project)
}

fn project_color(project: &str) -> egui::Color32 {
    if project == UNTAGGED {
        theme::palette::TEXT_DIM
    } else {
        theme::project_hue(project)
    }
}

fn project_name(project: &str) -> &str {
    if project == UNTAGGED {
        "untagged"
    } else {
        project
    }
}

/// Single-row project mix under the week chart: the week's total split by
/// project, biggest first, over a [`legend`] — the chart's colour key.
/// Hover a segment for the project's total.
fn project_mix(ui: &mut egui::Ui, width: f32, r: &RangeReport) {
    let total = r.grand_total_ms.max(1) as f32;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 8.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::same(2), theme::palette::SURFACE_2);
    let hover_x = resp.hover_pos().map(|p| p.x);
    let mut hovered: Option<&ProjectTotal> = None;
    let mut x = rect.left();
    for p in &r.projects {
        let w = rect.width() * p.total_ms as f32 / total;
        let seg = egui::Rect::from_min_max(
            egui::pos2(x, rect.top()),
            egui::pos2((x + w - 1.0).max(x + 1.0), rect.bottom()),
        );
        painter.rect_filled(seg, egui::CornerRadius::same(2), project_color(&p.project));
        if hover_x.is_some_and(|hx| seg.x_range().contains(hx)) {
            hovered = Some(p);
        }
        x += w;
    }
    if let Some(p) = hovered {
        resp.on_hover_text(format!(
            "{} \u{b7} {} \u{b7} {}%",
            project_name(&p.project),
            fmt_dur(p.total_ms),
            (p.total_ms as f32 / total * 100.0).round()
        ));
    }
    ui.add_space(theme::SPACE_XS);
    let entries: Vec<(&str, egui::Color32, i64)> = r
        .projects
        .iter()
        .map(|p| {
            (
                project_name(&p.project),
                project_color(&p.project),
                p.total_ms,
            )
        })
        .collect();
    legend(ui, width, r.grand_total_ms, &entries);
}

/// Compact legend row: colour swatch, elided name (fixed
/// [`LEGEND_NAME_COL`]), a thin share bar filling what's left, then
/// duration and percent right-aligned in mono ([`theme::NUM_COL`],
/// [`LEGEND_PCT_COL`]) so every row's numbers line up.
fn legend_row(
    ui: &mut egui::Ui,
    width: f32,
    name: &str,
    color: egui::Color32,
    ms: i64,
    total: f32,
) {
    let share = ms as f32 / total;
    let row_h = ui.text_style_height(&egui::TextStyle::Small).max(12.0);
    ui.allocate_ui_with_layout(
        egui::vec2(width, row_h),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_width(width);
            let (slot, _) =
                ui.allocate_exact_size(egui::vec2(theme::STATUS_COL, row_h), egui::Sense::hover());
            ui.painter().circle_filled(slot.center(), 4.0, color);
            ui.allocate_ui_with_layout(
                egui::vec2(LEGEND_NAME_COL, row_h),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.set_width(LEGEND_NAME_COL);
                    theme::truncated_label(
                        ui,
                        egui::Label::new(
                            egui::RichText::new(name)
                                .text_style(egui::TextStyle::Small)
                                .color(theme::palette::TEXT),
                        )
                        .truncate(),
                        name,
                    );
                },
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                theme::num_cell(
                    ui,
                    LEGEND_PCT_COL,
                    theme::num(format!("{}%", (share * 100.0).round())),
                );
                theme::num_cell(ui, theme::NUM_COL, theme::num(fmt_dur(ms)));
                let (bar, _) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width().max(0.0), 4.0),
                    egui::Sense::hover(),
                );
                let painter = ui.painter();
                painter.rect_filled(bar, egui::CornerRadius::same(2), theme::palette::SURFACE_2);
                let fill = egui::Rect::from_min_size(
                    bar.min,
                    egui::vec2(
                        (bar.width() * share).max(2.0).min(bar.width()),
                        bar.height(),
                    ),
                );
                painter.rect_filled(fill, egui::CornerRadius::same(2), color);
            });
        },
    );
}

/// Top 5 `entries` (already sorted biggest first) as [`legend_row`]s, then
/// an "other" row summing what's left of `total_ms` — the chart's colour
/// key as a small table instead of a text sentence.
fn legend(ui: &mut egui::Ui, width: f32, total_ms: i64, entries: &[(&str, egui::Color32, i64)]) {
    const TOP_N: usize = 5;
    let total = total_ms.max(1) as f32;
    let shown = &entries[..entries.len().min(TOP_N)];
    for &(name, color, ms) in shown {
        legend_row(ui, width, name, color, ms, total);
    }
    let shown_ms: i64 = shown.iter().map(|&(_, _, ms)| ms).sum();
    let rest_ms = (total_ms - shown_ms).max(0);
    if rest_ms > 0 {
        legend_row(ui, width, "other", theme::palette::TEXT_DIM, rest_ms, total);
    }
}

/// Project header ([`legend_row`]: swatch, name, share bar, duration,
/// percent) then its tasks as list rows indented [`theme::PAGE_MARGIN`],
/// aligned to the header's columns; a task row click jumps to the timeline
/// on the task's busiest day.
fn project_group(
    ui: &mut egui::Ui,
    width: f32,
    r: &RangeReport,
    p: &ProjectTotal,
    jump: &mut Option<(i64, jiff::civil::Date)>,
) {
    let color = project_color(&p.project);
    legend_row(
        ui,
        width,
        project_name(&p.project),
        color,
        p.total_ms,
        r.grand_total_ms.max(1) as f32,
    );
    let indent = theme::PAGE_MARGIN as f32;
    for t in r.tasks.iter().filter(|t| t.project == p.project) {
        ui.horizontal(|ui| {
            ui.add_space(indent);
            // The header's percent cell sits right of its duration, so the
            // task rows reserve it (empty) to share the duration column.
            let row_w = width - indent - LEGEND_PCT_COL - ui.spacing().item_spacing.x;
            let resp = ui
                .scope_builder(egui::UiBuilder::new().sense(egui::Sense::click()), |ui| {
                    let mut row = theme::ListRow::new(&t.label)
                        .dot(task_color(t))
                        .num(fmt_dur(t.total_ms));
                    let mix = chronicle_core::report::kind_mix(&t.by_kind);
                    if !mix.is_empty() {
                        row = row.meta(None, mix);
                    }
                    row.show(ui, row_w, |_| {});
                })
                .response;
            if resp.clicked() {
                *jump = Some((t.task_id, busiest_day(r, t)));
            }
            resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        });
    }
}

/// The day this task got the most time (earliest on ties).
fn busiest_day(r: &RangeReport, t: &TaskRow) -> jiff::civil::Date {
    t.by_day
        .iter()
        .enumerate()
        .max_by_key(|&(d, &ms)| (ms, std::cmp::Reverse(d)))
        .map_or(r.days[0], |(d, _)| r.days[d])
}

/// Stacked per-day bars in task identity colors; today's label accented.
/// Day totals live in the hover tooltip (a painted max-value label clipped at
/// the widget's top edge); the hovered day's stack lightens and lifts.
fn week_chart(
    ui: &mut egui::Ui,
    width: f32,
    r: &RangeReport,
    today: &jiff::civil::Date,
    day_totals: &[i64],
) {
    let max_ms = day_totals.iter().copied().max().unwrap_or(0);
    if max_ms == 0 {
        return;
    }
    const CHART_H: f32 = 110.0;
    const LABEL_H: f32 = 16.0;
    const LIFT: f32 = 2.0;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(width, CHART_H + LABEL_H + 14.0),
        egui::Sense::hover(),
    );
    let slot = rect.width() / r.days.len() as f32;
    let bar_w = (slot * 0.55).min(48.0);
    // One x→day mapping used for both hit-testing here and (inverted) bar
    // placement below, so the tooltip always matches the bar under the cursor.
    let hovered_day = resp
        .hover_pos()
        .map(|p| (((p.x - rect.left()) / slot) as usize).min(r.days.len() - 1))
        .filter(|&d| day_totals[d] > 0);
    let painter = ui.painter();
    // The segment under the pointer (task index, its ms), for the tooltip.
    let hover_pos = resp.hover_pos();
    let mut hovered_seg: Option<(usize, i64)> = None;
    for (d, day) in r.days.iter().enumerate() {
        let cx = rect.left() + slot * (d as f32 + 0.5);
        let base = rect.top() + CHART_H;
        let hovered = hovered_day == Some(d);
        let raise = LIFT
            * ui.ctx()
                .animate_bool_with_time(resp.id.with(d), hovered, 0.08);
        // Stack biggest-task-first, bottom-up; LIFT headroom stays reserved
        // so a lifted full-height bar never leaves the allocated rect.
        let mut y = base - raise;
        for (ti, t) in r.tasks.iter().enumerate() {
            let ms = t.by_day[d];
            if ms == 0 {
                continue;
            }
            let h = (ms as f32 / max_ms as f32 * (CHART_H - LIFT - 1.0)).max(1.0);
            let seg = egui::Rect::from_min_max(
                egui::pos2(cx - bar_w / 2.0, y - h),
                egui::pos2(cx + bar_w / 2.0, y - 1.0),
            );
            let color = task_color(t);
            let color = if hovered {
                color.gamma_multiply(1.2)
            } else {
                color
            };
            painter.rect_filled(seg, egui::CornerRadius::same(2), color);
            if hovered && hover_pos.is_some_and(|p| seg.expand2(egui::vec2(0.0, 0.5)).contains(p)) {
                hovered_seg = Some((ti, ms));
            }
            y -= h;
        }
        let label_color = if day == today {
            theme::palette::ACCENT
        } else {
            theme::palette::TEXT_DIM
        };
        painter.text(
            egui::pos2(cx, base + 4.0),
            egui::Align2::CENTER_TOP,
            day.strftime("%a").to_string(),
            theme::caption().resolve(ui.style()),
            label_color,
        );
    }
    if let Some(d) = hovered_day {
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        resp.on_hover_ui_at_pointer(|ui| {
            ui.set_max_width(220.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(r.days[d].strftime("%a %-d %b").to_string())
                        .color(theme::palette::TEXT),
                );
                ui.weak(fmt_dur(day_totals[d]));
            });
            // The segment under the pointer first: label, project, its time
            // that day — the bar's own legend.
            if let Some((ti, ms)) = hovered_seg {
                let t = &r.tasks[ti];
                ui.horizontal(|ui| {
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 3.0, task_color(t));
                    ui.add(
                        egui::Label::new(egui::RichText::new(&t.label).color(theme::palette::TEXT))
                            .truncate(),
                    );
                });
                ui.horizontal(|ui| {
                    ui.add_space(14.0);
                    ui.weak(format!(
                        "{} \u{b7} {}",
                        project_name(&t.project),
                        fmt_dur(ms)
                    ));
                });
                ui.add_space(theme::SPACE_XS);
            }
            // Top tasks only; a 15-task day would fill the whole widget.
            let mut day_tasks: Vec<&TaskRow> = r.tasks.iter().filter(|t| t.by_day[d] > 0).collect();
            day_tasks.sort_by_key(|t| std::cmp::Reverse(t.by_day[d]));
            const TOOLTIP_ROWS: usize = 6;
            for t in day_tasks.iter().take(TOOLTIP_ROWS) {
                let ms = t.by_day[d];
                ui.horizontal(|ui| {
                    let (dot, _) =
                        ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                    ui.painter().circle_filled(dot.center(), 3.0, task_color(t));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.weak(egui::RichText::new(fmt_dur(ms)).monospace());
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&t.label)
                                        .text_style(egui::TextStyle::Small),
                                )
                                .truncate(),
                            );
                        });
                    });
                });
            }
            if day_tasks.len() > TOOLTIP_ROWS {
                let rest: i64 = day_tasks[TOOLTIP_ROWS..].iter().map(|t| t.by_day[d]).sum();
                ui.weak(format!(
                    "+{} more \u{b7} {}",
                    day_tasks.len() - TOOLTIP_ROWS,
                    fmt_dur(rest)
                ));
            }
        });
    }
}
