//! Home view: input dashboard — declare a task, manage open and recently
//! closed tasks, and the raw-spans debug list.

use eframe::egui;

use super::timeline::{matches_filter, merge_menu};
use super::{Action, OpenRow, SpanRow, TimelineApp, fmt_dur, theme};

impl TimelineApp {
    /// "Where you left off" card: newest checkpoint since the last UI open.
    fn resume_card_ui(&mut self, ui: &mut egui::Ui) {
        let Some(resume) = &self.resume else {
            return;
        };
        let mut open = false;
        let mut dismiss = false;
        theme::hover_card(ui, "resume_card", |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Where you left off")
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT_DIM),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if theme::ghost_button(ui, "\u{d7}").clicked() {
                        dismiss = true;
                    }
                });
            });
            ui.horizontal(|ui| {
                ui.label(egui::RichText::new(&resume.label).strong());
                if let Some(external_ref) = &resume.external_ref {
                    theme::badge(ui, external_ref, theme::palette::TEXT_DIM);
                }
            });
            ui.label(
                egui::RichText::new(&resume.state)
                    .text_style(egui::TextStyle::Small)
                    .color(theme::palette::TEXT),
            );
            ui.label(
                egui::RichText::new(format!("Next: {}", resume.next_steps))
                    .text_style(egui::TextStyle::Small)
                    .color(theme::palette::TEXT_DIM),
            );
            ui.add_space(theme::SPACE_XS);
            if theme::secondary_button(ui, "open workspace").clicked() {
                open = true;
            }
        });
        ui.add_space(theme::CARD_GAP);
        if open {
            let resume = self.resume.take().expect("checked above");
            self.selected_task = Some(resume.task_id);
            // The checkpointed work may be yesterday's — show its day.
            self.day = chronicle_core::types::ms_to_ts(resume.ts)
                .to_zoned(self.tz.clone())
                .date();
            self.loaded_at = None;
            self.view = super::View::Timeline;
        } else if dismiss {
            self.resume = None;
        }
    }

    /// Standup card: yesterday's drafted update, a spinner while drafting,
    /// or a lone draft button. Returns true when (re)drafting was clicked.
    fn standup_card_ui(&mut self, ui: &mut egui::Ui) -> bool {
        let mut generate = false;
        if self.standup.is_none() && self.standup_job.is_none() {
            if !self.model_missing
                && ui
                    .small_button("draft standup")
                    .on_hover_text("draft a standup from yesterday's journals")
                    .clicked()
            {
                generate = true;
            }
            self.standup_error_ui(ui);
            ui.add_space(4.0);
            return generate;
        }
        theme::hover_card(ui, "standup_card", |ui| {
            let day = self
                .standup
                .as_ref()
                .map(|s| format!("Standup \u{b7} {}", s.day))
                .unwrap_or_else(|| "Standup".to_owned());
            let mut open = self.standup_open;
            theme::disclosure_header(ui, &mut open, &day, None);
            self.standup_open = open;
            theme::fade_body(ui, "standup_body", self.standup_open, |ui| {
                if self.standup_job.is_some() {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(12.0));
                        ui.weak("drafting from yesterday's journals\u{2026}");
                    });
                } else if let Some(standup) = &self.standup {
                    // Long drafts (7-task fallback days) otherwise push the
                    // whole task list off a 640px window.
                    egui::ScrollArea::vertical()
                        .id_salt("standup_draft")
                        .max_height(220.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&standup.content)
                                        .text_style(egui::TextStyle::Small)
                                        .color(theme::palette::TEXT),
                                )
                                .wrap(),
                            );
                        });
                    ui.add_space(4.0);
                    if !self.model_missing && ui.small_button("redraft").clicked() {
                        generate = true;
                    }
                    self.standup_error_ui(ui);
                }
            });
        });
        ui.add_space(theme::CARD_GAP);
        generate
    }

    /// Reason the last standup job failed, if any.
    fn standup_error_ui(&self, ui: &mut egui::Ui) {
        if let Some(err) = &self.standup_error {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!("couldn't draft: {err}"))
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::AMBER),
                )
                .wrap(),
            );
        }
    }

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
            self.resume_card_ui(ui);
            if self.standup_card_ui(ui) {
                pending = Some(Action::GenerateStandup);
            }
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
            let suggestion = &self.suggestion;
            let model_missing = self.model_missing;
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    theme::section_header(ui, "Working on", None);
                    ui.add_space(6.0);
                    // One column plan shared by the declare row and every task row,
                    // so the whole section reads as a single aligned table.
                    // Every column is a fixed-width `cell` — including the
                    // action column — so Grid can never auto-widen past 400px.
                    let label_w = (theme::content_width(ui)
                        - PROJECT_COL
                        - STATUS_COL
                        - ACTION_COL
                        - 3.0 * 8.0)
                        .max(120.0);
                    egui::Grid::new("working_on")
                        .num_columns(4)
                        .striped(true)
                        .spacing([8.0, 6.0])
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
                            cell(ui, ACTION_COL, |ui| {
                                if theme::primary_button(ui, "add").clicked() {
                                    pending = Some(Action::Declare);
                                }
                            });
                            ui.end_row();
                            for &o in &open_vis {
                                let t = &open_tasks[o];
                                task_cells(ui, t, label_w, true);
                                cell(ui, ACTION_COL, |ui| {
                                    ui.menu_button("\u{2026}", |ui| {
                                        if ui.button("close").clicked() {
                                            pending = Some(Action::Close(t.task_id));
                                            ui.close();
                                        }
                                        merge_menu(ui, t.task_id, &candidates, &mut pending);
                                    });
                                });
                                ui.end_row();
                            }
                        });
                    // AI declare-suggestion: button → spinner → dismissible
                    // chip whose "use" pre-fills the declare inputs.
                    if !model_missing {
                        ui.add_space(4.0);
                        match suggestion {
                            None => {
                                if ui
                                    .small_button("suggest")
                                    .on_hover_text("suggest a task from the last 15 minutes")
                                    .clicked()
                                {
                                    pending = Some(Action::SuggestTask);
                                }
                            }
                            Some(super::SuggestionState::Pending(_)) => {
                                ui.horizontal(|ui| {
                                    ui.add(egui::Spinner::new().size(12.0));
                                    ui.weak("reading the last 15 minutes\u{2026}");
                                });
                            }
                            Some(super::SuggestionState::Failed(msg)) => {
                                ui.horizontal(|ui| {
                                    ui.weak(msg.as_str());
                                    if ui.small_button("\u{d7}").clicked() {
                                        pending = Some(Action::DismissSuggestion);
                                    }
                                });
                            }
                            Some(super::SuggestionState::Ready(s)) => {
                                egui::Frame::new()
                                    .fill(theme::palette::ACCENT.gamma_multiply(0.10))
                                    .stroke(egui::Stroke::new(
                                        1.0,
                                        theme::palette::ACCENT.gamma_multiply(0.35),
                                    ))
                                    .corner_radius(egui::CornerRadius::same(theme::RADIUS_MD))
                                    .inner_margin(egui::Margin::same(8))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.horizontal(|ui| {
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(&s.label)
                                                        .color(theme::palette::TEXT),
                                                )
                                                .truncate(),
                                            );
                                            if let Some(p) = &s.project {
                                                theme::badge(ui, p, theme::palette::ACCENT);
                                            }
                                        });
                                        if let Some(d) = &s.description {
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(d)
                                                        .text_style(egui::TextStyle::Small)
                                                        .color(theme::palette::TEXT_DIM),
                                                )
                                                .wrap(),
                                            );
                                        }
                                        ui.horizontal(|ui| {
                                            if ui.small_button("use").clicked() {
                                                pending = Some(Action::UseSuggestion);
                                            }
                                            if ui.small_button("dismiss").clicked() {
                                                pending = Some(Action::DismissSuggestion);
                                            }
                                        });
                                    });
                            }
                        }
                    }

                    if !closed_vis.is_empty() {
                        ui.add_space(theme::SPACE_XS);
                        theme::disclosure_header(
                            ui,
                            show_closed,
                            "recently closed",
                            Some(closed_vis.len()),
                        );
                        theme::fade_body(ui, "recently_closed_body", *show_closed, |ui| {
                            egui::Grid::new("recently_closed")
                                .num_columns(4)
                                .striped(true)
                                .spacing([8.0, 6.0])
                                .show(ui, |ui| {
                                    for &c in &closed_vis {
                                        let t = &closed_tasks[c];
                                        task_cells(ui, t, label_w, false);
                                        cell(ui, ACTION_COL, |ui| {
                                            if ui.small_button("reopen").clicked() {
                                                pending = Some(Action::Reopen(t.task_id));
                                            }
                                        });
                                        ui.end_row();
                                    }
                                });
                        });
                    }

                    // Debug-grade raw spans; hidden unless enabled in settings.
                    if spans_debug {
                        ui.add_space(theme::SPACE_XS);
                        theme::disclosure_header(ui, show_spans, "Spans", Some(span_vis.len()));
                        theme::fade_body(ui, "spans_body", *show_spans, |ui| {
                            for &s in &span_vis {
                                span_row(ui, &spans[s]);
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

/// Fixed column widths shared by both task grids (and the declare row), so
/// badges line up regardless of label length.
const PROJECT_COL: f32 = 84.0;
const STATUS_COL: f32 = 64.0;
/// Widest 4th-column content across both grids ("reopen" small button).
const ACTION_COL: f32 = 56.0;
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
        theme::truncated_label(ui, egui::Label::new(text).truncate(), &task.label);
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
