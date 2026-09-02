//! Timeline view: widget-style "Today" face — focus-total header, activity
//! band, task cards, detail pane.

use eframe::egui;
use jiff::{ToSpan, Zoned};

use super::{Action, EditState, SpanRow, TaskGroup, TimelineApp, WorkspaceEdit, fmt_dur, theme};

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
        let narrow = ui.ctx().viewport_rect().width() < 700.0;
        if let Some(sel) = self.selected_task {
            match self.groups.iter().position(|g| g.task_id == sel) {
                Some(gi) => {
                    let group = &self.groups[gi];
                    let edit = &mut self.edit;
                    let ws_edit = &mut self.ws_edit;
                    let mut close_detail = false;
                    let color = theme::series_color_for(group.task_id);
                    if narrow {
                        // Actions pinned to the widget's bottom edge; the
                        // bottom panel must be added before the CentralPanel.
                        let frame = egui::Frame::new()
                            .fill(theme::palette::SURFACE)
                            .inner_margin(egui::Margin::symmetric(12, 8));
                        egui::Panel::bottom("task_detail_actions")
                            .frame(frame)
                            .show(ui, |ui| {
                                ui.multiply_opacity(detail_t);
                                detail_actions(ui, group, edit, &candidates, &mut pending);
                            });
                        egui::CentralPanel::default().show(ui, |ui| {
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
                                &candidates,
                                &mut pending,
                            );
                            ui.add_space(10.0);
                            detail_actions(ui, group, edit, &candidates, &mut pending);
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
            let selected_task = &mut self.selected_task;
            let show_background = &mut self.show_background;
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
                        activity_band(ui, content_w, groups, &band_vis, lo, hi, day_start);
                    }
                    ui.add_space(4.0);
                    for &g in &group_vis {
                        task_card(
                            ui,
                            content_w,
                            &groups[g],
                            theme::series_color_for(groups[g].task_id),
                            edit,
                            selected_task,
                            &candidates,
                            &mut pending,
                        );
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
                                    theme::series_color_for(groups[g].task_id),
                                    edit,
                                    selected_task,
                                    &candidates,
                                    &mut pending,
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

/// Horizontal day strip: one colored segment per display session (task
/// identity color), background showing through = away. Hour labels below.
fn activity_band(
    ui: &mut egui::Ui,
    width: f32,
    groups: &[TaskGroup],
    vis: &[usize],
    lo: i64,
    hi: i64,
    day_start: &Zoned,
) {
    const HOUR_MS: i64 = 3_600_000;
    // (clamped start, clamped end, group index, session index).
    let mut segments: Vec<(i64, i64, usize, usize)> = Vec::new();
    let (mut act_lo, mut act_hi) = (i64::MAX, i64::MIN);
    for &g in vis {
        for (si, s) in groups[g].sessions.iter().enumerate() {
            let s_lo = s.start.timestamp().as_millisecond().max(lo);
            let s_hi = s.end.timestamp().as_millisecond().min(hi);
            if s_hi <= s_lo {
                continue;
            }
            act_lo = act_lo.min(s_lo);
            act_hi = act_hi.max(s_hi);
            segments.push((s_lo, s_hi, g, si));
        }
    }
    if segments.is_empty() {
        return;
    }
    // Pad the shown range out to whole local hours.
    let band_lo = lo + (act_lo - lo) / HOUR_MS * HOUR_MS;
    let band_hi = (lo + (act_hi - lo + HOUR_MS - 1) / HOUR_MS * HOUR_MS).min(hi);
    let span = (band_hi - band_lo).max(1) as f32;

    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, 22.0), egui::Sense::hover());
    // Pointer x → ms, the inverse of the segment placement below, so the
    // tooltip always describes the segment actually under the cursor.
    let hover_ms = resp
        .hover_pos()
        .map(|p| band_lo + ((p.x - rect.left()) / rect.width() * span) as i64);
    let hovered_seg = hover_ms.and_then(|ms| {
        segments
            .iter()
            .position(|&(s_lo, s_hi, _, _)| ms >= s_lo && ms < s_hi)
    });
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::same(7), theme::palette::SURFACE);
    for (i, (s_lo, s_hi, g, _)) in segments.iter().enumerate() {
        let x0 = rect.left() + (s_lo - band_lo) as f32 / span * rect.width();
        let x1 = rect.left() + (s_hi - band_lo) as f32 / span * rect.width();
        let hovered = hovered_seg == Some(i);
        // Hovered segment brightens and grows into the band's 3px padding.
        let grow = 3.0
            * ui.ctx()
                .animate_bool_with_time(resp.id.with(i), hovered, 0.08);
        let seg = egui::Rect::from_min_max(
            egui::pos2(x0, rect.top() + 4.0 - grow),
            egui::pos2(x1.max(x0 + 2.0), rect.bottom() - 4.0 + grow),
        );
        let color = theme::series_color_for(groups[*g].task_id);
        let color = if hovered {
            color.gamma_multiply(1.2)
        } else {
            color
        };
        painter.rect_filled(seg, egui::CornerRadius::same(3), color);
    }
    if let Some(i) = hovered_seg {
        let (s_lo, s_hi, g, si) = segments[i];
        let group = &groups[g];
        let s = &group.sessions[si];
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        resp.on_hover_ui_at_pointer(|ui| {
            ui.set_max_width(220.0);
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(
                    dot.center(),
                    3.0,
                    theme::series_color_for(group.task_id),
                );
                ui.add(egui::Label::new(&group.label).truncate());
            });
            ui.weak(format!(
                "{}\u{2013}{} \u{b7} {}",
                s.start.strftime("%H:%M"),
                s.end.strftime("%H:%M"),
                fmt_dur(s_hi - s_lo)
            ));
        });
    }

    // Hour tick labels, thinned to at most ~6.
    let hours = ((band_hi - band_lo) / HOUR_MS).max(1);
    let step = (hours + 5) / 6;
    let (label_rect, _) = ui.allocate_exact_size(egui::vec2(width, 14.0), egui::Sense::hover());
    let painter = ui.painter();
    let first_hour = (band_lo - lo) / HOUR_MS;
    // Too narrow for even one label: clamp below would panic (min > max).
    let (label_min, label_max) = (label_rect.left() + 14.0, label_rect.right() - 14.0);
    if label_min > label_max {
        return;
    }
    for k in (0..=hours).step_by(step as usize) {
        let ms = band_lo + k * HOUR_MS;
        let Ok(z) = day_start.checked_add((first_hour + k).hours()) else {
            continue;
        };
        let x = label_rect.left() + (ms - band_lo) as f32 / span * label_rect.width();
        painter.text(
            egui::pos2(x.clamp(label_min, label_max), label_rect.top()),
            egui::Align2::CENTER_TOP,
            z.strftime("%H:%M").to_string(),
            theme::caption().resolve(ui.style()),
            theme::palette::TEXT_DIM,
        );
    }
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
    selected_task: &mut Option<i64>,
    candidates: &[(i64, String)],
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
                    candidates,
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
    candidates: &[(i64, String)],
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
            ui.horizontal(|ui| {
                let (dot, _) = ui.allocate_exact_size(egui::vec2(8.0, 8.0), egui::Sense::hover());
                ui.painter().circle_filled(dot.center(), 4.0, color);
                // Leave room for the duration + confidence dot + menu.
                let label_w = (ui.available_width() - 110.0).max(60.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(label_w, 18.0),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        theme::truncated_label(
                            ui,
                            egui::Label::new(
                                egui::RichText::new(&group.label)
                                    .family(egui::FontFamily::Name(theme::MEDIUM.into()))
                                    .color(theme::palette::TEXT),
                            )
                            .truncate(),
                            &group.label,
                        );
                    },
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    card_menu(ui, group, edit, candidates, pending);
                    confidence_dot(ui, group);
                    ui.label(
                        egui::RichText::new(fmt_dur(group.total_ms))
                            .text_style(egui::TextStyle::Small)
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
                if let (Some(first), Some(last)) = (group.sessions.first(), group.sessions.last()) {
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
            theme::ai_summary_line(ui, group.ai_summary.as_deref(), false);
        });
}

/// Detail pane: identity, summary, session chips (with whole-session move),
/// per-app evidence bars, and correction actions. Returns true to close.
fn detail_ui(
    ui: &mut egui::Ui,
    group: &TaskGroup,
    color: egui::Color32,
    edit: &mut Option<EditState>,
    ws_edit: &mut Option<WorkspaceEdit>,
    candidates: &[(i64, String)],
    pending: &mut Option<Action>,
) -> bool {
    let mut close = false;
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
    ui.horizontal(|ui| {
        if let Some(project) = &group.project {
            theme::badge(ui, project, color);
        }
        if let Some(external_ref) = &group.external_ref {
            theme::badge(ui, external_ref, theme::palette::TEXT_DIM);
        }
        if group.declared {
            theme::badge(ui, "declared", theme::palette::TEXT_DIM);
        }
    });
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

    ui.add_space(8.0);
    theme::section_header(ui, "Sessions", None);
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
        theme::section_header(ui, "Where the time went", None);
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

    if !group.commits.is_empty() {
        ui.add_space(8.0);
        theme::section_header(ui, "Commits", None);
        for c in &group.commits {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(c.time.strftime("%H:%M").to_string())
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT_DIM),
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&c.summary)
                            .text_style(egui::TextStyle::Small)
                            .color(theme::palette::TEXT),
                    )
                    .truncate(),
                );
            });
        }
    }

    if let Some(cp) = &group.checkpoint {
        ui.add_space(8.0);
        theme::section_header(ui, "Checkpoint", None);
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
        ui.add_space(8.0);
        theme::section_header(ui, "Journal", Some(group.journal.len()));
        for (id, time, entry) in &group.journal {
            let editing = matches!(ws_edit,
                Some(WorkspaceEdit::Journal { entry_id, .. }) if entry_id == id);
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
                ui.label(
                    egui::RichText::new(time)
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT_DIM),
                );
                // Click an entry to correct it in place.
                let resp = ui
                    .add(
                        egui::Label::new(
                            egui::RichText::new(entry)
                                .text_style(egui::TextStyle::Small)
                                .color(theme::palette::TEXT),
                        )
                        .wrap()
                        .sense(egui::Sense::click()),
                    )
                    .on_hover_text("edit");
                if resp.clicked() {
                    *ws_edit = Some(WorkspaceEdit::Journal {
                        entry_id: *id,
                        text: entry.clone(),
                    });
                }
            });
        }
    }

    if let Some((fetched_ts, content)) = &group.task_context {
        ui.add_space(8.0);
        theme::section_header(ui, "Context", None);
        let fetched = chronicle_core::types::ms_to_ts(*fetched_ts)
            .to_zoned(jiff::tz::TimeZone::system())
            .strftime("%b %-d %H:%M")
            .to_string();
        ui.label(
            egui::RichText::new(format!("fetched {fetched}"))
                .text_style(egui::TextStyle::Small)
                .color(theme::palette::TEXT_DIM),
        );
        egui::CollapsingHeader::new(
            egui::RichText::new(context_preview(content))
                .text_style(egui::TextStyle::Small)
                .color(theme::palette::TEXT),
        )
        .id_salt(("task_context", group.task_id))
        .show(ui, |ui| {
            ui.label(
                egui::RichText::new(content)
                    .text_style(egui::TextStyle::Small)
                    .color(theme::palette::TEXT),
            );
        });
    }

    close
}

