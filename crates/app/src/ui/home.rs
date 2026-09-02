//! Home view: input dashboard — the standup draft, declare a task, manage
//! open and recently closed tasks, unassigned activity (and the raw-spans
//! debug list).

use eframe::egui;

use super::timeline::{matches_filter, merge_item, merge_picker};
use super::{Action, FeedBlock, OpenRow, SpanRow, StandupRow, TimelineApp, fmt_dur, theme};

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

    /// Standup card: yesterday's draft as one block per task (label line,
    /// prose, the next step lifted out), a spinner while drafting, or a lone
    /// draft button. Returns true when (re)drafting was clicked.
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
        let title = match &self.standup {
            Some(s) => format!("Standup \u{b7} {}", standup_day(&s.day)),
            None => "Standup".to_owned(),
        };
        let model_missing = self.model_missing;
        let drafting = self.standup_job.is_some();
        let mut open = self.standup_open;
        theme::hover_card(ui, "standup_card", |ui| {
            // Redraft lives on the header line so the body's height never
            // depends on it (the old bottom-row button made the text jump).
            theme::card_header(ui, &title, Some(&mut open), |ui| {
                if drafting {
                    ui.add(egui::Spinner::new().size(12.0));
                } else if !model_missing && theme::ghost_button(ui, "redraft").clicked() {
                    generate = true;
                }
            });
            theme::fade_body(ui, "standup_body", open, |ui| {
                ui.add_space(theme::SPACE_XS);
                if drafting {
                    ui.weak("drafting from yesterday's journals\u{2026}");
                } else if let Some(standup) = &self.standup {
                    standup_body_ui(ui, standup, &self.open_tasks, &self.closed_tasks);
                }
                self.standup_error_ui(ui);
            });
        });
        self.standup_open = open;
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
        let feed_vis: Vec<usize> = (0..self.feed.len())
            .filter(|&f| {
                let b = &self.feed[f];
                b.claim
                    .as_ref()
                    .is_some_and(|c| matches_filter(&q, &c.label, c.project.as_deref()))
                    || b.lines
                        .iter()
                        .any(|(app, title, _)| matches_filter(&q, title, Some(app)))
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
        let mut open_triage = false;
        theme::page().show(ui, |ui| {
            if let Some(warning) = &self.warning {
                ui.colored_label(ui.visuals().warn_fg_color, warning);
            }
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
                return;
            }
            // The whole page scrolls (cards included): a long standup draft
            // grows its card instead of pushing the task list off-window.
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .show(ui, |ui| {
                    // Pinned once; every row below sizes from this (see
                    // theme::content_width).
                    let content_w = theme::content_width(ui);
                    self.model_card_ui(ui);
                    self.service_card_ui(ui);
                    self.resume_card_ui(ui);
                    if self.standup_card_ui(ui) {
                        pending = Some(Action::GenerateStandup);
                    }
                    let open_tasks = &self.open_tasks;
                    let closed_tasks = &self.closed_tasks;
                    let feed = &self.feed;
                    let feed_seen = &self.feed_seen;
                    let feed_ejected = &self.feed_ejected;
                    let tz = &self.tz;
                    let unassigned_ms = self.unassigned_ms;
                    let show_closed = &mut self.show_closed;
                    let show_spans = &mut self.show_spans;
                    let spans_debug = self.spans_debug;
                    let spans = &self.spans;
                    let new_label = &mut self.new_label;
                    let new_project = &mut self.new_project;
                    let merge_pick = &mut self.merge_pick;
                    let suggestion = &self.suggestion;
                    let model_missing = self.model_missing;

                    theme::section_header_with(ui, "Working on", None, |ui| {
                        if model_missing {
                            return;
                        }
                        match suggestion {
                            None => {
                                if theme::ghost_button(ui, "suggest")
                                    .on_hover_text("suggest a task from the last 15 minutes")
                                    .clicked()
                                {
                                    pending = Some(Action::SuggestTask);
                                }
                            }
                            Some(super::SuggestionState::Pending(_)) => {
                                ui.add(egui::Spinner::new().size(12.0));
                            }
                            _ => {}
                        }
                    });
                    ui.add_space(theme::SPACE_SM);
                    // Declare row: label input fills, project fixed, add pinned.
                    ui.horizontal(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::primary_button(ui, "add").clicked() {
                                pending = Some(Action::Declare);
                            }
                            ui.add(
                                egui::TextEdit::singleline(new_project)
                                    .desired_width(PROJECT_COL)
                                    .hint_text("project"),
                            );
                            ui.with_layout(
                                egui::Layout::left_to_right(egui::Align::Center),
                                |ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(new_label)
                                            .desired_width(ui.available_width())
                                            .hint_text("declare a task\u{2026}"),
                                    );
                                },
                            );
                        });
                    });
                    for &o in &open_vis {
                        let t = &open_tasks[o];
                        let color = theme::series_color_for(t.task_id);
                        task_row(t, color).emphasis().show(ui, content_w, |ui| {
                            ui.menu_button("\u{2026}", |ui| {
                                if ui.button("close").clicked() {
                                    pending = Some(Action::Close(t.task_id));
                                    ui.close();
                                }
                                merge_item(ui, t.task_id, merge_pick);
                            });
                        });
                        if *merge_pick == Some(t.task_id)
                            && !merge_picker(
                                ui,
                                content_w,
                                color,
                                t.task_id,
                                &candidates,
                                &mut pending,
                            )
                        {
                            *merge_pick = None;
                        }
                    }
                    // AI declare-suggestion outcome: a dismissible chip whose
                    // "use" pre-fills the declare inputs, or the failure.
                    match suggestion {
                        Some(super::SuggestionState::Failed(msg)) => {
                            ui.horizontal(|ui| {
                                ui.weak(msg.as_str());
                                if ui.small_button("\u{d7}").clicked() {
                                    pending = Some(Action::DismissSuggestion);
                                }
                            });
                        }
                        Some(super::SuggestionState::Ready(s)) => {
                            ui.add_space(theme::SPACE_XS);
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
                        _ => {}
                    }

                    if !closed_vis.is_empty() {
                        ui.add_space(theme::SECTION_GAP);
                        theme::card_header(ui, "Recently closed", Some(&mut *show_closed), |ui| {
                            theme::badge(
                                ui,
                                &closed_vis.len().to_string(),
                                theme::palette::TEXT_DIM,
                            );
                        });
                        theme::fade_body(ui, "recently_closed_body", *show_closed, |ui| {
                            for &c in &closed_vis {
                                let t = &closed_tasks[c];
                                let color = theme::series_color_for(t.task_id);
                                task_row(t, color).show(ui, content_w, |ui| {
                                    if theme::ghost_button(ui, "reopen").clicked() {
                                        pending = Some(Action::Reopen(t.task_id));
                                    }
                                });
                            }
                        });
                    }

                    // The feed: the day's blocks newest first, each with who
                    // placed it and why; the header carries the day's whole
                    // unassigned total.
                    if !feed_vis.is_empty() || unassigned_ms > 0 {
                        ui.add_space(theme::SECTION_GAP);
                        theme::section_header_with(ui, "Feed", None, |ui| {
                            if theme::ghost_button(ui, "organize")
                                .on_hover_text("assign the day's unassigned time to tasks")
                                .clicked()
                            {
                                open_triage = true;
                            }
                            ui.label(theme::num(fmt_dur(unassigned_ms)))
                                .on_hover_text("unassigned today");
                        });
                        ui.add_space(theme::SPACE_XS);
                        for &f in &feed_vis {
                            let block = &feed[f];
                            let age = feed_seen
                                .get(&super::feed_key(block))
                                .map_or(1.0, |t| t.elapsed().as_secs_f32() / super::FEED_FADE_SECS);
                            if age < 1.0 {
                                ui.ctx().request_repaint();
                            }
                            ui.scope(|ui| {
                                ui.multiply_opacity(age.clamp(0.0, 1.0));
                                feed_row(
                                    ui,
                                    content_w,
                                    tz,
                                    block,
                                    feed_ejected.get(&block.start_ts).map(String::as_str),
                                    &candidates,
                                    new_label,
                                    &mut pending,
                                );
                            });
                        }
                    }

                    // Debug-grade raw spans; hidden unless enabled in settings.
                    if spans_debug {
                        ui.add_space(theme::SECTION_GAP);
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
        if open_triage {
            self.open_triage();
        }
    }
}

