//! Chat tab: owns the warm `chat-worker` child and renders the transcript.
//! Conversations live in the DB; the UI creates/lists them and tells the
//! worker which one to use.

use std::io::{BufRead, Write};
use std::process::Child;
use std::sync::mpsc;

use chronicle_core::types::{ActivityKind, ms_to_ts, ts_to_ms};
use eframe::egui;
use jiff::tz::TimeZone;
use jiff::{Timestamp, ToSpan, Zoned, civil};
use rusqlite::Connection;

use super::{TimelineApp, theme};

enum ChatEvent {
    Ready,
    Tok(String),
    Done,
    Err(String),
    Context(AnswerContext),
    /// Worker stdout closed (crash or exit).
    Exited,
}

/// What one answer was retrieved from (`WorkerMsg::Context`), for the
/// "Read 14 blocks · Thu 3 Sep 08:00–14:56" footer and the rows it expands
/// to.
struct AnswerContext {
    blocks: usize,
    /// None when the question carried no time reference and search stood in.
    start_ms: Option<i64>,
    end_ms: Option<i64>,
    rows: Vec<String>,
}

struct ChatMsg {
    user: bool,
    text: String,
    /// Answers streamed into this panel; None for messages read back from
    /// the DB, which stores the text but not the rows behind it.
    context: Option<AnswerContext>,
    /// Footer expanded to its row list.
    rows_open: bool,
}

impl ChatMsg {
    fn new(user: bool, text: String) -> Self {
        Self {
            user,
            text,
            context: None,
            rows_open: false,
        }
    }
}

/// The day's own rows behind the suggested questions, read once per reload:
/// the empty state and the follow-ups name work the user recognizes.
#[derive(Default, Clone)]
struct Seeds {
    /// Ticket ref (else label) of the task with the most time today.
    top_task: Option<String>,
    /// `HH:MM` of today's last call.
    last_call: Option<String>,
    /// Task labels and ticket refs with the task they open: what a reply
    /// links on.
    labels: Vec<(String, i64)>,
}

/// Where a link in an answer goes.
enum ChatLink {
    /// A clock time: the timeline on its day.
    Time(i64),
    /// A task label or ticket ref: that task's detail pane.
    Task(i64),
}

/// Chat state. Owns the warm `chat-worker` child; dropping the panel kills it
/// (README: model resident only while the chat tab is open).
pub(super) struct ChatPanel {
    child: Child,
    rx: mpsc::Receiver<ChatEvent>,
    /// None until the first question creates the row, so an untouched chat
    /// never reaches the history list.
    conversation_id: Option<i64>,
    /// Retrieval scoped to this task's workspace (m16): (task_id, label).
    task_scope: Option<(i64, String)>,
    transcript: Vec<ChatMsg>,
    input: String,
    /// Model still loading; no questions accepted yet.
    warming: bool,
    /// Question in flight (answer streaming in).
    busy: bool,
    error: Option<String>,
    /// History row whose "×" was clicked once; the next click deletes.
    delete_arm: Option<i64>,
    /// Past conversations for the history menu (id, last_ts, snippet);
    /// refreshed with the 5 s reload, not queried per frame.
    history: Vec<(i64, i64, String)>,
    /// Suggested-question material, refreshed with the same reload.
    seeds: Seeds,
}

impl Seeds {
    /// The day's rows behind the buttons: the task with the most time so
    /// far and the last call, plus the labels a reply can link on. The
    /// worker resolves the same day in the system zone.
    fn build(conn: &Connection) -> Self {
        use chronicle_core::storage;

        let tz = TimeZone::system();
        let now = Timestamp::now().to_zoned(tz.clone());
        let hi = ts_to_ms(now.timestamp());
        let lo = now
            .start_of_day()
            .map_or(hi - 24 * 3_600_000, |z| ts_to_ms(z.timestamp()));
        let tasks = storage::tasks_in_range(conn, lo, hi).unwrap_or_default();
        let top_task = chronicle_core::report::task_totals(&tasks, lo, hi)
            .first()
            .map(|top| {
                tasks
                    .iter()
                    .find(|t| t.id == top.task_id)
                    .and_then(|t| t.external_ref.clone())
                    .unwrap_or_else(|| top.label.clone())
            });
        let last_call = storage::activity_in_range(conn, lo, hi)
            .unwrap_or_default()
            .iter()
            .rev()
            .find(|e| e.kind == ActivityKind::Call)
            .map(|e| e.ts.to_zoned(tz.clone()).strftime("%H:%M").to_string());
        let mut labels: Vec<(String, i64)> = Vec::new();
        let open = storage::open_tasks(conn, 20).unwrap_or_default();
        let candidates = tasks
            .iter()
            .map(|t| (t.label.clone(), t.id))
            .chain(
                tasks
                    .iter()
                    .filter_map(|t| Some((t.external_ref.clone()?, t.id))),
            )
            .chain(open.iter().map(|t| (t.label.clone(), t.id)));
        for (text, id) in candidates {
            if text.len() >= 4 && !labels.iter().any(|(t, _)| *t == text) {
                labels.push((text, id));
            }
        }
        // Longest first, so "start dev on ACME-11382" wins the span over
        // the ref it contains.
        labels.sort_by_key(|(t, _)| std::cmp::Reverse(t.len()));
        Self {
            top_task,
            last_call,
            labels,
        }
    }

