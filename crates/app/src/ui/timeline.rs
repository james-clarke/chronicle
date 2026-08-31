//! Timeline view: widget-style "Today" face — focus-total header, activity
//! band, task cards — with the working-on list and raw spans below.

use eframe::egui;
use jiff::{ToSpan, Zoned};

use super::{Action, EditState, OpenRow, SpanRow, TaskGroup, TimelineApp, fmt_dur, theme};

impl TimelineApp {
    pub(super) fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        // Filtered index sets; empty query keeps everything.
        let q = self.filter.trim().to_lowercase();
        let open_vis: Vec<usize> = (0..self.open_tasks.len())
            .filter(|&o| {
                let t = &self.open_tasks[o];
                matches_filter(&q, &t.label, t.project.as_deref())
            })
            .collect();
        let closed_vis: Vec<usize> = (0..self.closed_tasks.len())
            .filter(|&c| {
                let t = &self.closed_tasks[c];
                matches_filter(&q, &t.label, t.project.as_deref())
            })
            .collect();
        let group_vis: Vec<usize> = (0..self.groups.len())
            .filter(|&g| {
                let t = &self.groups[g];
                matches_filter(&q, &t.label, t.project.as_deref())
            })
            .collect();
        let span_vis: Vec<usize> = (0..self.spans.len())
            .filter(|&s| {
                let sp = &self.spans[s];
                matches_filter(&q, &sp.title, Some(&sp.app))
            })
            .collect();

        // Reassignment targets: every task in sight (open + today's).
        let mut candidates: Vec<(i64, String)> = Vec::new();
        for t in &self.open_tasks {
            candidates.push((t.task_id, t.label.clone()));
        }
        for g in &self.groups {
            if !candidates.iter().any(|(id, _)| *id == g.task_id) {
                candidates.push((g.task_id, g.label.clone()));
            }
        }

        let day_range = self.day_range_ms().ok();
        let day_start = self.day.to_zoned(self.tz.clone()).ok();