/// Project input width on the declare row.
const PROJECT_COL: f32 = 110.0;

/// Task row: identity dot, label, project chip in the task's color, the
/// "declared" chip.
fn task_row<'a>(task: &'a OpenRow, color: egui::Color32) -> theme::ListRow<'a> {
    let mut row = theme::ListRow::new(&task.label).dot(color);
    if let Some(project) = &task.project {
        row = row.chip(project.as_str(), color);
    }
    if task.declared {
        row = row.chip("declared", theme::palette::TEXT_DIM);
    }
    row
}

/// One feed block: a task row (identity dot, label, app chip, a
/// "provisional" chip for pre-pass rows) or a run row (top title, app chip,
/// state chip), the focus time, and a menu of what the user can do with it;
/// then a Small line with the block's span and the one-line reason it sits
/// where it does.
#[allow(clippy::too_many_arguments)]
fn feed_row(
    ui: &mut egui::Ui,
    width: f32,
    tz: &jiff::tz::TimeZone,
    block: &FeedBlock,
    ejected_from: Option<&str>,
    candidates: &[(i64, String)],
    new_label: &mut String,
    pending: &mut Option<Action>,
) {
    let top = block.lines.first();
    let app = top.map(|l| l.0.as_str()).unwrap_or("");
    let top_title = top
        .map(|l| {
            if l.1.is_empty() {
                l.0.as_str()
            } else {
                l.1.as_str()
            }
        })
        .unwrap_or("");
    let (title, reason): (&str, String) = match &block.claim {
        Some(c) => {
            let reason = match c.source.as_str() {
                "prepass" => c.reason.clone().unwrap_or_else(|| "pre-pass".to_owned()),
                "user" => match &c.reason {
                    Some(r) => format!("you kept it \u{b7} {r}"),
                    None => "you".to_owned(),
                },
                _ => format!("model, {:.0}%", c.confidence * 100.0),
            };
            (c.label.as_str(), reason)
        }
        None => {
            let reason = match ejected_from {
                Some(label) => format!("ejected from {label}"),
                None if block.derived => "derive left it".to_owned(),
                None => "no batch yet".to_owned(),
            };
            (top_title, reason)
        }
    };
    let mut row = theme::ListRow::new(title).num(fmt_dur(block.ms));
    match &block.claim {
        Some(c) => {
            row = row.dot(theme::series_color_for(c.task_id));
            if c.source == "prepass" {
                row = row.chip("provisional", theme::palette::AMBER);
            }
        }
        None => {
            if !app.is_empty() && app != title {
                row = row.chip(app, theme::palette::TEXT_DIM);
            }
            let state = if ejected_from.is_some() {
                "ejected"
            } else if block.derived {
                "unmatched"
            } else {
                "fresh"
            };
            row = row.chip(state, theme::palette::TEXT_DIM);
        }
    }
    row.show(ui, width, |ui| {
        ui.menu_button("\u{2026}", |ui| match &block.claim {
            Some(c) => {
                if c.source == "prepass" && ui.button("keep").clicked() {
                    *pending = Some(Action::KeepBlock(c.interval_id));
                    ui.close();
                }
                ui.menu_button("move to", |ui| {
                    for (task_id, label) in candidates.iter().filter(|(id, _)| *id != c.task_id) {
                        if ui.button(label).clicked() {
                            *pending = Some(Action::ReassignSession {
                                interval_ids: vec![c.interval_id],
                                to_task: *task_id,
                            });
                            ui.close();
                        }
                    }
                });
                if ui.button("eject").clicked() {
                    *pending = Some(Action::EjectBlock {
                        interval_id: c.interval_id,
                        start_ts: block.start_ts,
                        end_ts: block.end_ts,
                        label: c.label.clone(),
                    });
                    ui.close();
                }
            }
            None => {
                if ui.button("declare").clicked() {
                    *new_label = top_title.to_owned();
                    ui.close();
                }
                ui.menu_button("assign to", |ui| {
                    for (task_id, label) in candidates {
                        if ui.button(label).clicked() {
                            *pending = Some(Action::AssignRuns {
                                runs: vec![(block.start_ts, block.end_ts)],
                                to_task: *task_id,
                            });
                            ui.close();
                        }
                    }
                });
            }
        });
    });
    let fmt = |ms: i64| {
        chronicle_core::types::ms_to_ts(ms)
            .to_zoned(tz.clone())
            .strftime("%H:%M")
            .to_string()
    };
    let mut sub = format!(
        "{}\u{2013}{} \u{b7} {reason}",
        fmt(block.start_ts),
        fmt(block.end_ts)
    );
    if block.claim.is_some() && !top_title.is_empty() {
        sub.push_str(" \u{b7} ");
        if !app.is_empty() && app != top_title {
            sub.push_str(app);
            sub.push_str(": ");
        }
        sub.push_str(top_title);
    }
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        ui.add(
            egui::Label::new(
                egui::RichText::new(sub)
                    .text_style(egui::TextStyle::Small)
                    .color(theme::palette::TEXT_DIM),
            )
            .truncate(),
        );
    });
}

