//! Home view: input dashboard — declare a task, manage open and recently
//! closed tasks, and the raw-spans debug list.

use eframe::egui;

use super::timeline::{matches_filter, merge_menu};
use super::{Action, OpenRow, SpanRow, TimelineApp, fmt_dur, theme};

impl TimelineApp {
    pub(super) fn home_ui(&mut self, ui: &mut egui::Ui) {
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
        let span_vis: Vec<usize> = (0..self.spans.len())
            .filter(|&s| {
                let sp = &self.spans[s];
                matches_filter(&q, &sp.title, Some(&sp.app))
            })
            .collect();
        let candidates = self.merge_candidates();

        let mut pending: Option<Action> = None;
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
            let open_tasks = &self.open_tasks;
            let closed_tasks = &self.closed_tasks;
            let show_closed = &mut self.show_closed;
            let show_spans = &mut self.show_spans;
            let spans = &self.spans;
            let new_label = &mut self.new_label;
            let new_project = &mut self.new_project;
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                theme::section_header(ui, "Working on", None);
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
                        let arrow = if *show_closed { "\u{25bc}" } else { "\u{25b6}" };
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
                    let arrow = if *show_spans { "\u{25bc}" } else { "\u{25b6}" };
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