        let mut pending: Option<Action> = None;
        // Detail pane for the selected task (before the central panel so the
        // remaining width goes to the card list).
        if let Some(sel) = self.selected_task {
            match self.groups.iter().position(|g| g.task_id == sel) {
                Some(gi) => {
                    let group = &self.groups[gi];
                    let edit = &mut self.edit;
                    let mut close_detail = false;
                    let frame = egui::Frame::new()
                        .fill(theme::palette::SURFACE)
                        .inner_margin(egui::Margin::same(14));
                    egui::Panel::right("task_detail")
                        .frame(frame)
                        .resizable(true)
                        .default_size(320.0)
                        .show(ui, |ui| {
                            close_detail = detail_ui(
                                ui,
                                group,
                                theme::series_color(gi),
                                edit,
                                &candidates,
                                &mut pending,
                            );
                        });
                    if close_detail {
                        self.selected_task = None;
                    }
                }
                // Task left the day (merged away / reassigned): drop selection.
                None => self.selected_task = None,
            }
        }
        egui::CentralPanel::default().show(ui, |ui| {
            self.model_card_ui(ui);
            self.service_card_ui(ui);
            if let Some(warning) = &self.warning {
                ui.colored_label(ui.visuals().warn_fg_color, warning);
            }
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
                return;
            }
            let groups = &self.groups;
            let open_tasks = &self.open_tasks;
            let closed_tasks = &self.closed_tasks;
            let show_closed = &mut self.show_closed;
            let show_spans = &mut self.show_spans;
            let spans = &self.spans;
            let edit = &mut self.edit;
            let new_label = &mut self.new_label;
            let new_project = &mut self.new_project;
            let selected_task = &mut self.selected_task;
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                today_header(ui, groups, spans);
                if let (Some((lo, hi)), Some(day_start)) = (day_range, &day_start) {
                    activity_band(ui, groups, &group_vis, lo, hi, day_start);
                }
                ui.add_space(4.0);
                if groups.is_empty() {
                    ui.weak("no tasks derived yet");
                } else if group_vis.is_empty() {
                    ui.weak("no tasks match the filter");
                }
                for &g in &group_vis {
                    task_card(
                        ui,
                        &groups[g],
                        theme::series_color(g),
                        edit,
                        selected_task,
                        &candidates,
                        &mut pending,
                    );
                }
                ui.add_space(10.0);

                section_header(ui, "Working on", None);
                ui.horizontal(|ui| {
                    let label_w = (ui.available_width() - 220.0).clamp(240.0, 420.0);
                    ui.add(
                        egui::TextEdit::singleline(new_label)
                            .desired_width(label_w)
                            .hint_text("declare a task\u{2026}"),
                    );
                    ui.add(
                        egui::TextEdit::singleline(new_project)
                            .desired_width(110.0)
                            .hint_text("project"),
                    );
                    if ui.button("add").clicked() {
                        pending = Some(Action::Declare);
                    }
                });
                for &o in &open_vis {
                    open_row(ui, &open_tasks[o], &candidates, &mut pending);
                }
                if !closed_vis.is_empty() {
                    ui.horizontal(|ui| {
                        ui.add_space(12.0);
                        let arrow = if *show_closed { "\u{25be}" } else { "\u{25b8}" };
                        if ui.small_button(format!("{arrow} recently closed")).clicked() {
                            *show_closed = !*show_closed;
                        }
                    });
                    if *show_closed {
                        for &c in &closed_vis {
                            closed_row(ui, &closed_tasks[c], &mut pending);
                        }
                    }
                }

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    let arrow = if *show_spans { "\u{25be}" } else { "\u{25b8}" };
                    if ui
                        .small_button(format!("{arrow} Spans \u{b7} {}", span_vis.len()))
                        .clicked()
                    {
                        *show_spans = !*show_spans;
                    }
                });
                if *show_spans {
                    for &s in &span_vis {
                        span_row(ui, &spans[s]);
                    }
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
fn matches_filter(q: &str, label: &str, project: Option<&str>) -> bool {
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
                .size(22.0)
                .family(egui::FontFamily::Name(theme::MEDIUM.into()))
                .color(theme::palette::TEXT),
        );
        ui.label(
            egui::RichText::new("focused")
                .text_style(egui::TextStyle::Small)
                .color(theme::palette::TEXT_DIM),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if away > 0 || switching > 0 {
                ui.label(
                    egui::RichText::new(format!(
                        "away {} \u{b7} switching {}",
                        fmt_dur(away),
                        fmt_dur(switching)
                    ))
                    .text_style(egui::TextStyle::Small)
                    .color(theme::palette::TEXT_DIM),
                );
            }
        });
    });
}

/// Horizontal day strip: one colored segment per display session (task
/// identity color), background showing through = away. Hour labels below.
fn activity_band(
    ui: &mut egui::Ui,
    groups: &[TaskGroup],
    vis: &[usize],
    lo: i64,
    hi: i64,
    day_start: &Zoned,
) {
    const HOUR_MS: i64 = 3_600_000;
    let mut segments: Vec<(i64, i64, egui::Color32)> = Vec::new();
    let (mut act_lo, mut act_hi) = (i64::MAX, i64::MIN);
    for &g in vis {
        let color = theme::series_color(g);
        for s in &groups[g].sessions {
            let s_lo = s.start.timestamp().as_millisecond().max(lo);
            let s_hi = s.end.timestamp().as_millisecond().min(hi);
            if s_hi <= s_lo {
                continue;
            }
            act_lo = act_lo.min(s_lo);
            act_hi = act_hi.max(s_hi);
            segments.push((s_lo, s_hi, color));
        }
    }
    if segments.is_empty() {
        return;
    }
    // Pad the shown range out to whole local hours.
    let band_lo = lo + (act_lo - lo) / HOUR_MS * HOUR_MS;
    let band_hi = (lo + (act_hi - lo + HOUR_MS - 1) / HOUR_MS * HOUR_MS).min(hi);
    let span = (band_hi - band_lo).max(1) as f32;

    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, 14.0), egui::Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::same(7), theme::palette::SURFACE);
    for (s_lo, s_hi, color) in &segments {
        let x0 = rect.left() + (s_lo - band_lo) as f32 / span * rect.width();
        let x1 = rect.left() + (s_hi - band_lo) as f32 / span * rect.width();
        let seg = egui::Rect::from_min_max(
            egui::pos2(x0, rect.top() + 2.0),
            egui::pos2(x1.max(x0 + 2.0), rect.bottom() - 2.0),
        );
        painter.rect_filled(seg, egui::CornerRadius::same(3), *color);
    }

    // Hour tick labels, thinned to at most ~6.
    let hours = ((band_hi - band_lo) / HOUR_MS).max(1);
    let step = (hours + 5) / 6;
    let (label_rect, _) = ui.allocate_exact_size(egui::vec2(width, 14.0), egui::Sense::hover());
    let painter = ui.painter();
    let first_hour = (band_lo - lo) / HOUR_MS;
    for k in (0..=hours).step_by(step as usize) {
        let ms = band_lo + k * HOUR_MS;
        let Ok(z) = day_start.checked_add((first_hour + k).hours()) else {
            continue;
        };
        let x = label_rect.left() + (ms - band_lo) as f32 / span * label_rect.width();
        painter.text(
            egui::pos2(
                x.clamp(label_rect.left() + 14.0, label_rect.right() - 14.0),
                label_rect.top(),
            ),
            egui::Align2::CENTER_TOP,
            z.strftime("%H:%M").to_string(),
            egui::FontId::new(10.0, egui::FontFamily::Proportional),
            theme::palette::TEXT_DIM,
        );
    }
}