    /// Three openers for the empty state.
    fn openers(&self) -> Vec<String> {
        let mut out = vec!["what did I work on this morning?".to_owned()];
        if let Some(task) = &self.top_task {
            out.push(format!("how long on {task} this week?"));
        }
        if let Some(call) = &self.last_call {
            out.push(format!("what was I doing before the {call} call?"));
        }
        self.pad(&mut out, "", 3);
        out
    }

    /// Two next questions, varied by what was just asked.
    fn follow_ups(&self, question: &str) -> Vec<String> {
        let q = question.to_lowercase();
        let quantity = ["how long", "how much", "total", "time on"]
            .iter()
            .any(|p| q.contains(p));
        let mut out = Vec::new();
        if quantity {
            out.push("what did I actually do in that time?".to_owned());
        } else if let Some(task) = &self.top_task {
            out.push(format!("how long on {task} today?"));
        }
        if let Some(call) = &self.last_call
            && !q.contains("call")
        {
            out.push(format!("what was I doing before the {call} call?"));
        }
        self.pad(&mut out, &q, 2);
        out
    }

    /// Fill `out` up to `want` with generic questions the asked one doesn't
    /// already cover.
    fn pad(&self, out: &mut Vec<String>, asked: &str, want: usize) {
        for extra in [
            "what did I get done yesterday?",
            "which project took the most time this week?",
        ] {
            if out.len() >= want {
                break;
            }
            if extra
                .split(' ')
                .any(|w| w.len() > 4 && asked.contains(w.trim_end_matches('?')))
            {
                continue;
            }
            out.push(extra.to_owned());
        }
        out.truncate(want);
    }
}

