//! Chat panel: owns the warm `chat-worker` child and renders the transcript.

use std::io::{BufRead, Write};
use std::process::Child;
use std::sync::mpsc;

use eframe::egui;
use rusqlite::Connection;

use super::TimelineApp;

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

/// Chat panel state. Owns the warm `chat-worker` child; dropping the panel
/// kills it (README: model resident only while the panel is open).
pub(super) struct ChatPanel {
    child: Child,
    rx: mpsc::Receiver<ChatEvent>,
    transcript: Vec<ChatMsg>,
    input: String,
    /// Model still loading; no questions accepted yet.
    warming: bool,
    /// Question in flight (answer streaming in).
    busy: bool,
    error: Option<String>,
}

impl ChatPanel {
    fn spawn(ctx: &egui::Context, conn: Option<&Connection>) -> anyhow::Result<Self> {
        let mut child = crate::spawn_chat_worker()?;
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
        // Prior conversation tail; the worker preloads the same history.
        let transcript = conn
            .and_then(|c| chronicle_core::storage::recent_chat_messages(c, 20).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|(role, text)| ChatMsg {
                user: role == "user",
                text,
            })
            .collect();
        Ok(Self {
            child,
            rx,
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

    fn send_question(&mut self) {
        let ask = self.input.trim().to_owned();
        if ask.is_empty() || self.busy || self.warming {
            return;
        }
        let line = serde_json::to_string(&crate::chatproto::Ask { ask: ask.clone() })
            .expect("ask serializes");
        let Some(stdin) = self.child.stdin.as_mut() else {
            self.error = Some("chat worker not reachable".into());
            return;
        };
        if stdin
            .write_all(format!("{line}\n").as_bytes())
            .and_then(|()| stdin.flush())
            .is_err()
        {
            self.error = Some("chat worker not reachable".into());
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
}

impl Drop for ChatPanel {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl TimelineApp {
    pub(super) fn toggle_chat(&mut self, ctx: &egui::Context) {
        if self.chat.is_some() {
            self.chat = None; // Drop kills the worker
            return;
        }
        match ChatPanel::spawn(ctx, self.conn.as_ref()) {
            Ok(panel) => self.chat = Some(panel),
            Err(e) => self.error = Some(format!("chat worker spawn failed: {e}")),
        }
    }

    pub(super) fn chat_panel_ui(&mut self, ui: &mut egui::Ui) {
        let Some(chat) = &mut self.chat else { return };
        chat.drain_events();
        let mut close = false;
        egui::Panel::right("chat_panel")
            .default_size(300.0)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.strong("Chat");
                    if chat.warming {
                        ui.weak("loading model\u{2026}");
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("close").clicked() {
                            close = true;
                        }
                    });
                });
                egui::Panel::bottom("chat_input").show(ui, |ui| {
                    if let Some(error) = &chat.error {
                        ui.colored_label(ui.visuals().error_fg_color, error);
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
                                let entered = edit.lost_focus()
                                    && ui.input(|i| i.key_pressed(egui::Key::Enter));
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
                            for msg in &chat.transcript {
                                if msg.user {
                                    ui.strong(format!("you: {}", msg.text));
                                } else if msg.text.is_empty() && chat.busy {
                                    ui.weak("thinking\u{2026}");
                                } else {
                                    ui.label(&msg.text);
                                }
                                ui.add_space(6.0);
                            }
                        });
                });
            });
        if close {
            self.chat = None;
        }
    }
}