/// One task card: identity dot + label + duration, then project pill, time
/// range, and top evidence line. Actions live in the `…` menu; clicking the
/// card toggles its detail pane.
fn task_card(
    ui: &mut egui::Ui,
    group: &TaskGroup,
    color: egui::Color32,
    edit: &mut Option<EditState>,
    selected_task: &mut Option<i64>,
    candidates: &[(i64, String)],
    pending: &mut Option<Action>,
) {
    let selected = *selected_task == Some(group.task_id);
    let stroke_color = if selected { color } else { theme::palette::SURFACE_2 };
    let resp = egui::Frame::new()
        .fill(theme::palette::SURFACE)
        .stroke(egui::Stroke::new(1.0, stroke_color))
        .corner_radius(egui::CornerRadius::same(10))
        .inner_margin(egui::Margin::symmetric(12, 11))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            if edit.as_ref().is_some_and(|e| e.task_id == group.task_id) {
                ui.horizontal(|ui| {
                    let e = edit.as_mut().expect("checked above");
                    ui.add(egui::TextEdit::singleline(&mut e.label).desired_width(220.0));
                    ui.add(
                        egui::TextEdit::singleline(&mut e.project)
                            .desired_width(110.0)
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
            ui.horizontal(|ui| {
                let (dot, _) =
                    ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, color);
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&group.label)
                            .size(13.0)
                            .family(egui::FontFamily::Name(theme::MEDIUM.into()))
                            .color(theme::palette::TEXT),
                    )
                    .truncate(),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    card_menu(ui, group, edit, candidates, pending);
                    confidence_dot(ui, group);
                    ui.label(
                        egui::RichText::new(fmt_dur(group.total_ms))
                            .size(12.0)
                            .color(theme::palette::TEXT_DIM),
                    );
                });
            });
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                if let Some(project) = &group.project {
                    theme::badge(ui, project, color);
                }
                if group.declared {
                    theme::badge(ui, "declared", theme::palette::TEXT_DIM);
                }
                if let (Some(first), Some(last)) =
                    (group.sessions.first(), group.sessions.last())
                {
                    ui.label(
                        egui::RichText::new(format!(
                            "{}\u{2013}{}",
                            first.start.strftime("%H:%M"),
                            last.end.strftime("%H:%M")
                        ))
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT_DIM),
                    );
                }
                if let Some(e) = group.evidence.first() {
                    let text = if e.top_title.is_empty() {
                        e.app.clone()
                    } else {
                        format!("{} \u{b7} {}", e.app, e.top_title)
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
        })
        .response;
    // Registered after the card's own widgets, so buttons/menus keep priority.
    let resp = ui.interact(
        resp.rect,
        ui.id().with(("task_card", group.task_id)),
        egui::Sense::click(),
    );
    if resp.clicked() {
        *selected_task = if selected { None } else { Some(group.task_id) };
    }
    ui.add_space(2.0);
}