impl ChatPanel {
    fn spawn(
        ctx: &egui::Context,
        conn: Option<&Connection>,
        conversation_id: Option<i64>,
        task_scope: Option<(i64, String)>,
    ) -> anyhow::Result<Self> {
        let mut child = crate::spawn_chat_worker(
            conversation_id.unwrap_or(0),
            task_scope.as_ref().map(|(id, _)| *id),
        )?;
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
                        Ok(WorkerMsg::Context {
                            blocks,
                            start_ms,
                            end_ms,
                            rows,
                        }) => ChatEvent::Context(AnswerContext {
                            blocks,
                            start_ms,
                            end_ms,
                            rows,
                        }),
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
        let history = conn
            .map(|c| chronicle_core::storage::list_conversations(c, 12).unwrap_or_default())
            .unwrap_or_default();
        let seeds = conn.map(Seeds::build).unwrap_or_default();
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
            delete_arm: None,
            history,
            seeds,
        })
    }

    /// Refreshes the history-menu and suggestion caches; called from
    /// `reload_if_stale`.
    pub(super) fn refresh_history(&mut self, conn: &Connection) {
        self.history = chronicle_core::storage::list_conversations(conn, 12).unwrap_or_default();
        self.seeds = Seeds::build(conn);
    }

    /// Kill the worker mid-answer and bring a fresh one up on the same
    /// conversation: the worker reads stdin only between questions and
    /// llama's token callback has no abort, so there is nothing to cancel
    /// short of the process. The partial answer stays on screen; it was
    /// never stored.
    fn restart(&mut self, ctx: &egui::Context, conn: Option<&Connection>) -> anyhow::Result<()> {
        let mut fresh = Self::spawn(ctx, conn, self.conversation_id, self.task_scope.clone())?;
        std::mem::swap(&mut fresh.transcript, &mut self.transcript);
        std::mem::swap(&mut fresh.input, &mut self.input);
        *self = fresh;
        Ok(())
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.rx.try_recv() {
            match event {
                ChatEvent::Ready => self.warming = false,
                ChatEvent::Tok(text) => match self.transcript.last_mut() {
                    Some(m) if !m.user => m.text.push_str(&text),
                    _ => self.transcript.push(ChatMsg::new(false, text)),
                },
                ChatEvent::Context(context) => {
                    if let Some(m) = self.transcript.last_mut().filter(|m| !m.user) {
                        m.context = Some(context);
                    }
                }
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

    /// Send the composer text.
    fn send_question(&mut self, conn: Option<&Connection>) {
        let ask = self.input.trim().to_owned();
        if self.ask(conn, ask) {
            self.input.clear();
        }
    }

    /// Ask `ask` (a suggestion button, or the composer). A conversation
    /// without a row yet gets one here (and the worker pointed at it) —
    /// never on view open. False = nothing was sent.
    fn ask(&mut self, conn: Option<&Connection>, ask: String) -> bool {
        if ask.is_empty() || self.busy || self.warming {
            return false;
        }
        if self.conversation_id.is_none() {
            let created = conn.ok_or_else(|| "no database".to_owned()).and_then(|c| {
                chronicle_core::storage::create_conversation(c, Timestamp::now())
                    .map_err(|e| format!("conversation create failed: {e}"))
            });
            let id = match created {
                Ok(id) => id,
                Err(e) => {
                    self.error = Some(e);
                    return false;
                }
            };
            let task_id = self.task_scope.as_ref().map(|(id, _)| *id);
            if !self.send_line(&crate::chatproto::ClientMsg::Switch {
                conversation_id: id,
                task_id,
            }) {
                return false;
            }
            self.conversation_id = Some(id);
        }
        if !self.send_line(&crate::chatproto::ClientMsg::Ask { ask: ask.clone() }) {
            return false;
        }
        self.transcript.push(ChatMsg::new(true, ask));
        self.transcript.push(ChatMsg::new(false, String::new()));
        self.busy = true;
        self.error = None;
        true
    }

    /// Point the panel (and worker) at another conversation, with its task
    /// scope (None = general retrieval). No-op while an answer is streaming.
    fn switch_conversation(
        &mut self,
        conn: Option<&Connection>,
        conversation_id: Option<i64>,
        task_scope: Option<(i64, String)>,
    ) {
        let same_scope =
            self.task_scope.as_ref().map(|(id, _)| *id) == task_scope.as_ref().map(|(id, _)| *id);
        if self.busy || (conversation_id == self.conversation_id && same_scope) {
            return;
        }
        if !self.send_line(&crate::chatproto::ClientMsg::Switch {
            conversation_id: conversation_id.unwrap_or(0),
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

fn load_transcript(conn: Option<&Connection>, conversation_id: Option<i64>) -> Vec<ChatMsg> {
    conn.zip(conversation_id)
        .and_then(|(c, id)| chronicle_core::storage::recent_chat_messages(c, id, 20).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|(role, text)| ChatMsg::new(role == "user", text))
        .collect()
}

impl TimelineApp {
    /// Ensure a live worker on the newest general conversation, or a fresh
    /// one whose row waits for the first question.
    fn chat_ensure(&mut self, ctx: &egui::Context) {
        if self.chat.is_some() {
            return;
        }
        let Some(conn) = self.conn.as_ref() else {
            return;
        };
        // Plain navigation never resumes a task-scoped thread: scope carries
        // in only via the task card's "chat" button (or an explicit history
        // pick). Resuming the newest conversation of any kind brought a
        // dismissed scope chip back on every tab switch.
        let latest = chronicle_core::storage::latest_general_conversation(conn)
            .ok()
            .flatten();
        match ChatPanel::spawn(ctx, self.conn.as_ref(), latest, None) {
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
            Some(chat) => chat.switch_conversation(Some(conn), Some(conversation_id), scope),
            None => match ChatPanel::spawn(ctx, self.conn.as_ref(), Some(conversation_id), scope) {
                Ok(panel) => self.chat = Some(panel),
                Err(e) => self.error = Some(format!("chat worker spawn failed: {e}")),
            },
        }
    }

    /// Top-bar action: start a fresh conversation (row created by its first
    /// question).
    pub(super) fn chat_new(&mut self) {
        let (Some(conn), Some(chat)) = (self.conn.as_ref(), self.chat.as_mut()) else {
            return;
        };
        if chat.busy {
            return;
        }
        chat.switch_conversation(Some(conn), None, None);
    }

    /// Top-bar menu listing past conversations by first question. "×" on a
    /// row arms a delete; the confirming click ("delete?") removes the
    /// conversation with its messages. Deleting the open one starts fresh.
    /// Wide windows list them in the panel's own left column instead.
    pub(super) fn chat_history_menu(&mut self, ui: &mut egui::Ui) {
        if theme::wide(ui.ctx()) {
            return;
        }
        let (Some(conn), Some(chat)) = (self.conn.as_mut(), self.chat.as_mut()) else {
            return;
        };
        let tz = self.tz.clone();
        let mut error: Option<String> = None;
        // Default menus close on any inner click, which would drop the armed
        // "\u{d7}" before its confirming click can land.
        let config = egui::containers::menu::MenuConfig::new()
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside);
        let (_, inner) = egui::containers::menu::MenuButton::new("history")
            .config(config)
            .ui(ui, |ui| error = history_list(ui, conn, chat, &tz, true));
        let open = inner.is_some();
        if !open {
            chat.delete_arm = None;
        }
        if error.is_some() {
            self.error = error;
        }
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
            .flatten();
        chat.switch_conversation(Some(conn), general, None);
    }

    /// "stop" under a streaming answer.
    fn chat_stop(&mut self, ctx: &egui::Context) {
        let Some(chat) = self.chat.as_mut() else {
            return;
        };
        if let Err(e) = chat.restart(ctx, self.conn.as_ref()) {
            self.error = Some(format!("chat worker spawn failed: {e}"));
        }
    }

    /// Follow a link in an answer: the timeline on the linked time's day,
    /// or the linked task's detail pane on the day it was last touched.
    fn chat_open_link(&mut self, link: ChatLink) {
        let day = match link {
            ChatLink::Time(ms) => {
                self.selected_task = None;
                Some(ms_to_ts(ms).to_zoned(self.tz.clone()).date())
            }
            ChatLink::Task(task_id) => {
                self.selected_task = Some(task_id);
                self.conn
                    .as_ref()
                    .and_then(|c| chronicle_core::storage::task_last_end(c, task_id).ok())
                    .flatten()
                    .map(|ms| ms_to_ts(ms).to_zoned(self.tz.clone()).date())
            }
        };
        if let Some(day) = day {
            self.set_day(day);
        }
        self.loaded_at = None;
        self.view = super::View::Timeline;
    }

    pub(super) fn chat_ui(&mut self, ui: &mut egui::Ui) {
        self.chat_take_task_request(ui.ctx());
        self.chat_ensure(ui.ctx());
        // Wide window: past conversations get a column of their own (the
        // top-bar menu stands down), added before the input and the page.
        if theme::wide(ui.ctx()) {
            let tz = self.tz.clone();
            let mut error: Option<String> = None;
            if let (Some(conn), Some(chat)) = (self.conn.as_mut(), self.chat.as_mut()) {
                egui::Panel::left("chat_history")
                    .frame(theme::page_frame())
                    .resizable(false)
                    .default_size(200.0)
                    .show(ui, |ui| {
                        theme::section_header_with(ui, "History", None, |_| {});
                        ui.add_space(theme::SPACE_SM);
                        egui::ScrollArea::vertical()
                            .id_salt("chat_history_col")
                            .auto_shrink(false)
                            .show(ui, |ui| {
                                error = history_list(ui, conn, chat, &tz, false);
                            });
                    });
            }
            if error.is_some() {
                self.error = error;
            }
        }
        let tz = self.tz.clone();
        let shown_day = self.day;
        let conn = self.conn.as_ref();
        let Some(chat) = &mut self.chat else {
            theme::page().show(ui, |ui| {
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
        let mut stop = false;
        let mut ask: Option<String> = None;
        let mut nav: Option<ChatLink> = None;
        egui::Panel::bottom("chat_input")
            .frame(theme::page_frame())
            .show(ui, |ui| {
                if let Some((_, label)) = &chat.task_scope {
                    ui.horizontal(|ui| {
                        theme::badge(ui, &format!("scoped to {label}"), theme::palette::ACCENT);
                        if theme::ghost_button(ui, "\u{d7}")
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
                            if error.contains("no model")
                                && ui.small_button("download model").clicked()
                            {
                                start_dl = true;
                            }
                        });
                }
                ui.horizontal_top(|ui| {
                    let can_send = !chat.busy && !chat.warming;
                    let send_clicked = ui
                        .with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            let clicked = if chat.busy {
                                stop = theme::secondary_button(ui, "stop")
                                    .on_hover_text("stop this answer")
                                    .clicked();
                                false
                            } else {
                                theme::primary_button_enabled(ui, can_send, "send").clicked()
                            };
                            // Enter sends, shift+enter is a newline: the key
                            // has to be taken off the queue before the
                            // multiline edit sees it and inserts one.
                            let id = egui::Id::new("chat_composer");
                            let entered = can_send
                                && ui.memory(|m| m.has_focus(id))
                                && ui.input_mut(|i| {
                                    i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                                });
                            let edit = ui.add(
                                egui::TextEdit::multiline(&mut chat.input)
                                    .id(id)
                                    .desired_rows(1)
                                    .desired_width(f32::INFINITY)
                                    .hint_text("ask about your day"),
                            );
                            if clicked || entered {
                                edit.request_focus();
                            }
                            clicked || entered
                        })
                        .inner;
                    if send_clicked && can_send {
                        chat.send_question(conn);
                    }
                });
            });
        theme::page().show(ui, |ui| {
            egui::ScrollArea::vertical()
                .auto_shrink(false)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    let max_w = ui.available_width() * 0.85;
                    let busy = chat.busy;
                    if chat.transcript.is_empty() && !chat.warming {
                        ui.add_space(ui.available_height() * 0.25);
                        theme::empty_state(ui, "ask about your day", "pick one, or type your own");
                        ui.vertical_centered(|ui| {
                            for question in chat.seeds.openers() {
                                if theme::secondary_button(ui, &question).clicked() {
                                    ask = Some(question);
                                }
                                ui.add_space(theme::SPACE_XS);
                            }
                        });
                    }
                    let labels = &chat.seeds.labels;
                    for (i, msg) in chat.transcript.iter_mut().enumerate() {
                        if !msg.user && msg.text.is_empty() && busy {
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
                                    if msg.user {
                                        ui.label(&msg.text);
                                    } else {
                                        let links = LinkCtx {
                                            labels,
                                            day: link_day(msg.context.as_ref(), shown_day, &tz),
                                        };
                                        markdown(ui, &msg.text, &links, &mut nav);
                                    }
                                });
                            if !msg.user && !msg.text.is_empty() {
                                answer_footer(ui, i, msg, &tz);
                            }
                        });
                        ui.add_space(6.0);
                    }
                    let answered = chat
                        .transcript
                        .last()
                        .is_some_and(|m| !m.user && !m.text.is_empty());
                    if answered && !busy {
                        let question = chat
                            .transcript
                            .iter()
                            .rev()
                            .find(|m| m.user)
                            .map(|m| m.text.clone())
                            .unwrap_or_default();
                        ui.horizontal_wrapped(|ui| {
                            for follow_up in chat.seeds.follow_ups(&question) {
                                if theme::secondary_button(ui, &follow_up).clicked() {
                                    ask = Some(follow_up);
                                }
                            }
                        });
                    }
                });
        });
        if let Some(question) = ask {
            chat.ask(conn, question);
        }
        if stop {
            self.chat_stop(ui.ctx());
        }
        if let Some(link) = nav {
            self.chat_open_link(link);
        }
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

/// Render the model's answer with the light markdown it tends to emit:
/// `**bold**`, `*italic*`, `` `code` ``, `-`/`*`/`1.` list items and `#`
/// headings. Anything else is plain text; unmatched markers stay literal.
/// Clock times and known task labels in it become links; a clicked one
/// lands in `nav`.
fn markdown(ui: &mut egui::Ui, text: &str, links: &LinkCtx, nav: &mut Option<ChatLink>) {
    let base = egui::TextStyle::Body.resolve(ui.style());
    let color = ui.visuals().text_color();
    let width = ui.available_width();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            ui.add_space(4.0);
            continue;
        }
        let heading = trimmed.starts_with('#');
        let body = if heading {
            trimmed.trim_start_matches('#').trim_start()
        } else {
            trimmed
        };
        let (marker, body) = list_marker(body);
        match marker {
            Some(m) => {
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    ui.add_sized([12.0, base.size * 1.4], egui::Label::new(m));
                    line_body(ui, body, &base, color, heading, width - 18.0, links, nav);
                });
            }
            None => line_body(ui, body, &base, color, heading, width, links, nav),
        }
    }
}

