//! Home view: input dashboard — the standup draft, declare a task, manage
//! open and recently closed tasks, unassigned activity (and the raw-spans
//! debug list).

use std::borrow::Cow;

use eframe::egui;

use super::timeline::{matches_filter, merge_item, merge_picker};
use super::{
    Action, FeedBlock, OpenRow, Proposal, SpanRow, StandupRow, TimelineApp, fmt_dur, theme,
};

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
            let day = chronicle_core::types::ms_to_ts(resume.ts)
                .to_zoned(self.tz.clone())
                .date();
            self.set_day(day);
            self.loaded_at = None;
            self.view = super::View::Timeline;
        } else if dismiss {
            self.resume = None;
        }
    }

    /// Morning picker: "Today I'm on" over the open tasks plus a
    /// free-text line, shown until an intent is stored for today ("skip
    /// today" stores an empty one so it stops asking).
    fn intent_card_ui(&mut self, ui: &mut egui::Ui, pending: &mut Option<Action>) {
        if self.intent.is_some() {
            return;
        }
        let open_tasks = &self.open_tasks;
        let picks = &mut self.intent_pick;
        let text = &mut self.intent_text;
        let mut set = false;
        let mut skip = false;
        theme::hover_card(ui, "intent_card", |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    theme::glyph(theme::icon::FLAG)
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::ACCENT),
                );
                ui.label(egui::RichText::new("Today I'm on\u{2026}").strong());
            });
            for t in open_tasks {
                let mut on = picks.contains(&t.task_id);
                if ui
                    .checkbox(&mut on, super::clip_chars(&t.label, 46))
                    .changed()
                {
                    if on {
                        picks.insert(t.task_id);
                    } else {
                        picks.remove(&t.task_id);
                    }
                }
            }
            ui.add_space(theme::SPACE_XS);
            ui.add(
                egui::TextEdit::singleline(text)
                    .desired_width(ui.available_width())
                    .hint_text("and/or in your own words\u{2026}"),
            );
            ui.add_space(theme::SPACE_XS);
            ui.horizontal(|ui| {
                if theme::primary_button(ui, "set").clicked() {
                    set = true;
                }
                if theme::ghost_button(ui, "skip today").clicked() {
                    skip = true;
                }
            });
        });
        ui.add_space(theme::CARD_GAP);
        if set || skip {
            let intent = if skip {
                chronicle_core::intent::Intent::default()
            } else {
                chronicle_core::intent::Intent {
                    // Picker order, not click order.
                    task_ids: open_tasks
                        .iter()
                        .map(|t| t.task_id)
                        .filter(|id| picks.contains(id))
                        .collect(),
                    text: text.trim().to_owned(),
                }
            };
            *pending = Some(Action::SetIntent(intent));
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
        let model_missing = self.model_missing;
        let drafting = self.standup_job.is_some();
        let mut open = self.standup_open;
        // Collapsed: one line that says what is inside and that it was read.
        let title = match &self.standup {
            Some(s) if !open => {
                let n = s.task_count;
                let read = if self.standup_read_day.as_deref() == Some(s.day.as_str()) {
                    " \u{b7} read"
                } else {
                    ""
                };
                format!("Standup \u{b7} {n} tasks{read}")
            }
            Some(s) => format!("Standup \u{b7} {}", standup_day(&s.day)),
            None => "Standup".to_owned(),
        };
        let mut show_all = self.standup_show_all;
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
                    standup_body_ui(
                        ui,
                        standup,
                        &self.open_tasks,
                        &self.closed_tasks,
                        &mut show_all,
                    );
                }
                self.standup_error_ui(ui);
            });
        });
        self.standup_open = open;
        self.standup_show_all = show_all;
        // Shown expanded once = read: remembered per draft day so the next
        // launch starts it collapsed.
        if open
            && !drafting
            && let Some(day) = self.standup.as_ref().map(|s| s.day.clone())
            && self.standup_read_day.as_deref() != Some(day.as_str())
        {
            if let Some(conn) = self.conn.as_ref() {
                let _ = chronicle_core::storage::set_meta(
                    conn,
                    &format!("standup_read:{day}"),
                    Some("1"),
                );
            }
            self.standup_read_day = Some(day);
        }
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
        // Filtered index sets; empty query keeps everything without a scan.
        // Owned (not borrowed): later code in this fn calls `&mut self`
        // methods while `q` is still in scope.
        let q = self.filter_lc.clone();
        let open_vis: Vec<usize> = if q.is_empty() {
            (0..self.open_tasks.len()).collect()
        } else {
            (0..self.open_tasks.len())
                .filter(|&o| {
                    let t = &self.open_tasks[o];
                    matches_filter(&q, &t.label, t.project.as_deref())
                })
                .collect()
        };
        let closed_vis: Vec<usize> = if q.is_empty() {
            (0..self.closed_tasks.len()).collect()
        } else {
            (0..self.closed_tasks.len())
                .filter(|&c| {
                    let t = &self.closed_tasks[c];
                    matches_filter(&q, &t.label, t.project.as_deref())
                })
                .collect()
        };
        let feed_vis: Vec<usize> = if q.is_empty() {
            (0..self.feed.len()).collect()
        } else {
            (0..self.feed.len())
                .filter(|&f| {
                    let b = &self.feed[f];
                    b.claim
                        .as_ref()
                        .is_some_and(|c| matches_filter(&q, &c.label, c.project.as_deref()))
                        || b.lines
                            .iter()
                            .any(|(app, title, _)| matches_filter(&q, title, Some(app)))
                })
                .collect()
        };
        let span_vis: Vec<usize> = if q.is_empty() {
            (0..self.spans.len()).collect()
        } else {
            (0..self.spans.len())
                .filter(|&s| {
                    let sp = &self.spans[s];
                    matches_filter(&q, &sp.title, Some(&sp.app))
                })
                .collect()
        };
        let candidates = self.merge_candidates();

        let mut pending: Option<Action> = None;
        let mut open_triage = false;
        // Wide window: the feed gets its own column beside the cards and
        // task list (a side panel, added before the central page).
        let wide = theme::wide(ui.ctx());
        if wide && self.error.is_none() {
            let frame = theme::page_frame();
            egui::Panel::right("home_feed")
                .frame(frame)
                .resizable(true)
                .default_size((ui.available_width() * 0.5).clamp(320.0, 520.0))
                .size_range(300.0..=640.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("home_feed_scroll")
                        .auto_shrink(false)
                        .show(ui, |ui| {
                            let content_w = theme::content_width(ui);
                            feed_section_ui(
                                ui,
                                content_w,
                                FeedSection {
                                    tz: &self.tz,
                                    feed: &self.feed,
                                    feed_vis: &feed_vis,
                                    proposals: &self.proposals,
                                    feed_seen: &self.feed_seen,
                                    feed_ejected: &self.feed_ejected,
                                    unassigned_ms: self.unassigned_ms,
                                    candidates: &candidates,
                                    progress: self.progress.as_ref(),
                                    tidy: self.tidy,
                                },
                                &mut self.new_label,
                                &mut pending,
                                &mut open_triage,
                            );
                        });
                });
        }
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
                    self.intent_card_ui(ui, &mut pending);
                    let open_tasks = &self.open_tasks;
                    let closed_tasks = &self.closed_tasks;
                    let feed = &self.feed;
                    let proposals = &self.proposals;
                    let feed_seen = &self.feed_seen;
                    let feed_ejected = &self.feed_ejected;
                    let tz = &self.tz;
                    let unassigned_ms = self.unassigned_ms;
                    let show_closed = &mut self.show_closed;
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
                    if open_tasks.is_empty() && q.is_empty() {
                        ui.add_space(theme::SPACE_XS);
                        ui.weak("no open tasks yet \u{2014} declare one above, or accept what the feed proposes");
                    }
                    for &o in &open_vis {
                        let t = &open_tasks[o];
                        let color = theme::task_color(t.task_id, t.project.as_deref());
                        task_row(t, color, true)
                            .emphasis()
                            .padded()
                            .subtitle(working_subtitle(t))
                            .show(ui, content_w, |ui| {
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
                                let color = theme::task_color(t.task_id, t.project.as_deref());
                                task_row(t, color, false).show(ui, content_w, |ui| {
                                    if theme::ghost_button(ui, "reopen").clicked() {
                                        pending = Some(Action::Reopen(t.task_id));
                                    }
                                });
                            }
                        });
                    }

                    if !wide {
                        feed_section_ui(
                            ui,
                            content_w,
                            FeedSection {
                                tz,
                                feed,
                                feed_vis: &feed_vis,
                                proposals,
                                feed_seen,
                                feed_ejected,
                                unassigned_ms,
                                candidates: &candidates,
                                progress: self.progress.as_ref(),
                                tidy: self.tidy,
                            },
                            new_label,
                            &mut pending,
                            &mut open_triage,
                        );
                    }

                    // Standup last (m26): the task list is the page; the
                    // draft is one click away and collapses once read.
                    ui.add_space(theme::SECTION_GAP);
                    if self.standup_card_ui(ui) {
                        pending = Some(Action::GenerateStandup);
                    }

                    // Debug-grade raw spans; hidden unless enabled in settings.
                    let show_spans = &mut self.show_spans;
                    let spans = &self.spans;
                    if self.spans_debug {
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

/// The feed's `?` hover: how a block gets placed, and when.
fn feed_help_ui(ui: &mut egui::Ui) {
    ui.set_max_width(300.0);
    ui.label("Each block of work is placed by the first rule that fits:");
    for line in [
        "1. the checked-out branch carries a ticket key \u{2192} that task",
        "2. the block's repo matches an open task's project",
        "3. its window titles match one of your past corrections",
        "4. otherwise it waits; blocks sharing a rare title or a repo become a proposal after 10 min",
    ] {
        ui.weak(line);
    }
    ui.add_space(theme::SPACE_XS);
    ui.weak("Rules run every 60 s and place provisionally (\"to confirm\"). The model re-reads each batch about 35 min later and its placements replace the provisional ones. Rows you keep or move are never touched.");
}

/// Task row: identity mark (a left-edge bar for Working on, a dot
/// elsewhere), label, project chip in the task's color, the anchor chip
/// (ticket key or branch), the "declared" chip.
fn task_row<'a>(task: &'a OpenRow, color: egui::Color32, bar: bool) -> theme::ListRow<'a> {
    let mut row = theme::ListRow::new(&task.label);
    row = if bar { row.bar(color) } else { row.dot(color) };
    if let Some(project) = &task.project {
        row = row.chip(project.as_str(), color);
    }
    if bar && let Some(anchor) = &task.anchor {
        row = row.chip(anchor.as_str(), theme::palette::TEXT_DIM);
    }
    if task.declared {
        row = row.chip("declared", theme::palette::TEXT_DIM);
    }
    if task.intent {
        row = row.chip("intent", theme::palette::ACCENT);
    }
    if task.stuck {
        row = row.chip("stuck", theme::palette::AMBER);
    }
    row
}

