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
            let spans_debug = self.spans_debug;
            let spans = &self.spans;
            let new_label = &mut self.new_label;
            let new_project = &mut self.new_project;
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    theme::section_header(ui, "Working on", None);
                    ui.add_space(6.0);
                    // One column plan shared by the declare row and every task row,
                    // so the whole section reads as a single aligned table.
                    let label_w = (ui.available_width() - 230.0).clamp(160.0, 460.0);
                    egui::Grid::new("working_on")
                        .num_columns(4)
                        .striped(true)
                        .spacing([10.0, 6.0])
                        .show(ui, |ui| {
                            ui.add(
                                egui::TextEdit::singleline(new_label)
                                    .desired_width(label_w)
                                    .hint_text("declare a task\u{2026}"),
                            );
                            ui.add(
                                egui::TextEdit::singleline(new_project)
                                    .desired_width(PROJECT_COL)
                                    .hint_text("project"),
                            );
                            ui.label("");
                            if ui.button("add").clicked() {
                                pending = Some(Action::Declare);
                            }
                            ui.end_row();
                            for &o in &open_vis {
                                let t = &open_tasks[o];
                                task_cells(ui, t, label_w, true);
                                ui.menu_button("\u{2026}", |ui| {
                                    if ui.button("close").clicked() {
                                        pending = Some(Action::Close(t.task_id));
                                        ui.close();
                                    }
                                    merge_menu(ui, t.task_id, &candidates, &mut pending);
                                });
                                ui.end_row();
                            }
                        });
                    if !closed_vis.is_empty() {
                        ui.add_space(4.0);
                        let arrow = if *show_closed { "\u{25bc}" } else { "\u{25b6}" };
                        if ui
                            .small_button(format!("{arrow} recently closed"))
                            .clicked()
                        {
                            *show_closed = !*show_closed;
                        }
                        if *show_closed {
                            egui::Grid::new("recently_closed")
                                .num_columns(4)
                                .striped(true)
                                .spacing([10.0, 6.0])
                                .show(ui, |ui| {
                                    for &c in &closed_vis {
                                        let t = &closed_tasks[c];
                                        task_cells(ui, t, label_w, false);
                                        if ui.small_button("reopen").clicked() {
                                            pending = Some(Action::Reopen(t.task_id));
                                        }
                                        ui.end_row();
                                    }
                                });
                        }
                    }

                    // Debug-grade raw spans; hidden unless enabled in settings.
                    if spans_debug {
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
                    }
                });
        });
        if let Some(action) = pending {
            self.apply_action(action);
        }
    }
}

/// Fixed column widths shared by both task grids (and the declare row), so
/// badges line up regardless of label length.
const PROJECT_COL: f32 = 96.0;
const STATUS_COL: f32 = 64.0;
const ROW_H: f32 = 20.0;

/// Left-aligned fixed-width cell; contents clip rather than widen the column.
fn cell(ui: &mut egui::Ui, w: f32, add: impl FnOnce(&mut egui::Ui)) {
    ui.allocate_ui_with_layout(
        egui::vec2(w, ROW_H),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.set_max_width(w);
            add(ui);
        },
    );
}

/// The three data cells of a task row: label, project badge, declared badge.
fn task_cells(ui: &mut egui::Ui, task: &OpenRow, label_w: f32, strong: bool) {
    cell(ui, label_w, |ui| {
        let text = egui::RichText::new(&task.label);
        let text = if strong { text.strong() } else { text };
        ui.add(egui::Label::new(text).truncate());
    });
    cell(ui, PROJECT_COL, |ui| {
        if let Some(project) = &task.project {
            theme::badge(ui, project, theme::palette::ACCENT);
        }
    });
    cell(ui, STATUS_COL, |ui| {
        if task.declared {
            theme::badge(ui, "declared", theme::palette::TEXT_DIM);
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
