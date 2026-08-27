//! Timeline view: virtual-scrolled day breakdown (working-on list, project
//! totals, task groups with intervals, raw spans).

use eframe::egui;

use super::{
    Action, EditState, IntervalRow, OpenRow, SpanRow, TaskGroup, TimelineApp, fmt_dur, theme,
};

impl TimelineApp {
    pub(super) fn timeline_ui(&mut self, ui: &mut egui::Ui) {
        // Flat row list for the virtual scroller: working-on section, then
        // the day's tasks grouped with their intervals, then raw spans.
        enum RowKind {
            WorkingHeader,
            DeclareForm,
            Open(usize),
            ClosedToggle,
            Closed(usize),
            ProjectsHeader,
            Project(usize),
            TasksHeader,
            TaskHeader(usize),
            Interval(usize, usize),
            SpansHeader,
            Span(usize),
        }
        let mut rows: Vec<RowKind> = vec![RowKind::WorkingHeader, RowKind::DeclareForm];
        rows.extend((0..self.open_tasks.len()).map(RowKind::Open));
        if !self.closed_tasks.is_empty() {
            rows.push(RowKind::ClosedToggle);
            if self.show_closed {
                rows.extend((0..self.closed_tasks.len()).map(RowKind::Closed));
            }
        }
        // Per-project totals for the shown day, biggest first ("(none)" =
        // untagged); sums the same group totals the Tasks section shows.
        let mut projects: Vec<(String, i64)> = Vec::new();
        for g in &self.groups {
            let name = g.project.clone().unwrap_or_else(|| "(none)".into());
            match projects.iter_mut().find(|(n, _)| *n == name) {
                Some((_, ms)) => *ms += g.total_ms,
                None => projects.push((name, g.total_ms)),
            }
        }
        projects.sort_by_key(|&(_, ms)| std::cmp::Reverse(ms));
        if !projects.is_empty() {
            rows.push(RowKind::ProjectsHeader);
            rows.extend((0..projects.len()).map(RowKind::Project));
        }
        rows.push(RowKind::TasksHeader);
        for (g, group) in self.groups.iter().enumerate() {
            rows.push(RowKind::TaskHeader(g));
            rows.extend((0..group.intervals.len()).map(|i| RowKind::Interval(g, i)));
        }
        rows.push(RowKind::SpansHeader);
        rows.extend((0..self.spans.len()).map(RowKind::Span));

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

        let mut pending: Option<Action> = None;
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(warning) = &self.warning {
                ui.colored_label(ui.visuals().warn_fg_color, warning);
            }
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
                return;
            }
            let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
            let groups = &self.groups;
            let open_tasks = &self.open_tasks;
            let closed_tasks = &self.closed_tasks;
            let show_closed = &mut self.show_closed;
            let spans = &self.spans;
            let edit = &mut self.edit;
            let new_label = &mut self.new_label;
            let new_project = &mut self.new_project;
            egui::ScrollArea::vertical().auto_shrink(false).show_rows(
                ui,
                row_height,
                rows.len(),
                |ui, range| {
                    for i in range {
                        match rows[i] {
                            RowKind::WorkingHeader => {
                                ui.strong("Working on");
                            }
                            RowKind::DeclareForm => {
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(new_label)
                                            .desired_width(240.0)
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
                            }
                            RowKind::Open(o) => {
                                open_row(ui, &open_tasks[o], &candidates, &mut pending)
                            }
                            RowKind::ClosedToggle => {
                                ui.horizontal(|ui| {
                                    ui.add_space(12.0);
                                    let arrow = if *show_closed { "\u{25be}" } else { "\u{25b8}" };
                                    if ui
                                        .small_button(format!("{arrow} recently closed"))
                                        .clicked()
                                    {
                                        *show_closed = !*show_closed;
                                    }
                                });
                            }
                            RowKind::Closed(c) => closed_row(ui, &closed_tasks[c], &mut pending),
                            RowKind::ProjectsHeader => {
                                ui.strong("Projects");
                            }
                            RowKind::Project(p) => {
                                let (name, ms) = &projects[p];
                                ui.horizontal(|ui| {
                                    ui.weak(format!("{:>7}", fmt_dur(*ms)));
                                    ui.label(name);
                                });
                            }
                            RowKind::TasksHeader => {
                                if groups.is_empty() {
                                    ui.horizontal(|ui| {
                                        ui.strong("Tasks");
                                        ui.weak("none derived yet");
                                    });
                                } else {
                                    ui.strong("Tasks");
                                }
                            }
                            RowKind::TaskHeader(g) => {
                                task_header_row(ui, &groups[g], edit, &candidates, &mut pending)
                            }
                            RowKind::Interval(g, iv) => interval_row(
                                ui,
                                &groups[g],
                                &groups[g].intervals[iv],
                                &candidates,
                                &mut pending,
                            ),
                            RowKind::SpansHeader => {
                                ui.strong("Spans");
                            }
                            RowKind::Span(s) => span_row(ui, &spans[s]),
                        }
                    }
                },
            );
        });
        if let Some(action) = pending {
            self.apply_action(action);
        }
    }
}