/// Working-on second line: time today · last touched · next step.
fn working_subtitle(task: &OpenRow) -> String {
    let mut parts: Vec<String> = Vec::new();
    if task.today_ms > 0 {
        parts.push(format!("{} today", fmt_dur(task.today_ms)));
    } else {
        parts.push("no time today".to_owned());
    }
    if let Some(z) = &task.last_touched {
        parts.push(format!("touched {}", z.strftime("%H:%M")));
    }
    if let Some(next) = &task.next_step {
        parts.push(next.clone());
    }
    parts.join(" \u{b7} ")
}

/// A proposed task: the cluster's suggested label (a spinner while the
/// naming job runs, the top title when there is no name), project chip,
/// span + focus time, its app/title lines, and accept / not a task.
fn proposal_card(
    ui: &mut egui::Ui,
    tz: &jiff::tz::TimeZone,
    p: &Proposal,
    pending: &mut Option<Action>,
) {
    let fmt = |ms: i64| {
        chronicle_core::types::ms_to_ts(ms)
            .to_zoned(tz.clone())
            .strftime("%H:%M")
            .to_string()
    };
    let fallback = p
        .lines
        .first()
        .map(|l| {
            if l.1.is_empty() {
                l.0.clone()
            } else {
                l.1.clone()
            }
        })
        .unwrap_or_else(|| "unnamed".to_owned());
    let label = p.label.clone().unwrap_or_else(|| fallback.clone());
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
                ui.label(
                    egui::RichText::new("proposed task")
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::ACCENT),
                );
                if p.naming && p.label.is_none() {
                    ui.add(egui::Spinner::new().size(10.0));
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(theme::num(fmt_dur(p.ms)));
                    ui.label(
                        egui::RichText::new(format!(
                            "{}\u{2013}{} \u{b7} {} stretch{}",
                            fmt(p.start_ts),
                            fmt(p.end_ts),
                            p.runs.len(),
                            if p.runs.len() == 1 { "" } else { "es" }
                        ))
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT_DIM),
                    );
                });
            });
            ui.horizontal(|ui| {
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&label)
                            .family(egui::FontFamily::Name(theme::MEDIUM.into()))
                            .color(theme::palette::TEXT),
                    )
                    .truncate(),
                );
                if let Some(project) = &p.project {
                    theme::badge(ui, project, theme::palette::ACCENT);
                }
            });
            for (app, title, ms) in p.lines.iter().take(2) {
                let title = theme::display_title(title);
                let line = if title.is_empty() {
                    format!("{} \u{b7} {app}", fmt_dur(*ms))
                } else {
                    format!("{} \u{b7} {app}: {title}", fmt_dur(*ms))
                };
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(line)
                            .text_style(egui::TextStyle::Small)
                            .color(theme::palette::TEXT_DIM),
                    )
                    .truncate(),
                );
            }
            ui.add_space(theme::SPACE_XS);
            ui.horizontal(|ui| {
                if theme::primary_button(ui, "accept")
                    .on_hover_text("declare this task and assign the stretches to it")
                    .clicked()
                {
                    *pending = Some(Action::AcceptProposal {
                        id: p.id,
                        label: label.clone(),
                    });
                }
                if theme::ghost_button(ui, "not a task").clicked() {
                    *pending = Some(Action::DismissProposal(p.id));
                }
            });
        });
    ui.add_space(theme::SPACE_XS);
}

