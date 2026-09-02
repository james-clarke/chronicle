//! Home → Unassigned → "organize" takeover (m23): every unassigned focus run
//! of the shown day as a card with a task pick, pre-filled from past
//! corrections (FTS over the run's app/title mix). Assigning writes
//! confidence-1.0 intervals plus an 'assign' correction, so the time shows
//! under the task everywhere and the next derive sees the mapping.

use chronicle_core::storage::{self, UnassignedRun};
use chronicle_core::types::ms_to_ts;
use eframe::egui;
use jiff::Zoned;
use jiff::tz::TimeZone;
use rusqlite::Connection;

use super::{Action, TimelineApp, fmt_dur, theme};

/// Focus gap that still joins two spans into one run.
const RUN_GAP_MS: i64 = 5 * 60_000;
/// App/title lines shown per card.
const MAX_LINES: usize = 4;

pub(super) struct RunCard {
    run: UnassignedRun,
    start: Zoned,
    end: Zoned,
    selected: bool,
    pick: Option<i64>,
    /// Task a past correction points at (the pre-filled pick).
    suggested: Option<i64>,
}

pub(super) struct TriagePanel {
    cards: Vec<RunCard>,
    new_label: String,
    status: Option<Result<String, String>>,
    /// Set after an assign: cards reload on the next frame.
    pub(super) dirty: bool,
}

impl TriagePanel {
    pub(super) fn load(
        conn: &Connection,
        lo: i64,
        hi: i64,
        tz: &TimeZone,
        candidates: &[(i64, String)],
    ) -> anyhow::Result<Self> {
        let mut panel = Self {
            cards: Vec::new(),
            new_label: String::new(),
            status: None,
            dirty: false,
        };
        panel.reload(conn, lo, hi, tz, candidates)?;
        Ok(panel)
    }

    /// Rebuild the cards; picks and selection survive for runs that are
    /// still there (keyed by start).
    pub(super) fn reload(
        &mut self,
        conn: &Connection,
        lo: i64,
        hi: i64,
        tz: &TimeZone,
        candidates: &[(i64, String)],
    ) -> anyhow::Result<()> {
        let old: Vec<(i64, bool, Option<i64>)> = self
            .cards
            .iter()
            .map(|c| (c.run.start_ts, c.selected, c.pick))
            .collect();
        let mut cards = Vec::new();
        for run in storage::unassigned_runs(conn, lo, hi, RUN_GAP_MS)? {
            let text: String = run
                .lines
                .iter()
                .take(MAX_LINES)
                .map(|(app, title, _)| format!("{app} {title}"))
                .collect::<Vec<_>>()
                .join(" ");
            let suggested = storage::suggest_correction(conn, &text)?.and_then(|c| {
                candidates
                    .iter()
                    .find(|(_, label)| label.eq_ignore_ascii_case(&c.new_label))
                    .map(|(id, _)| *id)
            });
            let prior = old.iter().find(|(s, _, _)| *s == run.start_ts);
            cards.push(RunCard {
                start: ms_to_ts(run.start_ts).to_zoned(tz.clone()),
                end: ms_to_ts(run.end_ts).to_zoned(tz.clone()),
                selected: prior.is_some_and(|p| p.1),
                pick: prior.and_then(|p| p.2).or(suggested),
                suggested,
                run,
            });
        }
        self.cards = cards;
        self.dirty = false;
        Ok(())
    }

    pub(super) fn set_status(&mut self, status: Result<String, String>) {
        self.status = Some(status);
    }
}

impl TimelineApp {
    pub(super) fn open_triage(&mut self) {
        let candidates = self.merge_candidates();
        let Some(conn) = self.conn.as_ref() else {
            return;
        };
        let Ok((lo, hi)) = self.day_range_ms() else {
            return;
        };
        match TriagePanel::load(conn, lo, hi, &self.tz, &candidates) {
            Ok(panel) => self.triage = Some(panel),
            Err(e) => self.error = Some(format!("triage load failed: {e}")),
        }
    }