/// One line of an answer. Without links it is a single wrapped label; with
/// them the line is laid out as alternating text and link widgets, the
/// inline `**`/`*`/`` ` `` state carrying across the pieces.
#[allow(clippy::too_many_arguments)]
fn line_body(
    ui: &mut egui::Ui,
    body: &str,
    base: &egui::FontId,
    color: egui::Color32,
    heading: bool,
    width: f32,
    links: &LinkCtx,
    nav: &mut Option<ChatLink>,
) {
    let mut state = Marks::new(heading);
    let spans = link_spans(body, links);
    if spans.is_empty() {
        let mut job = egui::text::LayoutJob::default();
        job.wrap.max_width = width;
        inline(&mut job, body, base, color, &mut state);
        ui.add(egui::Label::new(job).wrap());
        return;
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        let mut at = 0;
        for (start, end, link) in spans {
            fragment(ui, &body[at..start], base, color, &mut state);
            if ui.link(&body[start..end]).clicked() {
                *nav = Some(link);
            }
            at = end;
        }
        fragment(ui, &body[at..], base, color, &mut state);
    });
}

/// A run of text between two links, formatted from the running mark state.
fn fragment(
    ui: &mut egui::Ui,
    text: &str,
    base: &egui::FontId,
    color: egui::Color32,
    state: &mut Marks,
) {
    if text.is_empty() {
        return;
    }
    let mut job = egui::text::LayoutJob::default();
    inline(&mut job, text, base, color, state);
    if !job.sections.is_empty() {
        ui.add(egui::Label::new(job).wrap());
    }
}

