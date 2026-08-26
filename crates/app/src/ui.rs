//! Timeline window, run as a `chronicle ui` child process. The daemon writes
//! "toggle\n" to our stdin to raise the window; closing it exits the process.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use jiff::civil;
use jiff::tz::TimeZone;
use jiff::{ToSpan, Zoned};
use rusqlite::Connection;

const RELOAD_EVERY: Duration = Duration::from_secs(5);
/// Idle wake-up cadence; the only repaint source besides user input.
const WAKE_EVERY: Duration = Duration::from_secs(30);

pub fn run(data_dir: &Path) -> anyhow::Result<()> {
    let db_path = data_dir.join("chronicle.db");
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
            Ok(Box::new(TimelineApp::new(db_path, sock_path)))
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
}

impl TimelineApp {
    fn new(db_path: PathBuf, sock_path: PathBuf) -> Self {
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

        let mut pending: Option<EditState> = None;
        egui::CentralPanel::default().show(ui, |ui| {
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
            if ui.button("\u{2713}").clicked() {
                *pending = edit.take();
            }
            if ui.button("\u{2715}").clicked() {
                *edit = None;
            }
        } else {
            ui.strong(&task.label);
            if let Some(project) = &task.project {
                ui.label(project);
            }
            ui.weak(format!("{:.0}%", task.confidence * 100.0));
            if ui.small_button("\u{270e}").clicked() {
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