/// "2026-09-01" → "Mon 1 Sep"; the raw string when it doesn't parse.
fn standup_day(iso: &str) -> String {
    iso.parse::<jiff::civil::Date>()
        .map(|d| d.strftime("%a %-d %b").to_string())
        .unwrap_or_else(|_| iso.to_owned())
}

/// One paragraph of the draft, split for display.
struct StandupBlock {
    /// The task's label when the paragraph opens with one.
    label: Option<String>,
    /// Remaining lines, in order (fallback drafts are bullet lines).
    lines: Vec<String>,
    /// The lifted-out "Next steps:" / "Next:" sentence.
    next: Option<String>,
    /// Preamble rather than a task (the journal-free fallback's first line).
    meta: bool,
}

/// Split a draft into per-task blocks. Paragraphs are blank-line separated
/// (prompt rule and fallback alike); a paragraph opens with the task's label
/// (LLM drafts) or a `Task:` line (fallback); a `Next steps:` / `Next:`
/// sentence is lifted out of the prose. `labels` are the known task labels,
/// matched longest first so "foo bar" wins over "foo".
fn standup_blocks(content: &str, labels: &[&str]) -> Vec<StandupBlock> {
    let mut labels: Vec<&str> = labels.iter().copied().filter(|l| !l.is_empty()).collect();
    labels.sort_by_key(|l| std::cmp::Reverse(l.len()));
    let mut blocks = Vec::new();
    let mut para: Vec<&str> = Vec::new();
    for line in content.lines().chain(std::iter::once("")) {
        if line.trim().is_empty() {
            if !para.is_empty() {
                blocks.push(standup_block(&para, &labels));
                para.clear();
            }
        } else {
            para.push(line.trim());
        }
    }
    blocks
}