/// Split a leading list marker off `s`: `- `, `* `, `• ` become a bullet,
/// `N. ` keeps its number.
fn list_marker(s: &str) -> (Option<String>, &str) {
    for b in ["- ", "* ", "\u{2022} "] {
        if let Some(rest) = s.strip_prefix(b) {
            return (Some("\u{2022}".into()), rest);
        }
    }
    let digits = s.bytes().take_while(u8::is_ascii_digit).count();
    if digits > 0
        && digits <= 2
        && let Some(rest) = s[digits..].strip_prefix(". ")
    {
        return (Some(s[..digits + 1].to_string()), rest);
    }
    (None, s)
}

/// Inline markers open across a line: a `**` before a link and its closing
/// `**` after it are one run.
struct Marks {
    bold: bool,
    italic: bool,
    code: bool,
}

impl Marks {
    fn new(heading: bool) -> Self {
        Self {
            bold: heading,
            italic: false,
            code: false,
        }
    }
}

/// Append `s` to `job`, toggling bold / italic / code on their markers.
fn inline(
    job: &mut egui::text::LayoutJob,
    s: &str,
    base: &egui::FontId,
    color: egui::Color32,
    state: &mut Marks,
) {
    let medium = egui::FontId::new(base.size, egui::FontFamily::Name(theme::MEDIUM.into()));
    let mono = egui::FontId::new(base.size * 0.92, egui::FontFamily::Monospace);
    let (mut bold, mut italic, mut code) = (state.bold, state.italic, state.code);
    let mut buf = String::new();
    let flush = |job: &mut egui::text::LayoutJob,
                 buf: &mut String,
                 bold: bool,
                 italic: bool,
                 code: bool| {
        if buf.is_empty() {
            return;
        }
        let mut fmt = egui::TextFormat {
            font_id: if code {
                mono.clone()
            } else if bold {
                medium.clone()
            } else {
                base.clone()
            },
            color,
            italics: italic,
            ..Default::default()
        };
        if code {
            fmt.background = color.gamma_multiply(0.12);
        }
        job.append(buf, 0.0, fmt);
        buf.clear();
    };
    let mut rest = s;
    while !rest.is_empty() {
        if code {
            if let Some(r) = rest.strip_prefix('`') {
                flush(job, &mut buf, bold, italic, code);
                code = false;
                rest = r;
                continue;
            }
        } else if let Some(r) = rest.strip_prefix("**") {
            flush(job, &mut buf, bold, italic, code);
            bold = !bold;
            rest = r;
            continue;
        } else if let Some(r) = rest.strip_prefix('`') {
            flush(job, &mut buf, bold, italic, code);
            code = true;
            rest = r;
            continue;
        } else if let Some(r) = rest.strip_prefix('*') {
            flush(job, &mut buf, bold, italic, code);
            italic = !italic;
            rest = r;
            continue;
        }
        let ch = rest.chars().next().unwrap();
        buf.push(ch);
        rest = &rest[ch.len_utf8()..];
    }
    flush(job, &mut buf, bold, italic, code);
    *state = Marks { bold, italic, code };
}

