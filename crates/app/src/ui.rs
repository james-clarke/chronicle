//! Timeline window, run as a `chronicle ui` child process. The daemon writes
//! "toggle\n" to our stdin to raise the window; closing it exits the process.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use eframe::egui;
use jiff::civil;
use jiff::tz::TimeZone;
use jiff::{ToSpan, Zoned};
use rusqlite::Connection;

const RELOAD_EVERY: Duration = Duration::from_secs(5);
/// Idle wake-up cadence; the only repaint source besides user input.
const WAKE_EVERY: Duration = Duration::from_secs(10);

pub fn run(data_dir: &Path) -> anyhow::Result<()> {
    let db_path = data_dir.join("chronicle.db");
    let config_path = data_dir.join("config.toml");
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Chronicle")
            .with_inner_size([560.0, 760.0]),
        ..Default::default()
    };
    let sock_path = crate::socket_path(data_dir);
    eframe::run_native(
        "chronicle",
        options,
        Box::new(move |cc| {
            spawn_stdin_listener(cc.egui_ctx.clone());
            Ok(Box::new(TimelineApp::new(db_path, sock_path, config_path)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}

fn spawn_stdin_listener(ctx: egui::Context) {
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim() == "toggle" {
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                ctx.request_repaint();
            }
        }
    });
}

struct SpanRow {
    start: Zoned,
    end: Zoned,
    app: String,
    title: String,
    kind: String,
}

struct TaskRow {
    id: i64,
    start: Zoned,
    end: Zoned,
    label: String,
    project: Option<String>,
    confidence: f64,
}

/// In-flight label/project edit of one task row; committing writes a
/// `corrections` row (M5 few-shot source) and updates the task.
struct EditState {
    task_id: i64,
    label: String,
    project: String,
}

struct TimelineApp {
    db_path: PathBuf,
    sock_path: PathBuf,
    conn: Option<Connection>,
    tz: TimeZone,
    day: civil::Date,
    spans: Vec<SpanRow>,
    tasks: Vec<TaskRow>,
    edit: Option<EditState>,
    loaded_at: Option<Instant>,
    error: Option<String>,
    /// Daemon status flag from `meta` (e.g. AW endpoint port conflict).
    warning: Option<String>,
    /// Some = chat panel open, warm worker child alive.
    chat: Option<ChatPanel>,
    config_path: PathBuf,
    /// Some = settings window open.
    settings: Option<SettingsPanel>,
}

/// Editable view of config.toml. Numbers bind directly; list/path fields are
/// edited as text and parsed on save. Saving rewrites the whole file (hand
/// comments are lost); the daemon reads config at startup, so changes apply
/// on its next restart.
struct SettingsPanel {
    batch_minutes: u32,
    afk_close_secs: u32,
    derive_idle_secs: u32,
    retention_days: u32,
    port: u16,
    model_path: String,
    mcp_config: String,
    excluded_apps: String,
    excluded_titles: String,
    /// Config as loaded; fields without widgets pass through on save.
    base: chronicle_core::config::Config,
    status: Option<Result<String, String>>,
}

impl SettingsPanel {
    fn load(config_path: &Path) -> Result<Self, String> {
        let config =
            chronicle_core::config::Config::load(config_path).map_err(|e| e.to_string())?;
        Ok(Self {
            batch_minutes: config.batch_minutes,
            afk_close_secs: config.afk_close_secs,
            derive_idle_secs: config.derive_idle_secs,
            retention_days: config.retention_days,
            port: config.port,
            model_path: path_str(&config.model_path),
            mcp_config: path_str(&config.mcp_config),
            excluded_apps: config.excluded_apps.join("\n"),
            excluded_titles: config.excluded_titles.join("\n"),
            base: config,
            status: None,
        })
    }

    fn save(&self, config_path: &Path) -> Result<(), String> {
        let mut config = self.base.clone();
        config.batch_minutes = self.batch_minutes;
        config.afk_close_secs = self.afk_close_secs;
        config.derive_idle_secs = self.derive_idle_secs;
        config.retention_days = self.retention_days;
        config.port = self.port;
        config.model_path = opt_path(&self.model_path);
        config.mcp_config = opt_path(&self.mcp_config);
        config.excluded_apps = regex_lines(&self.excluded_apps)?;
        config.excluded_titles = regex_lines(&self.excluded_titles)?;
        let toml = toml::to_string_pretty(&config).map_err(|e| e.to_string())?;
        std::fs::write(config_path, toml).map_err(|e| e.to_string())
    }
}

fn path_str(p: &Option<PathBuf>) -> String {
    p.as_deref()
        .map_or_else(String::new, |p| p.display().to_string())
}

fn opt_path(s: &str) -> Option<PathBuf> {
    let s = s.trim();
    (!s.is_empty()).then(|| PathBuf::from(s))
}

/// One regex per line; each must compile so a typo can't silently disable
/// capture filtering after the next daemon restart.
fn regex_lines(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        regex::Regex::new(line).map_err(|e| format!("bad regex \"{line}\": {e}"))?;
        out.push(line.to_owned());
    }
    Ok(out)
}

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
struct ChatPanel {
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
    fn new(db_path: PathBuf, sock_path: PathBuf, config_path: PathBuf) -> Self {
        let tz = TimeZone::system();
        let day = Zoned::now().with_time_zone(tz.clone()).date();
        Self {
            db_path,
            sock_path,
            conn: None,
            tz,
            day,
            spans: Vec::new(),
            tasks: Vec::new(),
            edit: None,
            loaded_at: None,
            error: None,
            warning: None,
            chat: None,
            config_path,
            settings: None,
        }
    }