fn standup_block(para: &[&str], labels: &[&str]) -> StandupBlock {
    let mut lines: Vec<String> = para.iter().map(|l| (*l).to_owned()).collect();
    let first = lines[0].clone();
    let mut label = None;
    if let Some(rest) = first.strip_prefix("Task:") {
        label = Some(rest.trim().to_owned());
        lines.remove(0);
    } else if let Some(l) = labels
        .iter()
        .find(|l| first.is_char_boundary(l.len()) && first[..l.len()].eq_ignore_ascii_case(l))
    {
        label = Some(first[..l.len()].to_owned());
        let rest = first[l.len()..]
            .trim_start_matches([':', '-', '\u{2013}', '\u{2014}', ' '])
            .to_owned();
        if rest.is_empty() {
            lines.remove(0);
        } else {
            lines[0] = rest;
        }
    }
    let meta = label.is_none() && first.starts_with("No journal entries");
    let mut next = None;
    for i in 0..lines.len() {
        if let Some((start, end)) = next_marker(&lines[i]) {
            next = Some(lines[i][end..].trim().to_owned());
            let before = lines[i][..start].trim_end().to_owned();
            if before.is_empty() {
                lines.remove(i);
            } else {
                lines[i] = before;
            }
            break;
        }
    }
    StandupBlock {
        label,
        lines,
        next,
        meta,
    }
}

/// Byte range of the first "Next steps:" / "Next step:" / "Next:" marker.
fn next_marker(line: &str) -> Option<(usize, usize)> {
    let lower = line.to_ascii_lowercase();
    ["next steps:", "next step:", "next:"]
        .iter()
        .filter_map(|m| lower.find(m).map(|i| (i, i + m.len())))
        .min_by_key(|(i, _)| *i)
}

/// The draft as blocks: label line (Medium), prose lines, then the next
/// step indented behind a caret; 8pt between blocks.
fn standup_body_ui(ui: &mut egui::Ui, standup: &StandupRow, open: &[OpenRow], closed: &[OpenRow]) {
    let labels: Vec<&str> = open
        .iter()
        .chain(closed)
        .map(|t| t.label.as_str())
        .collect();
    for (i, block) in standup_blocks(&standup.content, &labels).iter().enumerate() {
        if i > 0 {
            ui.add_space(theme::SPACE_SM);
        }
        if block.meta {
            for line in &block.lines {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(line)
                            .text_style(egui::TextStyle::Small)
                            .color(theme::palette::TEXT_DIM),
                    )
                    .wrap(),
                );
            }
            continue;
        }
        if let Some(label) = &block.label {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(label)
                        .family(egui::FontFamily::Name(theme::MEDIUM.into()))
                        .color(theme::palette::TEXT),
                )
                .wrap(),
            );
        }
        for line in &block.lines {
            ui.add(egui::Label::new(egui::RichText::new(line).color(theme::palette::TEXT)).wrap());
        }
        if let Some(next) = &block.next {
            ui.horizontal(|ui| {
                ui.add_space(theme::SPACE_SM);
                ui.label(
                    theme::glyph(theme::icon::CARET_RIGHT)
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT_DIM),
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(format!("Next: {next}")).color(theme::palette::TEXT),
                    )
                    .wrap(),
                );
            });
        }
    }
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