/// What a line's links are resolved against: the task labels to spot, and
/// the day a bare `HH:MM` belongs to.
struct LinkCtx<'a> {
    labels: &'a [(String, i64)],
    day: Zoned,
}

/// The day an answer's clock times belong to: the range it was built from,
/// else the day the app is showing.
fn link_day(context: Option<&AnswerContext>, shown: civil::Date, tz: &TimeZone) -> Zoned {
    context
        .and_then(|c| c.start_ms)
        .map(|ms| ms_to_ts(ms).to_zoned(tz.clone()))
        .or_else(|| shown.to_zoned(tz.clone()).ok())
        .unwrap_or_else(|| Timestamp::now().to_zoned(tz.clone()))
}

/// Link spans in one line as byte ranges: clock times (a `HH:MM–HH:MM`
/// range opens its start) and known task labels or ticket refs, longest
/// label first, no two spans overlapping.
fn link_spans(line: &str, ctx: &LinkCtx) -> Vec<(usize, usize, ChatLink)> {
    let mut out: Vec<(usize, usize, ChatLink)> = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i + 5 <= bytes.len() {
        let boundary = i == 0 || !(bytes[i - 1].is_ascii_digit() || bytes[i - 1] == b':');
        if !boundary || !is_clock(&bytes[i..]) {
            i += 1;
            continue;
        }
        let mut end = i + 5;
        for sep in ["\u{2013}", "\u{2014}", "-"] {
            if let Some(rest) = line[end..].strip_prefix(sep)
                && is_clock(rest.as_bytes())
            {
                end += sep.len() + 5;
                break;
            }
        }
        let hour = (bytes[i] - b'0') * 10 + (bytes[i + 1] - b'0');
        let minute = (bytes[i + 3] - b'0') * 10 + (bytes[i + 4] - b'0');
        if let Some(ms) = clock_ms(&ctx.day, i64::from(hour), i64::from(minute)) {
            out.push((i, end, ChatLink::Time(ms)));
        }
        i = end;
    }
    for (label, task_id) in ctx.labels {
        let Some(start) = find_ci(line, label) else {
            continue;
        };
        let end = start + label.len();
        if !line.is_char_boundary(start) || !line.is_char_boundary(end) {
            continue;
        }
        let edges = line[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric())
            && line[end..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_alphanumeric());
        if edges && !out.iter().any(|(s, e, _)| start < *e && end > *s) {
            out.push((start, end, ChatLink::Task(*task_id)));
        }
    }
    out.sort_by_key(|(start, _, _)| *start);
    out
}