    fn toggle_settings(&mut self) {
        if self.settings.is_some() {
            self.settings = None;
            return;
        }
        match SettingsPanel::load(&self.config_path) {
            Ok(panel) => self.settings = Some(panel),
            Err(e) => self.error = Some(format!("config load failed: {e}")),
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let config_path = self.config_path.clone();
        let Some(panel) = &mut self.settings else {
            return;
        };
        let mut open = true;
        egui::Window::new("settings")
            .open(&mut open)
            .resizable(true)
            .default_width(360.0)
            .show(ctx, |ui| {
                egui::Grid::new("settings_nums")
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.label("batch minutes");
                        ui.add(egui::DragValue::new(&mut panel.batch_minutes).range(5..=240));
                        ui.end_row();
                        ui.label("afk close secs");
                        ui.add(egui::DragValue::new(&mut panel.afk_close_secs).range(30..=3600));
                        ui.end_row();
                        ui.label("derive idle secs");
                        ui.add(egui::DragValue::new(&mut panel.derive_idle_secs).range(60..=3600));
                        ui.end_row();
                        ui.label("retention days (0 = keep forever)");
                        ui.add(egui::DragValue::new(&mut panel.retention_days).range(0..=3650));
                        ui.end_row();
                        ui.label("aw endpoint port");
                        ui.add(egui::DragValue::new(&mut panel.port).range(1024..=65535));
                        ui.end_row();
                    });
                ui.separator();
                ui.label("model path (empty = default preset)");
                ui.text_edit_singleline(&mut panel.model_path);
                ui.label("mcp config path (empty = mcp.toml in data dir)");
                ui.text_edit_singleline(&mut panel.mcp_config);
                ui.label("excluded apps (one regex per line, never stored)");
                ui.add(egui::TextEdit::multiline(&mut panel.excluded_apps).desired_rows(2));
                ui.label("excluded titles (one regex per line)");
                ui.add(egui::TextEdit::multiline(&mut panel.excluded_titles).desired_rows(2));
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("save").clicked() {
                        panel.status = Some(match panel.save(&config_path) {
                            Ok(()) => Ok("saved \u{2014} restart daemon to apply".into()),
                            Err(e) => Err(e),
                        });
                    }
                    match &panel.status {
                        Some(Ok(msg)) => {
                            ui.weak(msg.as_str());
                        }
                        Some(Err(msg)) => {
                            ui.colored_label(ui.visuals().error_fg_color, msg);
                        }
                        None => {}
                    }
                });
            });
        if !open {
            self.settings = None;
        }
    }

    fn toggle_chat(&mut self, ctx: &egui::Context) {
        if self.chat.is_some() {
            self.chat = None; // Drop kills the worker
            return;
        }
        match ChatPanel::spawn(ctx, self.conn.as_ref()) {
            Ok(panel) => self.chat = Some(panel),
            Err(e) => self.error = Some(format!("chat worker spawn failed: {e}")),
        }
    }

    fn chat_panel_ui(&mut self, ui: &mut egui::Ui) {
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

    fn shift_day(&mut self, days: i64) {
        if let Ok(day) = self.day.checked_add(days.days()) {
            self.day = day;
            self.loaded_at = None;
        }
    }

    fn reload_if_stale(&mut self) {
        if self.loaded_at.is_some_and(|t| t.elapsed() < RELOAD_EVERY) {
            return;
        }
        self.loaded_at = Some(Instant::now());
        match self.load_spans().and_then(|spans| {
            let tasks = self.load_tasks()?;
            Ok((spans, tasks))
        }) {
            Ok((spans, tasks)) => {
                self.spans = spans;
                self.tasks = tasks;
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
        if let Some(conn) = self.conn.as_ref() {
            self.warning = chronicle_core::storage::get_meta(conn, "server_error")
                .ok()
                .flatten();
        }
    }

    fn day_range_ms(&self) -> anyhow::Result<(i64, i64)> {
        let start = self.day.to_zoned(self.tz.clone())?;
        let end = start.checked_add(1.day())?;
        Ok((
            start.timestamp().as_millisecond(),
            end.timestamp().as_millisecond(),
        ))
    }

    fn load_tasks(&mut self) -> anyhow::Result<Vec<TaskRow>> {
        let (lo, hi) = self.day_range_ms()?;
        let conn = self.conn.as_ref().expect("connection opened by load_spans");
        let mut stmt = conn.prepare(
            "SELECT id, start_ts, end_ts, label, project, confidence FROM tasks
             WHERE start_ts >= ?1 AND start_ts < ?2 ORDER BY start_ts, id",
        )?;
        let mut rows = stmt.query([lo, hi])?;
        let mut tasks = Vec::new();
        while let Some(row) = rows.next()? {
            let (start_ms, end_ms): (i64, i64) = (row.get(1)?, row.get(2)?);
            tasks.push(TaskRow {
                id: row.get(0)?,
                start: chronicle_core::types::ms_to_ts(start_ms).to_zoned(self.tz.clone()),
                end: chronicle_core::types::ms_to_ts(end_ms).to_zoned(self.tz.clone()),
                label: row.get(3)?,
                project: row.get(4)?,
                confidence: row.get(5)?,
            });
        }
        Ok(tasks)
    }

    /// Commit an edited task row: no-op if nothing changed, else write the
    /// correction (which also updates the task) and force a reload.
    fn apply_correction(&mut self, edit: EditState) {
        let label = edit.label.trim().to_owned();
        if label.is_empty() {
            return;
        }
        let project = edit.project.trim();
        let project = (!project.is_empty()).then_some(project);
        if let Some(t) = self.tasks.iter().find(|t| t.id == edit.task_id)
            && t.label == label
            && t.project.as_deref() == project
        {
            return;
        }
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        match chronicle_core::storage::insert_correction(
            conn,
            jiff::Timestamp::now(),
            edit.task_id,
            &label,
            project,
        ) {
            Ok(()) => self.loaded_at = None,
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn load_spans(&mut self) -> anyhow::Result<Vec<SpanRow>> {
        if self.conn.is_none() {
            self.conn = Some(chronicle_core::storage::open(&self.db_path)?);
        }
        let (lo, hi) = self.day_range_ms()?;
        let conn = self.conn.as_ref().expect("connection opened above");
        let mut stmt = conn.prepare(
            "SELECT start_ts, end_ts, app, title, kind FROM spans
             WHERE start_ts >= ?1 AND start_ts < ?2 ORDER BY start_ts, id",
        )?;
        let mut rows = stmt.query([lo, hi])?;
        let mut spans = Vec::new();
        while let Some(row) = rows.next()? {
            let (start_ms, end_ms): (i64, i64) = (row.get(0)?, row.get(1)?);
            spans.push(SpanRow {
                start: chronicle_core::types::ms_to_ts(start_ms).to_zoned(self.tz.clone()),
                end: chronicle_core::types::ms_to_ts(end_ms).to_zoned(self.tz.clone()),
                app: row.get(2)?,
                title: row.get(3)?,
                kind: row.get(4)?,
            });
        }
        Ok(spans)
    }
}

impl eframe::App for TimelineApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.reload_if_stale();

        egui::Panel::top("day_picker").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("\u{25c0}").clicked() {
                    self.shift_day(-1);
                }
                if ui.button("\u{25b6}").clicked() {
                    self.shift_day(1);
                }
                if ui.button("today").clicked() {
                    self.day = Zoned::now().with_time_zone(self.tz.clone()).date();
                    self.loaded_at = None;
                }
                ui.strong(self.day.to_string());
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button("settings").clicked() {
                        self.toggle_settings();
                    }
                    if ui.button("chat").clicked() {
                        self.toggle_chat(ui.ctx());
                    }
                    if ui.button("derive now").clicked()
                        && !crate::send_ctrl(&self.sock_path, "derive")
                    {
                        self.error = Some("daemon not reachable".into());
                    }
                    ui.weak(format!(
                        "{} tasks \u{b7} {} spans",
                        self.tasks.len(),
                        self.spans.len()
                    ));
                });
            });
        });

        self.chat_panel_ui(ui);
        self.settings_window(ui.ctx());

        let mut pending: Option<EditState> = None;
        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(warning) = &self.warning {
                ui.colored_label(ui.visuals().warn_fg_color, warning);
            }
            if let Some(error) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
                return;
            }
            if self.spans.is_empty() && self.tasks.is_empty() {
                ui.weak("no data for this day");
                return;
            }
            // Rows: tasks section (header + rows), then spans section.
            let n_tasks = self.tasks.len();
            let n_rows = 1 + n_tasks + 1 + self.spans.len();
            let row_height = ui.text_style_height(&egui::TextStyle::Body) + 6.0;
            egui::ScrollArea::vertical().auto_shrink(false).show_rows(
                ui,
                row_height,
                n_rows,
                |ui, range| {
                    for i in range {
                        if i == 0 {
                            if n_tasks == 0 {
                                ui.horizontal(|ui| {
                                    ui.strong("Tasks");
                                    ui.weak("none derived yet");
                                });
                            } else {
                                ui.strong("Tasks");
                            }
                        } else if i <= n_tasks {
                            task_row(ui, &self.tasks[i - 1], &mut self.edit, &mut pending);
                        } else if i == n_tasks + 1 {
                            ui.strong("Spans");
                        } else {
                            span_row(ui, &self.spans[i - n_tasks - 2]);
                        }
                    }
                },
            );
        });
        if let Some(edit) = pending {
            self.apply_correction(edit);
        }

        // Only scheduled wake-up; no unconditional repaint.
        ui.ctx().request_repaint_after(WAKE_EVERY);
    }
}