fn open_row(
    ui: &mut egui::Ui,
    task: &OpenRow,
    candidates: &[(i64, String)],
    pending: &mut Option<Action>,
) {
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.strong(&task.label);
        if let Some(project) = &task.project {
            ui.label(project);
        }
        if task.declared {
            ui.weak("(declared)");
        }
        if ui.small_button("close").clicked() {
            *pending = Some(Action::Close(task.task_id));
        }
        merge_menu(ui, task.task_id, candidates, pending);
    });
}

fn closed_row(ui: &mut egui::Ui, task: &OpenRow, pending: &mut Option<Action>) {
    ui.horizontal(|ui| {
        ui.add_space(24.0);
        ui.label(&task.label);
        if let Some(project) = &task.project {
            ui.weak(project);
        }
        if task.declared {
            ui.weak("(declared)");
        }
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

/// Group header: identity, total time, rename edit (writes a correction).
fn task_header_row(
    ui: &mut egui::Ui,
    group: &TaskGroup,
    edit: &mut Option<EditState>,
    candidates: &[(i64, String)],
    pending: &mut Option<Action>,
) {
    ui.horizontal(|ui| {
        ui.weak(format!("{:>7}", fmt_dur(group.total_ms)));
        if edit.as_ref().is_some_and(|e| e.task_id == group.task_id) {
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
        } else {
            ui.strong(&group.label);
            if let Some(project) = &group.project {
                ui.label(project);
            }
            if group.declared {
                ui.weak("(declared)");
            }
            if ui.small_button("edit").clicked() {
                *edit = Some(EditState {
                    task_id: group.task_id,
                    label: group.label.clone(),
                    project: group.project.clone().unwrap_or_default(),
                });
            }
            merge_menu(ui, group.task_id, candidates, pending);
        }
    });
}

/// One interval under its task header; "move" reassigns it to another task
/// (a 'reassign' correction).
fn interval_row(
    ui: &mut egui::Ui,
    group: &TaskGroup,
    interval: &IntervalRow,
    candidates: &[(i64, String)],
    pending: &mut Option<Action>,
) {
    let time = format!(
        "{}\u{2013}{}",
        interval.start.strftime("%H:%M:%S"),
        interval.end.strftime("%H:%M:%S")
    );
    let dur = fmt_dur(
        interval.end.timestamp().as_millisecond() - interval.start.timestamp().as_millisecond(),
    );
    ui.horizontal(|ui| {
        ui.add_space(24.0);
        ui.monospace(time);
        ui.weak(format!("{dur:>7}"));
        let pct = format!("{:.0}%", interval.confidence * 100.0);
        match theme::confidence_color(theme::confidence_band(interval.confidence)) {
            Some(color) => theme::badge(ui, &pct, color),
            None => {
                ui.weak(pct);
            }
        }
        ui.menu_button("move", |ui| {
            for (task_id, label) in candidates {
                if *task_id == group.task_id {
                    continue;
                }
                if ui.button(label).clicked() {
                    *pending = Some(Action::Reassign {
                        interval_id: interval.interval_id,
                        to_task: *task_id,
                    });
                    ui.close();
                }
            }
        });
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
