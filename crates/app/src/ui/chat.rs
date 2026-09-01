//! Chat tab: owns the warm `chat-worker` child and renders the transcript.
//! Conversations live in the DB; the UI creates/lists them and tells the
//! worker which one to use.

use std::io::{BufRead, Write};
use std::process::Child;
use std::sync::mpsc;

use eframe::egui;
use jiff::Timestamp;
use rusqlite::Connection;

use super::{TimelineApp, theme};

enum ChatEvent {
    Ready,
    Tok(String),
    Done,
    Err(String),
    /// Worker stdout closed (crash or exit).
    Exited,
}

struct ChatMsg {
    user: bool,
    text: String,
}

/// Chat state. Owns the warm `chat-worker` child; dropping the panel kills it
/// (README: model resident only while the chat tab is open).
pub(super) struct ChatPanel {
    child: Child,
    rx: mpsc::Receiver<ChatEvent>,
    conversation_id: i64,
    /// Retrieval scoped to this task's workspace (m16): (task_id, label).
    task_scope: Option<(i64, String)>,
    transcript: Vec<ChatMsg>,
    input: String,
    /// Model still loading; no questions accepted yet.
    warming: bool,
    /// Question in flight (answer streaming in).
    busy: bool,
    error: Option<String>,
}

impl ChatPanel {
    fn spawn(
        ctx: &egui::Context,
        conn: Option<&Connection>,
        conversation_id: i64,
        task_scope: Option<(i64, String)>,
    ) -> anyhow::Result<Self> {
        let mut child =
            crate::spawn_chat_worker(conversation_id, task_scope.as_ref().map(|(id, _)| *id))?;
        let stdout = child.stdout.take().expect("chat worker stdout is piped");
        let (tx, rx) = mpsc::channel();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("chat-reader".into())
            .spawn(move || {
                use crate::chatproto::WorkerMsg;
                for line in std::io::BufReader::new(stdout).lines() {
                    let Ok(line) = line else { break };
                    let event = match serde_json::from_str::<WorkerMsg>(&line) {
                        Ok(WorkerMsg::Ready) => ChatEvent::Ready,
                        Ok(WorkerMsg::Tok { text }) => ChatEvent::Tok(text),
                        Ok(WorkerMsg::Done) => ChatEvent::Done,
                        Ok(WorkerMsg::Err { message }) => ChatEvent::Err(message),
                        Err(_) => continue,
                    };
                    let dead = tx.send(event).is_err();
                    ctx.request_repaint();
                    if dead {
                        return;
                    }
                }
                let _ = tx.send(ChatEvent::Exited);
                ctx.request_repaint();
            })?;
        let transcript = load_transcript(conn, conversation_id);
        Ok(Self {
            child,
            rx,
            conversation_id,
            task_scope,
            transcript,
            input: String::new(),
            warming: true,
            busy: false,
            error: None,
        })
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                ChatEvent::Ready => self.warming = false,
                ChatEvent::Tok(text) => match self.transcript.last_mut() {
                    Some(m) if !m.user => m.text.push_str(&text),
                    _ => self.transcript.push(ChatMsg { user: false, text }),
                },
                ChatEvent::Done => self.busy = false,
                ChatEvent::Err(message) => {
                    self.busy = false;
                    self.warming = false;
                    self.error = Some(message);
                }
                ChatEvent::Exited => {
                    self.busy = false;
                    self.warming = false;
                    self.error
                        .get_or_insert_with(|| "chat worker exited".into());
                }
            }
        }
    }

    fn send_line(&mut self, msg: &crate::chatproto::ClientMsg) -> bool {
        let line = serde_json::to_string(msg).expect("client msg serializes");
        let Some(stdin) = self.child.stdin.as_mut() else {
            self.error = Some("chat worker not reachable".into());
            return false;
        };
        if stdin
            .write_all(format!("{line}\n").as_bytes())
            .and_then(|()| stdin.flush())
            .is_err()
        {
            self.error = Some("chat worker not reachable".into());
            return false;
        }
        true
    }

    fn send_question(&mut self) {
        let ask = self.input.trim().to_owned();
        if ask.is_empty() || self.busy || self.warming {
            return;
        }
        if !self.send_line(&crate::chatproto::ClientMsg::Ask { ask: ask.clone() }) {
            return;
        }
        self.transcript.push(ChatMsg {
            user: true,
            text: ask,
        });
        self.transcript.push(ChatMsg {
            user: false,
            text: String::new(),
        });
        self.input.clear();
        self.busy = true;
        self.error = None;
    }

    /// Point the panel (and worker) at another conversation, with its task
    /// scope (None = general retrieval). No-op while an answer is streaming.
    fn switch_conversation(
        &mut self,
        conn: Option<&Connection>,
        conversation_id: i64,
        task_scope: Option<(i64, String)>,
    ) {
        let same_scope =
            self.task_scope.as_ref().map(|(id, _)| *id) == task_scope.as_ref().map(|(id, _)| *id);
        if self.busy || (conversation_id == self.conversation_id && same_scope) {
            return;
        }
        if !self.send_line(&crate::chatproto::ClientMsg::Switch {
            conversation_id,
            task_id: task_scope.as_ref().map(|(id, _)| *id),
        }) {
            return;
        }
        self.conversation_id = conversation_id;
        self.task_scope = task_scope;
        self.transcript = load_transcript(conn, conversation_id);
        self.error = None;
    }
}

