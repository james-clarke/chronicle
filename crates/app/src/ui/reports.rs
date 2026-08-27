//! Reports view: week-at-a-glance per-task/per-day table.

use eframe::egui;

use super::{TimelineApp, fmt_dur};

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
            let cell =
                |ms: i64| format!("{:>7}", if ms == 0 { String::new() } else { fmt_dur(ms) });
            egui::ScrollArea::both().auto_shrink(false).show(ui, |ui| {
                ui.strong("Tasks");
                let mut header = String::new();
                for d in &r.days {
                    header.push_str(&format!("{:>7}", d.strftime("%a")));
                }
                header.push_str(&format!("{:>9}", "total"));
                ui.monospace(header);
                for t in &r.tasks {
                    ui.horizontal(|ui| {
                        let mut cells = String::new();
                        for ms in &t.by_day {
                            cells.push_str(&cell(*ms));
                        }
                        cells.push_str(&format!("{:>9}", fmt_dur(t.total_ms)));
                        ui.monospace(cells);
                        ui.label(&t.label);
                        if t.project != chronicle_core::report::UNTAGGED {
                            ui.weak(&t.project);
                        }
                    });
                }
                if r.tasks.is_empty() {
                    ui.weak("no tasks this week");
                }
                ui.add_space(8.0);
                ui.strong("Projects");
                for p in &r.projects {
                    ui.horizontal(|ui| {
                        ui.monospace(format!("{:>7}", fmt_dur(p.total_ms)));
                        ui.label(&p.project);
                    });
                }
                ui.horizontal(|ui| {
                    ui.monospace(format!("{:>7}", fmt_dur(r.grand_total_ms)));
                    ui.strong("total");
                });
            });
        });
    }
}