/// One feed block on the row grammar: block start in the time column, a
/// task dot (or a hollow ring while nothing claims it), the task label (or
/// the humanised top window title), the focus time, and a menu of what the
/// user can do with it; under the title the meta line: a tinted state word
/// (`to confirm` / `kept` / `live` / `unsorted` / `new` / `moved out`) and the
/// one-line reason it sits where it does, the span and pipeline detail on
/// hover.
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
    let top_title = theme::display_title(top_title);
    let fmt = |ms: i64| {
        chronicle_core::types::ms_to_ts(ms)
            .to_zoned(tz.clone())
            .strftime("%H:%M")
            .to_string()
    };
    // Human words on the row: a tinted state word leads the meta line, the
    // reason follows; the pipeline's own vocabulary (source, confidence,
    // rule) goes in `detail`, the hover on the meta line.
    type State = Option<(&'static str, egui::Color32)>;
    let (title, state, reason, detail): (Cow<str>, State, String, String) = match &block.claim {
        Some(c) => {
            let (state, reason, detail) = match c.source.as_str() {
                "prepass" => (
                    Some(("to confirm", theme::palette::AMBER)),
                    match &c.reason {
                        Some(r) => format!("placed by a rule \u{b7} {r}"),
                        None => "placed by a rule".to_owned(),
                    },
                    format!(
                        "pre-pass rule{}; not confirmed by the model yet",
                        c.reason
                            .as_deref()
                            .map(|r| format!(" ({r})"))
                            .unwrap_or_default()
                    ),
                ),
                "user" => (
                    Some(("kept", theme::palette::GREEN)),
                    match &c.reason {
                        Some(r) => format!("by you \u{b7} {r}"),
                        None => "by you".to_owned(),
                    },
                    "source: user".to_owned(),
                ),
                "live" => (
                    Some(("live", theme::palette::AMBER)),
                    "placed by the model".to_owned(),
                    format!(
                        "live pass, not confirmed by the batch yet \u{b7} confidence {:.0}%",
                        c.confidence * 100.0
                    ),
                ),
                _ => (
                    None,
                    "placed by the model".to_owned(),
                    format!(
                        "source: model \u{b7} confidence {:.0}%",
                        c.confidence * 100.0
                    ),
                ),
            };
            (Cow::Borrowed(c.label.as_str()), state, reason, detail)
        }
        None => {
            let (state, reason, detail) = match ejected_from {
                Some(label) => (
                    "moved out",
                    format!("of {label}"),
                    "ejected by you; the model will not re-place it".to_owned(),
                ),
                None if block.derived => (
                    "unsorted",
                    "not placed by the model".to_owned(),
                    "the model's pass over this stretch left it unassigned".to_owned(),
                ),
                None => (
                    "new",
                    "waiting for the model".to_owned(),
                    "no derive batch has covered this stretch yet".to_owned(),
                ),
            };
            (
                theme::humanize_title(top_title, app),
                Some((state, theme::palette::TEXT_DIM)),
                reason,
                detail,
            )
        }
    };
    let mut meta = reason;
    if !app.is_empty() && !title.contains(app) {
        meta.push_str(" \u{b7} ");
        meta.push_str(app);
        if block.claim.is_some() && !top_title.is_empty() && top_title != app {
            meta.push_str(": ");
            meta.push_str(top_title);
        }
    }
    let mut row = theme::ListRow::new(&title)
        .time(fmt(block.start_ts))
        .padded()
        .num(fmt_dur(block.ms))
        .meta(state, meta)
        .hover(format!(
            "{}\u{2013}{}\n{detail}",
            fmt(block.start_ts),
            fmt(block.end_ts)
        ));
    match &block.claim {
        Some(c) => {
            // A shaky model placement wears the timeline's confidence tint
            // instead of hiding it in the hover text.
            let dot = if c.source != "user" && c.confidence < 0.7 {
                theme::confidence_color(theme::confidence_band(c.confidence))
                    .unwrap_or_else(|| theme::task_color(c.task_id, c.project.as_deref()))
            } else {
                theme::task_color(c.task_id, c.project.as_deref())
            };
            row = row.dot(dot);
        }
        // Raw window titles stay on one line; the hover has the whole thing.
        None => row = row.ring().lines(1),
    }
    row.show(ui, width, |ui| {
        ui.menu_button("\u{2026}", |ui| match &block.claim {
            Some(c) => {
                if (c.source == "prepass" || c.source == "live") && ui.button("keep").clicked() {
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
}

/// Arguments of the feed section, which renders in place on the narrow
/// widget and in its own right-hand column when the window is wide.
struct FeedSection<'a> {
    tz: &'a jiff::tz::TimeZone,
    feed: &'a [FeedBlock],
    feed_vis: &'a [usize],
    proposals: &'a [Proposal],
    feed_seen: &'a std::collections::HashMap<super::FeedKey, std::time::Instant>,
    feed_ejected: &'a std::collections::HashMap<i64, String>,
    unassigned_ms: i64,
    candidates: &'a [(i64, String)],
    /// The resident worker's in-flight derive (m27): the "deriving…" row.
    progress: Option<&'a chronicle_core::storage::DeriveProgress>,
    /// Today's consolidation stamp (see `HomeData::tidy`); None off today.
    tidy: Option<Option<i64>>,
}

/// A progress row older than this is a stale meta value from a dead worker.
const PROGRESS_STALE_MS: i64 = 10 * 60_000;

fn feed_section_ui(
    ui: &mut egui::Ui,
    content_w: f32,
    f: FeedSection<'_>,
    new_label: &mut String,
    pending: &mut Option<Action>,
    open_triage: &mut bool,
) {
    let FeedSection {
        tz,
        feed,
        feed_vis,
        proposals,
        feed_seen,
        feed_ejected,
        unassigned_ms,
        candidates,
        progress,
        tidy,
    } = f;
    let progress = progress
        .filter(|p| jiff::Timestamp::now().as_millisecond() - p.started_ts < PROGRESS_STALE_MS);
    // The feed: the day's blocks newest first, each with who
    // placed it and why; the header carries the day's whole
    // unassigned total.
    if !feed_vis.is_empty() || !proposals.is_empty() || unassigned_ms > 0 || progress.is_some() {
        ui.add_space(theme::SECTION_GAP);
        theme::section_header_with(ui, "Feed", None, |ui| {
            if theme::ghost_button(ui, "organize")
                .on_hover_text("assign the day's unassigned time to tasks")
                .clicked()
            {
                *open_triage = true;
            }
            match tidy {
                Some(Some(id)) if id > 0 => {
                    if theme::ghost_button(ui, "undo tidy")
                        .on_hover_text("put back the tasks today's tidy merged or renamed")
                        .clicked()
                    {
                        *pending = Some(Action::UndoTidy(id));
                    }
                }
                Some(_) if theme::ghost_button(ui, "tidy")
                    .on_hover_text(
                        "merge today's duplicate model tasks and fold stray minutes into the work around them",
                    )
                    .clicked() =>
                {
                    *pending = Some(Action::TidyToday);
                }
                _ => {}
            }
            ui.label(theme::num(fmt_dur(unassigned_ms)))
                .on_hover_text("unassigned today");
            ui.label(egui::RichText::new("?").color(theme::palette::TEXT_DIM))
                .on_hover_ui(feed_help_ui);
        });
        ui.add_space(theme::SPACE_XS);
        for p in proposals {
            proposal_card(ui, tz, p, pending);
        }
        if let Some(p) = progress {
            progress_row(ui, content_w, tz, p);
        }
        for &f in feed_vis {
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
                    candidates,
                    new_label,
                    pending,
                );
            });
        }
    }
}

/// The "deriving 09:18–09:51 · <label>…" row above the newest block while
/// the resident worker types (m27 chunk 4); the 5 s reload carries updates.
fn progress_row(
    ui: &mut egui::Ui,
    width: f32,
    tz: &jiff::tz::TimeZone,
    p: &chronicle_core::storage::DeriveProgress,
) {
    let hm = |ms: i64| {
        chronicle_core::types::ms_to_ts(ms)
            .to_zoned(tz.clone())
            .strftime("%H:%M")
            .to_string()
    };
    let title = if p.kind == "day" {
        "tidying today".to_owned()
    } else {
        format!("deriving {}\u{2013}{}", hm(p.start_ts), hm(p.end_ts))
    };
    let sub = if p.label.is_empty() {
        "the model is reading the window".to_owned()
    } else {
        p.label.clone()
    };
    let chip = match p.kind.as_str() {
        "live" => "live",
        "day" => "day",
        _ => "batch",
    };
    theme::ListRow::new(&title)
        .padded()
        .meta(Some((chip, theme::palette::AMBER)), sub)
        .show(ui, width, |ui| {
            ui.add(egui::Spinner::new().size(12.0));
        });
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_secs(1));
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
/// Count of task blocks (non-meta) in a draft; the collapsed card title's
/// count, cached on `StandupRow` when the draft loads instead of reparsed
/// every frame.
pub(super) fn standup_task_count(content: &str, labels: &[&str]) -> usize {
    standup_blocks(content, labels)
        .iter()
        .filter(|b| !b.meta)
        .count()
}

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
/// step indented behind a caret; 8pt between blocks. Only the first task
/// block shows until `show_all` (m25: a seven-task draft buried the feed
/// below the fold); the next-step line is cut to whole sentences.
fn standup_body_ui(
    ui: &mut egui::Ui,
    standup: &StandupRow,
    open: &[OpenRow],
    closed: &[OpenRow],
    show_all: &mut bool,
) {
    let labels: Vec<&str> = open
        .iter()
        .chain(closed)
        .map(|t| t.label.as_str())
        .collect();
    let blocks = standup_blocks(&standup.content, &labels);
    let task_blocks = blocks.iter().filter(|b| !b.meta).count();
    let mut shown_tasks = 0;
    for (i, block) in blocks.iter().enumerate() {
        if !block.meta {
            shown_tasks += 1;
            if shown_tasks > 1 && !*show_all {
                continue;
            }
        }
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
            let (cut, capped) = theme::cap_sentences(next, NEXT_MAX_CHARS);
            ui.horizontal(|ui| {
                ui.add_space(theme::SPACE_SM);
                ui.label(
                    theme::glyph(theme::icon::CARET_RIGHT)
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT_DIM),
                );
                let text = if capped {
                    format!("Next: {cut}\u{2026}")
                } else {
                    format!("Next: {cut}")
                };
                let resp = ui.add(
                    egui::Label::new(egui::RichText::new(text).color(theme::palette::TEXT)).wrap(),
                );
                if capped {
                    resp.on_hover_text(next.clone());
                }
            });
        }
    }
    if task_blocks > 1 {
        ui.add_space(theme::SPACE_XS);
        let label = if *show_all {
            "show less".to_owned()
        } else {
            format!("{} more", task_blocks - 1)
        };
        if theme::ghost_button(ui, label).clicked() {
            *show_all = !*show_all;
        }
    }
}

/// Longest next-step line on the standup card before it is cut to whole
/// sentences (the full text stays on hover).
const NEXT_MAX_CHARS: usize = 180;

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