/// (task_id, label) scope of a conversation, when it is task-scoped.
fn conversation_scope(conn: &Connection, conversation_id: i64) -> Option<(i64, String)> {
    let task_id = chronicle_core::storage::conversation_task(conn, conversation_id)
        .ok()
        .flatten()?;
    Some((task_id, task_label(conn, task_id)))
}

fn task_label(conn: &Connection, task_id: i64) -> String {
    conn.query_row("SELECT label FROM tasks WHERE id=?1", [task_id], |r| {
        r.get(0)
    })
    .unwrap_or_else(|_| format!("task {task_id}"))
}

fn load_transcript(conn: Option<&Connection>, conversation_id: i64) -> Vec<ChatMsg> {
    conn.and_then(|c| chronicle_core::storage::recent_chat_messages(c, conversation_id, 20).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|(role, text)| ChatMsg {
            user: role == "user",
            text,
        })
        .collect()
}

impl TimelineApp {
    /// Ensure a live worker on the most recent conversation (or a fresh one).
    fn chat_ensure(&mut self, ctx: &egui::Context) {
        if self.chat.is_some() {
            return;
        }
        let Some(conn) = self.conn.as_ref() else {
            return;
        };
        let latest = chronicle_core::storage::list_conversations(conn, 1)
            .ok()
            .and_then(|v| v.first().map(|(id, _, _)| *id));
        let conversation_id = match latest {
            Some(id) => id,
            None => match chronicle_core::storage::create_conversation(conn, Timestamp::now()) {
                Ok(id) => id,
                Err(e) => {
                    self.error = Some(format!("conversation create failed: {e}"));
                    return;
                }
            },
        };
        // The latest conversation may be task-scoped; the worker must match.
        let task_scope = conversation_scope(conn, conversation_id);
        match ChatPanel::spawn(ctx, self.conn.as_ref(), conversation_id, task_scope) {
            Ok(panel) => self.chat = Some(panel),
            Err(e) => self.error = Some(format!("chat worker spawn failed: {e}")),
        }
    }

    /// Route a "chat about task" click: resolve the task's conversation and
    /// point the panel (or a fresh worker) at it, task-scoped.
    fn chat_take_task_request(&mut self, ctx: &egui::Context) {
        let Some(task_id) = self.chat_task_request else {
            return;
        };
        let Some(conn) = self.conn.as_ref() else {
            return;
        };
        if self.chat.as_ref().is_some_and(|c| c.busy) {
            return; // keep the request; retried next frame
        }
        self.chat_task_request = None;
        let conversation_id =
            match chronicle_core::storage::conversation_for_task(conn, task_id, Timestamp::now()) {
                Ok(id) => id,
                Err(e) => {
                    self.error = Some(format!("task conversation failed: {e}"));
                    return;
                }
            };
        let scope = Some((task_id, task_label(conn, task_id)));
        match self.chat.as_mut() {
            Some(chat) => chat.switch_conversation(Some(conn), conversation_id, scope),
            None => match ChatPanel::spawn(ctx, self.conn.as_ref(), conversation_id, scope) {
                Ok(panel) => self.chat = Some(panel),
                Err(e) => self.error = Some(format!("chat worker spawn failed: {e}")),
            },
        }
    }

    /// Top-bar action: start a fresh conversation.
    pub(super) fn chat_new(&mut self) {
        let (Some(conn), Some(chat)) = (self.conn.as_ref(), self.chat.as_mut()) else {
            return;
        };
        if chat.busy {
            return;
        }
        match chronicle_core::storage::create_conversation(conn, Timestamp::now()) {
            Ok(id) => chat.switch_conversation(Some(conn), id, None),
            Err(e) => self.error = Some(format!("conversation create failed: {e}")),
        }
    }

    /// Top-bar menu listing past conversations by first question.
    pub(super) fn chat_history_menu(&mut self, ui: &mut egui::Ui) {
        let (Some(conn), Some(chat)) = (self.conn.as_ref(), self.chat.as_mut()) else {
            return;
        };
        let items = chronicle_core::storage::list_conversations(conn, 12).unwrap_or_default();
        let tz = self.tz.clone();
        ui.menu_button("history", |ui| {
            for (id, last_ts, snippet) in &items {
                let head: String = if snippet.is_empty() {
                    "(empty chat)".into()
                } else {
                    snippet.chars().take(40).collect()
                };
                let date = Timestamp::from_millisecond(*last_ts)
                    .map(|t| t.to_zoned(tz.clone()).strftime("%-d %b").to_string())
                    .unwrap_or_default();
                let current = *id == chat.conversation_id;
                if ui
                    .selectable_label(current, format!("{date} \u{b7} {head}"))
                    .clicked()
                {
                    let scope = conversation_scope(conn, *id);
                    chat.switch_conversation(Some(conn), *id, scope);
                    ui.close();
                }
            }
        });
    }