/// `HH:MM` at the head of `bytes`, with no digit running on after it.
fn is_clock(bytes: &[u8]) -> bool {
    if bytes.len() < 5
        || !bytes[..2].iter().all(u8::is_ascii_digit)
        || bytes[2] != b':'
        || !bytes[3..5].iter().all(u8::is_ascii_digit)
        || bytes.get(5).is_some_and(u8::is_ascii_digit)
    {
        return false;
    }
    let hour = (bytes[0] - b'0') * 10 + (bytes[1] - b'0');
    let minute = (bytes[3] - b'0') * 10 + (bytes[4] - b'0');
    hour < 24 && minute < 60
}

fn clock_ms(day: &Zoned, hour: i64, minute: i64) -> Option<i64> {
    let start = day.start_of_day().ok()?;
    let at = start.checked_add(hour.hours().minutes(minute)).ok()?;
    Some(ts_to_ms(at.timestamp()))
}

/// First ASCII-case-insensitive occurrence of `needle` in `hay`.
fn find_ci(hay: &str, needle: &str) -> Option<usize> {
    let (hay, needle) = (hay.as_bytes(), needle.as_bytes());
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).find(|&i| hay[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

/// "Read 14 blocks · Thu 3 Sep 08:00–14:56" under an answer, expanding to
/// the rows the model was given (Settings' "show digest", for a reply).
fn answer_footer(ui: &mut egui::Ui, index: usize, msg: &mut ChatMsg, tz: &TimeZone) {
    let ChatMsg {
        text,
        context,
        rows_open,
        ..
    } = msg;
    ui.horizontal(|ui| {
        if theme::ghost_button(
            ui,
            egui::RichText::new("copy")
                .text_style(egui::TextStyle::Small)
                .color(theme::palette::TEXT_DIM),
        )
        .clicked()
        {
            ui.ctx().copy_text(text.clone());
        }
        let Some(context) = context.as_ref() else {
            return;
        };
        let label = match (context.start_ms, context.end_ms) {
            (Some(lo), Some(hi)) => format!(
                "Read {} blocks \u{b7} {}",
                context.blocks,
                range_text(lo, hi, tz)
            ),
            _ => format!("Searched {} blocks", context.blocks),
        };
        if theme::ghost_button(
            ui,
            egui::RichText::new(label)
                .text_style(egui::TextStyle::Small)
                .color(theme::palette::TEXT_DIM),
        )
        .on_hover_text("the rows this answer was built from")
        .clicked()
        {
            *rows_open = !*rows_open;
        }
    });
    let (Some(context), true) = (context.as_ref(), *rows_open) else {
        return;
    };
    egui::ScrollArea::vertical()
        .id_salt(("chat_context_rows", index))
        .max_height(200.0)
        .show(ui, |ui| {
            for row in &context.rows {
                ui.label(
                    egui::RichText::new(row)
                        .text_style(egui::TextStyle::Small)
                        .color(theme::palette::TEXT_DIM),
                );
            }
        });
}

/// A context range as "Thu 3 Sep 08:00–14:56", or both dates when it spans
/// more than one day.
fn range_text(lo: i64, hi: i64, tz: &TimeZone) -> String {
    let (from, to) = (
        ms_to_ts(lo).to_zoned(tz.clone()),
        ms_to_ts(hi).to_zoned(tz.clone()),
    );
    if from.date() == to.date() {
        format!(
            "{} {}\u{2013}{}",
            from.strftime("%a %-d %b"),
            from.strftime("%H:%M"),
            to.strftime("%H:%M")
        )
    } else {
        format!(
            "{} \u{2013} {}",
            from.strftime("%a %-d %b %H:%M"),
            to.strftime("%a %-d %b %H:%M")
        )
    }
}

/// The conversation list behind both the top-bar menu and the wide-mode
/// left column: a row per conversation, "×" arming a delete that the next
/// click confirms. Returns an error for the caller to surface.
fn history_list(
    ui: &mut egui::Ui,
    conn: &mut Connection,
    chat: &mut ChatPanel,
    tz: &TimeZone,
    in_menu: bool,
) -> Option<String> {
    let items = chat.history.clone();
    let mut error: Option<String> = None;
    if items.is_empty() {
        ui.weak("no conversations yet");
    }
    for (id, last_ts, snippet) in &items {
        let head: String = if snippet.is_empty() {
            "(empty chat)".into()
        } else {
            snippet.chars().take(40).collect()
        };
        let date = Timestamp::from_millisecond(*last_ts)
            .map(|t| t.to_zoned(tz.clone()).strftime("%-d %b").to_string())
            .unwrap_or_default();
        let current = chat.conversation_id == Some(*id);
        ui.horizontal(|ui| {
            if theme::selectable(ui, current, format!("{date} \u{b7} {head}")).clicked() {
                let scope = conversation_scope(conn, *id);
                chat.switch_conversation(Some(&*conn), Some(*id), scope);
                if in_menu {
                    ui.close();
                }
            }
            // No delete under a streaming answer: the worker is still
            // writing into this conversation.
            if current && chat.busy {
                return;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let armed = chat.delete_arm == Some(*id);
                let label = if armed {
                    egui::RichText::new("delete?").color(theme::palette::RED)
                } else {
                    egui::RichText::new("\u{d7}")
                };
                if !theme::ghost_button(ui, label).clicked() {
                    return;
                }
                if !armed {
                    chat.delete_arm = Some(*id);
                    return;
                }
                chat.delete_arm = None;
                if let Err(e) = chronicle_core::storage::delete_conversation(conn, *id) {
                    error = Some(format!("delete failed: {e}"));
                } else {
                    chat.history.retain(|(hid, _, _)| hid != id);
                    if current {
                        chat.switch_conversation(Some(&*conn), None, None);
                    }
                }
            });
        });
    }
    error
}
