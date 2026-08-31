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
                week_chart(ui, r, &today);
                ui.add_space(14.0);
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
                            let color = theme::series_color_for(t.task_id);
                            ui.horizontal(|ui| {
                                ui.set_max_width(220.0);
                                let (dot, _) = ui.allocate_exact_size(
                                    egui::vec2(8.0, 8.0),
                                    egui::Sense::hover(),
                                );
                                ui.painter().circle_filled(dot.center(), 4.0, color);
                                ui.add(egui::Label::new(&t.label).truncate());
                                if t.project != chronicle_core::report::UNTAGGED {
                                    theme::badge(ui, &t.project, color);
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

/// Stacked per-day bars in task identity colors; today's label accented.
fn week_chart(ui: &mut egui::Ui, r: &chronicle_core::report::RangeReport, today: &jiff::civil::Date) {
    let day_totals: Vec<i64> = (0..r.days.len())
        .map(|d| r.tasks.iter().map(|t| t.by_day[d]).sum())
        .collect();
    let max_ms = day_totals.iter().copied().max().unwrap_or(0);
    if max_ms == 0 {
        return;
    }
    const CHART_H: f32 = 110.0;
    const LABEL_H: f32 = 16.0;
    let width = ui.available_width().min(680.0);
    let (rect, _) = ui.allocate_exact_size(
        egui::vec2(width, CHART_H + LABEL_H + 14.0),
        egui::Sense::hover(),
    );
    let painter = ui.painter();
    let slot = rect.width() / r.days.len() as f32;
    let bar_w = (slot * 0.55).min(48.0);
    for (d, day) in r.days.iter().enumerate() {
        let cx = rect.left() + slot * (d as f32 + 0.5);
        let base = rect.top() + CHART_H;
        // Stack biggest-task-first, bottom-up.
        let mut y = base;
        for t in &r.tasks {
            let ms = t.by_day[d];
            if ms == 0 {
                continue;
            }
            let h = (ms as f32 / max_ms as f32 * (CHART_H - 16.0)).max(1.0);
            let seg = egui::Rect::from_min_max(
                egui::pos2(cx - bar_w / 2.0, y - h),
                egui::pos2(cx + bar_w / 2.0, y - 1.0),
            );
            painter.rect_filled(seg, egui::CornerRadius::same(2), theme::series_color_for(t.task_id));
            y -= h;
        }
        if day_totals[d] > 0 {
            painter.text(
                egui::pos2(cx, y - 4.0),
                egui::Align2::CENTER_BOTTOM,
                fmt_dur(day_totals[d]),
                egui::FontId::new(10.0, egui::FontFamily::Proportional),
                theme::palette::TEXT_DIM,
            );
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
            egui::FontId::new(11.0, egui::FontFamily::Proportional),
            label_color,
        );
    }
}