    /// True while the model is still loading (top-bar indicator).
    pub(super) fn chat_warming(&self) -> bool {
        self.chat.as_ref().is_some_and(|c| c.warming)
    }

    /// Clear the task scope: back to the newest general thread.
    fn chat_clear_scope(&mut self) {
        let (Some(conn), Some(chat)) = (self.conn.as_ref(), self.chat.as_mut()) else {
            return;
        };
        let general = chronicle_core::storage::latest_general_conversation(conn)
            .ok()
            .flatten()
            .or_else(|| chronicle_core::storage::create_conversation(conn, Timestamp::now()).ok());
        if let Some(id) = general {
            chat.switch_conversation(Some(conn), id, None);
        }
    }

    pub(super) fn chat_ui(&mut self, ui: &mut egui::Ui) {
        self.chat_take_task_request(ui.ctx());
        self.chat_ensure(ui.ctx());
        let Some(chat) = &mut self.chat else {
            egui::CentralPanel::default().show(ui, |ui| {
                ui.add_space(ui.available_height() * 0.35);
                theme::empty_state(ui, "chat unavailable", "");
                if let Some(error) = &self.error {
                    ui.vertical_centered(|ui| {
                        ui.colored_label(ui.visuals().error_fg_color, error);
                    });
                }
            });
            return;
        };
        chat.drain_events();
        let mut start_dl = false;
        let mut clear_scope = false;
        egui::Panel::bottom("chat_input").show(ui, |ui| {
            if let Some((_, label)) = &chat.task_scope {
                ui.horizontal(|ui| {
                    theme::badge(ui, &format!("scoped to {label}"), theme::palette::ACCENT);
                    if ui
                        .small_button("\u{2715}")
                        .on_hover_text("back to general chat")
                        .clicked()
                    {
                        clear_scope = true;
                    }
                });
            }
            if let Some(error) = &chat.error {
                egui::Frame::new()
                    .fill(theme::palette::RED.gamma_multiply(0.12))
                    .stroke(egui::Stroke::new(
                        1.0,
                        theme::palette::RED.gamma_multiply(0.4),
                    ))
                    .corner_radius(egui::CornerRadius::same(theme::RADIUS_MD))
                    .inner_margin(egui::Margin::same(8))
                    .show(ui, |ui| {
                        ui.set_width(ui.available_width());
                        ui.label(
                            egui::RichText::new("Chat unavailable")
                                .family(egui::FontFamily::Name(theme::MEDIUM.into()))
                                .color(theme::palette::RED),
                        );
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(error)
                                    .text_style(egui::TextStyle::Small)
                                    .color(theme::palette::TEXT_DIM),
                            )
                            .wrap(),
                        );
                        if error.contains("no model") && ui.small_button("download model").clicked()
                        {
                            start_dl = true;
                        }
                    });
            }
            ui.horizontal(|ui| {
                let can_send = !chat.busy && !chat.warming;
                let send_clicked = ui
                    .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let clicked = ui
                            .add_enabled(can_send, egui::Button::new("send"))
                            .clicked();
                        let edit = ui.add_sized(
                            ui.available_size(),
                            egui::TextEdit::singleline(&mut chat.input)
                                .hint_text("ask about your day"),
                        );
                        let entered =
                            edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if clicked || entered {
                            edit.request_focus();
                        }
                        clicked || entered
                    })
                    .inner;
                if send_clicked && can_send {
                    chat.send_question();
                }
            });
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    let max_w = ui.available_width() * 0.85;
                    if chat.transcript.is_empty() && !chat.warming {
                        ui.add_space(ui.available_height() * 0.35);
                        theme::empty_state(
                            ui,
                            "ask about your day",
                            "try \"what did I work on this morning?\"",
                        );
                    }
                    for msg in &chat.transcript {
                        if !msg.user && msg.text.is_empty() && chat.busy {
                            ui.horizontal(|ui| {
                                ui.add(egui::Spinner::new().size(14.0));
                                ui.weak("thinking\u{2026}");
                            });
                            ui.add_space(6.0);
                            continue;
                        }
                        let (fill, align) = if msg.user {
                            (
                                theme::palette::ACCENT.gamma_multiply(0.20),
                                egui::Align::Max,
                            )
                        } else {
                            (theme::palette::SURFACE, egui::Align::Min)
                        };
                        ui.with_layout(egui::Layout::top_down(align), |ui| {
                            egui::Frame::new()
                                .fill(fill)
                                .corner_radius(egui::CornerRadius::same(10))
                                .inner_margin(egui::Margin::symmetric(10, 6))
                                .show(ui, |ui| {
                                    ui.set_max_width(max_w);
                                    ui.label(&msg.text);
                                });
                        });
                        ui.add_space(6.0);
                    }
                });
        });
        if clear_scope {
            self.chat_clear_scope();
        }
        if start_dl {
            let ctx = ui.ctx().clone();
            self.start_model_download(&ctx, chronicle_derive::model::default_preset());
        }
    }
}

impl Drop for ChatPanel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