/// Detail pane: identity, summary, session chips (with whole-session move),
/// per-app evidence bars, and correction actions. Returns true to close.
fn detail_ui(
    ui: &mut egui::Ui,
    group: &TaskGroup,
    color: egui::Color32,
    edit: &mut Option<EditState>,
    candidates: &[(i64, String)],
    pending: &mut Option<Action>,
) -> bool {
    let mut close = false;
    ui.horizontal(|ui| {
        let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
        ui.painter().circle_filled(dot.center(), 4.0, color);
        ui.add(
            egui::Label::new(
                egui::RichText::new(&group.label)
                    .size(15.0)
                    .family(egui::FontFamily::Name(theme::MEDIUM.into()))
                    .color(theme::palette::TEXT),
            )
            .truncate(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("\u{2715}").clicked() {
                close = true;
            }
        });
    });
    ui.horizontal(|ui| {
        if let Some(project) = &group.project {
            theme::badge(ui, project, color);
        }
        if group.declared {
            theme::badge(ui, "declared", theme::palette::TEXT_DIM);
        }
    });
    let n = group.sessions.len();
    ui.weak(format!(
        "{} across {n} session{}",
        fmt_dur(group.total_ms),
        if n == 1 { "" } else { "s" }
    ));
    if edit.as_ref().is_some_and(|e| e.task_id == group.task_id) {
        ui.horizontal(|ui| {
            let e = edit.as_mut().expect("checked above");
            ui.add(egui::TextEdit::singleline(&mut e.label).desired_width(160.0));
            ui.add(
                egui::TextEdit::singleline(&mut e.project)
                    .desired_width(80.0)
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
    }

    ui.add_space(8.0);
    section_header(ui, "Sessions", None);
    for s in &group.sessions {
        let dur = s.end.timestamp().as_millisecond() - s.start.timestamp().as_millisecond();
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!(
                    "{}\u{2013}{}",
                    s.start.strftime("%H:%M"),
                    s.end.strftime("%H:%M")
                ))
                .color(theme::palette::TEXT),
            );
            ui.weak(fmt_dur(dur));
            if let Some(c) = theme::confidence_color(theme::confidence_band(s.confidence)) {
                let (dot, resp) =
                    ui.allocate_exact_size(egui::vec2(6.0, 6.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 3.0, c);
                resp.on_hover_text(format!("confidence {:.0}%", s.confidence * 100.0));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.menu_button("move", |ui| {
                    for (task_id, label) in candidates {
                        if *task_id == group.task_id {
                            continue;
                        }
                        if ui.button(label).clicked() {
                            *pending = Some(Action::ReassignSession {
                                interval_ids: s.interval_ids.clone(),
                                to_task: *task_id,
                            });
                            ui.close();
                        }
                    }
                });
            });
        });
    }

    if !group.evidence.is_empty() {
        ui.add_space(8.0);
        section_header(ui, "Where the time went", None);
        let max_ms = group.evidence.iter().map(|e| e.ms).max().unwrap_or(1).max(1);
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
                let (rect, resp) = ui
                    .allocate_exact_size(egui::vec2(bar_w, 8.0), egui::Sense::hover());
                let painter = ui.painter();
                painter.rect_filled(
                    rect,
                    egui::CornerRadius::same(4),
                    theme::palette::SURFACE_2,
                );
                let frac = e.ms as f32 / max_ms as f32;
                let fill = egui::Rect::from_min_size(
                    rect.min,
                    egui::vec2((rect.width() * frac).max(2.0), rect.height()),
                );
                painter.rect_filled(fill, egui::CornerRadius::same(4), color);
                if !e.top_title.is_empty() {
                    resp.on_hover_text(&e.top_title);
                }
                ui.label(
                    egui::RichText::new(dur_text)
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT_DIM),
                );
            });
        }
    }

    ui.add_space(10.0);
    ui.horizontal(|ui| {
        if ui.button("rename").clicked() {
            *edit = Some(EditState {
                task_id: group.task_id,
                label: group.label.clone(),
                project: group.project.clone().unwrap_or_default(),
            });
        }
        merge_menu(ui, group.task_id, candidates, pending);
        if ui.button("close task").clicked() {
            *pending = Some(Action::Close(group.task_id));
        }
    });
    close
}