fn task_row(
    ui: &mut egui::Ui,
    task: &TaskRow,
    edit: &mut Option<EditState>,
    pending: &mut Option<EditState>,
) {
    let time = format!(
        "{}\u{2013}{}",
        task.start.strftime("%H:%M:%S"),
        task.end.strftime("%H:%M:%S")
    );
    let dur =
        fmt_dur(task.end.timestamp().as_millisecond() - task.start.timestamp().as_millisecond());
    ui.horizontal(|ui| {
        ui.monospace(time);
        ui.weak(format!("{dur:>7}"));
        if edit.as_ref().is_some_and(|e| e.task_id == task.id) {
            let e = edit.as_mut().expect("checked above");
            ui.add(egui::TextEdit::singleline(&mut e.label).desired_width(220.0));
            ui.add(
                egui::TextEdit::singleline(&mut e.project)
                    .desired_width(110.0)
                    .hint_text("project"),
            );
            if ui.button("save").clicked() {
                *pending = edit.take();
            }
            if ui.button("cancel").clicked() {
                *edit = None;
            }
        } else {
            ui.strong(&task.label);
            if let Some(project) = &task.project {
                ui.label(project);
            }
            ui.weak(format!("{:.0}%", task.confidence * 100.0));
            if ui.small_button("edit").clicked() {
                *edit = Some(EditState {
                    task_id: task.id,
                    label: task.label.clone(),
                    project: task.project.clone().unwrap_or_default(),
                });
            }
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

fn fmt_dur(ms: i64) -> String {
    let s = ms / 1000;
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m{sec:02}s")
    } else {
        format!("{sec}s")
    }
}