/// First line of the context bundle, clipped, as the collapsed summary.
fn context_preview(content: &str) -> String {
    let line = content.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
    let mut p: String = line.chars().take(60).collect();
    if p.len() < line.len() {
        p.push('\u{2026}');
    }
    p
}

/// Correction actions for the selected task; rendered pinned to the widget's
/// bottom edge in narrow mode, inline at the pane's end when wide.
fn detail_actions(
    ui: &mut egui::Ui,
    group: &TaskGroup,
    edit: &mut Option<EditState>,
    candidates: &[(i64, String)],
    pending: &mut Option<Action>,
) {
    ui.horizontal(|ui| {
        if ui.button("chat").clicked() {
            *pending = Some(Action::ChatAboutTask(group.task_id));
        }
        if ui.button("rename").clicked() {
            *edit = Some(EditState {
                task_id: group.task_id,
                label: group.label.clone(),
                project: group.project.clone().unwrap_or_default(),
                description: group.ai_summary.clone().unwrap_or_default(),
            });
        }
        merge_menu(ui, group.task_id, candidates, pending);
        if group.external_ref.is_some() {
            let label = if group.context_pending {
                "fetching\u{2026}"
            } else if group.task_context.is_some() {
                "re-fetch context"
            } else {
                "fetch context"
            };
            if ui
                .add_enabled(!group.context_pending, egui::Button::new(label))
                .clicked()
            {
                *pending = Some(Action::FetchContext(group.task_id));
            }
        }
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
                description: group.ai_summary.clone().unwrap_or_default(),
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

/// "merge into" target picker: folds this task into the chosen one
/// (a 'merge' correction — see storage::merge_task).
pub(super) fn merge_menu(
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