/// Duration-weighted confidence; a small tinted dot appears only when the
/// assignment is not high-confidence (hover for the %).
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
    candidates: &[(i64, String)],
    pending: &mut Option<Action>,
) {
    ui.menu_button("\u{2026}", |ui| {
        if ui.button("rename").clicked() {
            *edit = Some(EditState {
                task_id: group.task_id,
                label: group.label.clone(),
                project: group.project.clone().unwrap_or_default(),
            });
            ui.close();
        }
        ui.menu_button("merge into", |ui| {
            for (task_id, label) in candidates {
                if *task_id == group.task_id {
                    continue;
                }
                if ui.button(label).clicked() {
                    *pending = Some(Action::Merge {
                        from_task: group.task_id,
                        to_task: *task_id,
                    });
                    ui.close();
                }
            }
        });
        if ui.button("close task").clicked() {
            *pending = Some(Action::Close(group.task_id));
            ui.close();
        }
    });
}

/// Section header: heading text, optional weak count, hairline underneath.
fn section_header(ui: &mut egui::Ui, title: &str, count: Option<usize>) {
    let resp = ui
        .horizontal(|ui| {
            ui.label(
                egui::RichText::new(title)
                    .text_style(egui::TextStyle::Heading)
                    .color(theme::palette::TEXT),
            );
            if let Some(n) = count {
                ui.weak(format!("\u{b7} {n}"));
            }
        })
        .response;
    ui.painter().hline(
        ui.max_rect().x_range(),
        resp.rect.bottom() + 2.0,
        egui::Stroke::new(1.0, theme::palette::SURFACE_2),
    );
}

/// Badge for a task's project tag, if any.
fn project_badge(ui: &mut egui::Ui, project: &Option<String>) {
    if let Some(project) = project {
        theme::badge(ui, project, theme::palette::ACCENT);
    }
}

fn declared_badge(ui: &mut egui::Ui, declared: bool) {
    if declared {
        theme::badge(ui, "declared", theme::palette::TEXT_DIM);
    }
}

fn open_row(
    ui: &mut egui::Ui,
    task: &OpenRow,
    candidates: &[(i64, String)],
    pending: &mut Option<Action>,
) {
    ui.horizontal(|ui| {
        ui.add_space(18.0);
        ui.strong(&task.label);
        project_badge(ui, &task.project);
        declared_badge(ui, task.declared);
        if ui.small_button("close").clicked() {
            *pending = Some(Action::Close(task.task_id));
        }
        merge_menu(ui, task.task_id, candidates, pending);
    });
}

fn closed_row(ui: &mut egui::Ui, task: &OpenRow, pending: &mut Option<Action>) {
    ui.horizontal(|ui| {
        ui.add_space(30.0);
        ui.label(&task.label);
        project_badge(ui, &task.project);
        declared_badge(ui, task.declared);
        if ui.small_button("reopen").clicked() {
            *pending = Some(Action::Reopen(task.task_id));
        }
    });
}

/// "merge into" target picker: folds this task into the chosen one
/// (a 'merge' correction — see storage::merge_task).
fn merge_menu(
    ui: &mut egui::Ui,
    self_task: i64,
    candidates: &[(i64, String)],
    pending: &mut Option<Action>,
) {
    ui.menu_button("merge into", |ui| {
        for (task_id, label) in candidates {
            if *task_id == self_task {
                continue;
            }
            if ui.button(label).clicked() {
                *pending = Some(Action::Merge {
                    from_task: self_task,
                    to_task: *task_id,
                });
                ui.close();
            }
        }
    });
}

fn span_row(ui: &mut egui::Ui, span: &SpanRow) {
    let time = format!(
        "{}\u{2013}{}",
        span.start.strftime("%H:%M:%S"),
        span.end.strftime("%H:%M:%S")
    );
    let dur =
        fmt_dur(span.end.timestamp().as_millisecond() - span.start.timestamp().as_millisecond());
    ui.horizontal(|ui| {
        ui.monospace(time);
        ui.weak(format!("{dur:>7}"));
        match span.kind.as_str() {
            "focus" => {
                ui.strong(&span.app);
                ui.label(&span.title);
            }
            other => {
                ui.weak(other);
            }
        }
    });
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
