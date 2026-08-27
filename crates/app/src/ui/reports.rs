//! Reports view: week-at-a-glance per-task/per-day table.

use eframe::egui;
use jiff::Zoned;

use super::{TimelineApp, fmt_dur, theme};

impl TimelineApp {
    pub(super) fn reports_ui(&mut self, ui: &mut egui::Ui) {
        egui::CentralPanel::default().show(ui, |ui| {
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
            let num_cell = |ui: &mut egui::Ui, ms: i64, strong: bool| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ms == 0 {
                        ui.weak("\u{b7}");
                    } else if strong {
                        ui.monospace(egui::RichText::new(fmt_dur(ms)).color(theme::palette::TEXT));
                    } else {
                        ui.weak(egui::RichText::new(fmt_dur(ms)).monospace());
                    }
                });
            };
            egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                ui.label(
                    egui::RichText::new("Tasks")
                        .text_style(egui::TextStyle::Heading)
                        .color(theme::palette::TEXT),
                );
                ui.add_space(4.0);
                egui::Grid::new("week_report")
                    .striped(true)
                    .min_col_width(48.0)
                    .show(ui, |ui| {
                        ui.label("");
                        for d in &r.days {
                            let name = d.strftime("%a").to_string();
                            let text = if *d == today {
                                egui::RichText::new(name).color(theme::palette::ACCENT)
                            } else {
                                egui::RichText::new(name).color(theme::palette::TEXT_DIM)
                            };
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    ui.label(text);
                                },
                            );
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(egui::RichText::new("total").color(theme::palette::TEXT_DIM));
                        });
                        ui.end_row();
                        for t in &r.tasks {
                            ui.horizontal(|ui| {
                                ui.set_max_width(220.0);
                                ui.add(egui::Label::new(&t.label).truncate());
                                if t.project != chronicle_core::report::UNTAGGED {
                                    theme::badge(ui, &t.project, theme::palette::ACCENT);
                                }
                            });
                            for ms in &t.by_day {
                                num_cell(ui, *ms, false);
                            }
                            num_cell(ui, t.total_ms, true);
                            ui.end_row();
                        }
                    });
                if r.tasks.is_empty() {
                    ui.weak("no tasks this week");
                }
                ui.add_space(14.0);
                ui.label(
                    egui::RichText::new("Projects")
                        .text_style(egui::TextStyle::Heading)
                        .color(theme::palette::TEXT),
                );
                ui.add_space(4.0);
                egui::Grid::new("week_projects")
                    .striped(true)
                    .min_col_width(48.0)
                    .show(ui, |ui| {
                        for p in &r.projects {
                            num_cell(ui, p.total_ms, false);
                            theme::badge(ui, &p.project, theme::palette::ACCENT);
                            ui.end_row();
                        }
                        num_cell(ui, r.grand_total_ms, true);
                        ui.label(
                            egui::RichText::new("total")
                                .text_style(egui::TextStyle::Heading)
                                .color(theme::palette::TEXT),
                        );
                        ui.end_row();
                    });
            });
        });
    }
}