    /// Full-window takeover (like settings) while `self.triage` is Some.
    pub(super) fn triage_ui(&mut self, ui: &mut egui::Ui) {
        if self.triage.as_ref().is_some_and(|t| t.dirty) {
            let candidates = self.merge_candidates();
            if let (Some(conn), Ok((lo, hi))) = (self.conn.as_ref(), self.day_range_ms())
                && let Some(panel) = &mut self.triage
                && let Err(e) = panel.reload(conn, lo, hi, &self.tz, &candidates)
            {
                self.error = Some(format!("triage reload failed: {e}"));
            }
        }
        let candidates = self.merge_candidates();
        let label_of = |id: i64| -> String {
            candidates
                .iter()
                .find(|(c, _)| *c == id)
                .map(|(_, l)| l.clone())
                .unwrap_or_else(|| "?".into())
        };
        let mut close = false;
        let mut pending: Vec<Action> = Vec::new();
        let Some(panel) = &mut self.triage else {
            return;
        };
        let total_ms: i64 = panel.cards.iter().map(|c| c.run.ms).sum();
        let selected: Vec<(i64, i64)> = panel
            .cards
            .iter()
            .filter(|c| c.selected)
            .map(|c| (c.run.start_ts, c.run.end_ts))
            .collect();
        let suggested: Vec<(i64, i64, i64)> = panel
            .cards
            .iter()
            .filter_map(|c| c.pick.map(|t| (c.run.start_ts, c.run.end_ts, t)))
            .collect();

        theme::page().show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new("Organize")
                        .text_style(egui::TextStyle::Heading)
                        .color(theme::palette::TEXT),
                );
                ui.label(theme::num(fmt_dur(total_ms)));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("\u{d7}").clicked() {
                        close = true;
                    }
                });
            });
            ui.weak(format!(
                "{} \u{b7} contiguous unassigned stretches",
                self_day(&panel.cards)
            ));
            ui.add_space(theme::SPACE_SM);

            // Bulk row: picked cards in one go, or the selection to one task.
            ui.horizontal_wrapped(|ui| {
                if !suggested.is_empty()
                    && theme::primary_button(ui, &format!("assign picked ({})", suggested.len()))
                        .clicked()
                {
                    // One action per target so each gets its own correction.
                    let mut by_task: Vec<(i64, Vec<(i64, i64)>)> = Vec::new();
                    for (s, e, t) in &suggested {
                        match by_task.iter_mut().find(|(id, _)| id == t) {
                            Some((_, runs)) => runs.push((*s, *e)),
                            None => by_task.push((*t, vec![(*s, *e)])),
                        }
                    }
                    for (to_task, runs) in by_task {
                        pending.push(Action::AssignRuns { runs, to_task });
                    }
                }
                if !selected.is_empty() {
                    ui.menu_button(format!("selected ({}) to\u{2026}", selected.len()), |ui| {
                        for (task_id, label) in &candidates {
                            if ui.button(label).clicked() {
                                pending.push(Action::AssignRuns {
                                    runs: selected.clone(),
                                    to_task: *task_id,
                                });
                                ui.close();
                            }
                        }
                    });
                    let w = ui.available_width();
                    ui.add(
                        egui::TextEdit::singleline(&mut panel.new_label)
                            .desired_width((w - 90.0).max(120.0))
                            .hint_text("new task for selected"),
                    );
                    let label = panel.new_label.trim().to_owned();
                    if theme::primary_button_enabled(ui, !label.is_empty(), "create").clicked() {
                        pending.push(Action::AssignRunsNew {
                            runs: selected.clone(),
                            label,
                        });
                        panel.new_label.clear();
                    }
                }
            });
            match &panel.status {
                Some(Ok(msg)) => {
                    ui.weak(msg.as_str());
                }
                Some(Err(e)) => {
                    ui.colored_label(theme::palette::RED, e);
                }
                None => {}
            }
            ui.add_space(theme::SPACE_SM);

            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    let content_w = theme::content_width(ui);
                    if panel.cards.is_empty() {
                        ui.weak("nothing unassigned on this day");
                    }
                    for card in &mut panel.cards {
                        theme::card().show(ui, |ui| {
                            ui.set_width(content_w - 2.0 * theme::CARD_MARGIN as f32);
                            ui.horizontal(|ui| {
                                ui.checkbox(&mut card.selected, "");
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{}\u{2013}{}",
                                        card.start.strftime("%H:%M"),
                                        card.end.strftime("%H:%M")
                                    ))
                                    .color(theme::palette::TEXT),
                                );
                                ui.label(theme::num(fmt_dur(card.run.ms)));
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if let Some(t) = card.pick
                                            && theme::primary_button(ui, "assign").clicked()
                                        {
                                            pending.push(Action::AssignRuns {
                                                runs: vec![(card.run.start_ts, card.run.end_ts)],
                                                to_task: t,
                                            });
                                        }
                                    },
                                );
                            });
                            // Picker on its own line: task labels run long and
                            // must truncate, never push the time off the row.
                            ui.horizontal(|ui| {
                                ui.add_space(20.0);
                                let title = match card.pick {
                                    Some(t) => label_of(t),
                                    None => "pick task\u{2026}".to_owned(),
                                };
                                ui.scope(|ui| {
                                    ui.set_max_width(ui.available_width() - 80.0);
                                    ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
                                    ui.menu_button(title, |ui| {
                                        for (task_id, label) in &candidates {
                                            if ui.button(label).clicked() {
                                                card.pick = Some(*task_id);
                                                ui.close();
                                            }
                                        }
                                    });
                                });
                                if card.pick.is_some() && card.pick == card.suggested {
                                    ui.label(
                                        egui::RichText::new("suggested")
                                            .text_style(theme::caption())
                                            .color(theme::palette::GREEN),
                                    );
                                }
                            });
                            for (app, title, ms) in card.run.lines.iter().take(MAX_LINES) {
                                ui.horizontal(|ui| {
                                    ui.add_space(20.0);
                                    let text = if title.is_empty() {
                                        format!("{app} \u{b7} {}", fmt_dur(*ms))
                                    } else {
                                        format!("{title} \u{b7} {app} \u{b7} {}", fmt_dur(*ms))
                                    };
                                    ui.add(
                                        egui::Label::new(
                                            egui::RichText::new(text)
                                                .text_style(theme::caption())
                                                .weak(),
                                        )
                                        .truncate(),
                                    );
                                });
                            }
                            let more = card.run.lines.len().saturating_sub(MAX_LINES);
                            if more > 0 {
                                ui.horizontal(|ui| {
                                    ui.add_space(20.0);
                                    ui.weak(format!("+{more} more"));
                                });
                            }
                        });
                        ui.add_space(theme::SPACE_XS);
                    }
                });
        });
        if close {
            self.triage = None;
            self.loaded_at = None;
        }
        for action in pending {
            self.apply_action(action);
        }
    }
}

/// "Tue 2 Sep" from the first card, or blank.
fn self_day(cards: &[RunCard]) -> String {
    cards
        .first()
        .map(|c| c.start.strftime("%a %-d %b").to_string())
        .unwrap_or_default()
}
