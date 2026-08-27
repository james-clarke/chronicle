//! Timeline window, run as a `chronicle ui` child process. The daemon writes
//! "toggle\n" to our stdin to raise the window; closing it exits the process.

mod chat;
mod reports;
mod settings;
mod timeline;

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use eframe::egui;
use jiff::civil;
use jiff::tz::TimeZone;
use jiff::{ToSpan, Zoned};
use rusqlite::Connection;

use chat::ChatPanel;
use settings::SettingsPanel;

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

/// One task identity with its intervals for the shown day (grouped timeline).
struct TaskGroup {
    task_id: i64,
    label: String,
    project: Option<String>,
    declared: bool,
    intervals: Vec<IntervalRow>,
    total_ms: i64,
}

struct IntervalRow {
    interval_id: i64,
    start: Zoned,
    end: Zoned,
    confidence: f64,
}

/// An open task in the "working on" list.
struct OpenRow {
    task_id: i64,
    label: String,
    project: Option<String>,
    declared: bool,
}

/// In-flight label/project edit of one task identity; committing writes a
/// `corrections` row (M5 few-shot source) and updates the task.
struct EditState {
    task_id: i64,
    label: String,
    project: String,
}

/// Deferred mutation collected during rendering, applied after the frame's
/// borrows end.
enum Action {
    Rename(EditState),
    Declare,
    Close(i64),
    Reassign { interval_id: i64, to_task: i64 },
    Merge { from_task: i64, to_task: i64 },
    Reopen(i64),
}

#[derive(PartialEq, Clone, Copy)]
enum View {
    Timeline,
    Reports,
}

struct TimelineApp {
    db_path: PathBuf,
    sock_path: PathBuf,
    conn: Option<Connection>,
    tz: TimeZone,
    day: civil::Date,
    view: View,
    /// Monday of the week the Reports view shows.
    week_anchor: civil::Date,
    report: Option<chronicle_core::report::RangeReport>,
    spans: Vec<SpanRow>,
    groups: Vec<TaskGroup>,
    open_tasks: Vec<OpenRow>,
    closed_tasks: Vec<OpenRow>,
    /// "recently closed" expander state.
    show_closed: bool,
    new_label: String,
    new_project: String,
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
            view: View::Timeline,
            week_anchor: chronicle_core::timeref::week_start(day).unwrap_or(day),
            report: None,
            spans: Vec::new(),
            groups: Vec::new(),
            open_tasks: Vec::new(),
            closed_tasks: Vec::new(),
            show_closed: false,
            new_label: String::new(),
            new_project: String::new(),
            edit: None,
            loaded_at: None,
            error: None,
            warning: None,
            chat: None,
            config_path,
            settings: None,
        }
    }

    fn shift_day(&mut self, days: i64) {
        if let Ok(day) = self.day.checked_add(days.days()) {
            self.day = day;
            self.loaded_at = None;
        }
    }

    fn shift_week(&mut self, weeks: i64) {
        if let Ok(anchor) = self.week_anchor.checked_add((weeks * 7).days()) {
            self.week_anchor = anchor;
            self.loaded_at = None;
        }
    }

    fn reload_if_stale(&mut self) {
        if self.loaded_at.is_some_and(|t| t.elapsed() < RELOAD_EVERY) {
            return;
        }
        self.loaded_at = Some(Instant::now());
        match self.load_spans().and_then(|spans| {
            let groups = self.load_groups()?;
            let open = self.load_open()?;
            let closed = self.load_closed()?;
            Ok((spans, groups, open, closed))
        }) {
            Ok((spans, groups, open, closed)) => {
                self.spans = spans;
                self.groups = groups;
                self.open_tasks = open;
                self.closed_tasks = closed;
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
        if self.view == View::Reports {
            match self.load_report() {
                Ok(r) => self.report = Some(r),
                Err(e) => self.error = Some(e.to_string()),
            }
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

    /// Week report for the Reports view: `week_anchor`'s Mon–Sun bucketed
    /// per task per day.
    fn load_report(&mut self) -> anyhow::Result<chronicle_core::report::RangeReport> {
        let days: Vec<civil::Date> = (0..7)
            .map(|i| Ok(self.week_anchor.checked_add(i.days())?))
            .collect::<anyhow::Result<_>>()?;
        let lo = days[0]
            .to_zoned(self.tz.clone())?
            .timestamp()
            .as_millisecond();
        let hi = days[6]
            .to_zoned(self.tz.clone())?
            .checked_add(1.day())?
            .timestamp()
            .as_millisecond();
        let conn = self.conn.as_ref().expect("connection opened by load_spans");
        let tasks = chronicle_core::storage::tasks_in_range(conn, lo, hi)?;
        Ok(chronicle_core::report::build(&tasks, days, &self.tz)?)
    }

    /// Day's intervals grouped under their task identity, in order of each
    /// task's first interval.
    fn load_groups(&mut self) -> anyhow::Result<Vec<TaskGroup>> {
        let (lo, hi) = self.day_range_ms()?;
        let conn = self.conn.as_ref().expect("connection opened by load_spans");
        let rows = chronicle_core::storage::tasks_in_range(conn, lo, hi)?;
        let mut groups: Vec<TaskGroup> = Vec::new();
        for t in rows {
            let start = t.start_ts.to_zoned(self.tz.clone());
            let end = t.end_ts.to_zoned(self.tz.clone());
            let dur = t.end_ts.as_millisecond() - t.start_ts.as_millisecond();
            let group = match groups.iter_mut().find(|g| g.task_id == t.id) {
                Some(g) => g,
                None => {
                    groups.push(TaskGroup {
                        task_id: t.id,
                        label: t.label.clone(),
                        project: t.project.clone(),
                        declared: t.declared,
                        intervals: Vec::new(),
                        total_ms: 0,
                    });
                    groups.last_mut().expect("just pushed")
                }
            };
            group.total_ms += dur;
            group.intervals.push(IntervalRow {
                interval_id: t.interval_id,
                start,
                end,
                confidence: t.confidence,
            });
        }
        Ok(groups)
    }

    fn load_open(&mut self) -> anyhow::Result<Vec<OpenRow>> {
        let conn = self.conn.as_ref().expect("connection opened by load_spans");
        let open = chronicle_core::storage::open_tasks(conn, 8)?;
        Ok(open
            .into_iter()
            .map(|t| OpenRow {
                task_id: t.id,
                label: t.label,
                project: t.project,
                declared: t.declared,
            })
            .collect())
    }

    fn load_closed(&mut self) -> anyhow::Result<Vec<OpenRow>> {
        let conn = self.conn.as_ref().expect("connection opened by load_spans");
        let closed = chronicle_core::storage::recently_closed(conn, 10)?;
        Ok(closed
            .into_iter()
            .map(|t| OpenRow {
                task_id: t.id,
                label: t.label,
                project: t.project,
                declared: t.declared,
            })
            .collect())
    }

    fn apply_action(&mut self, action: Action) {
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        let now = jiff::Timestamp::now();
        let result = match action {
            Action::Rename(edit) => {
                let label = edit.label.trim().to_owned();
                if label.is_empty() {
                    return;
                }
                let project = edit.project.trim();
                let project = (!project.is_empty()).then_some(project);
                // No-op if nothing changed.
                if self
                    .groups
                    .iter()
                    .find(|g| g.task_id == edit.task_id)
                    .is_some_and(|g| g.label == label && g.project.as_deref() == project)
                {
                    return;
                }
                chronicle_core::storage::insert_correction(conn, now, edit.task_id, &label, project)
            }
            Action::Declare => {
                let label = self.new_label.trim().to_owned();
                if label.is_empty() {
                    return;
                }
                let project = self.new_project.trim();
                let project = (!project.is_empty()).then_some(project);
                let result = chronicle_core::storage::insert_user_task(conn, now, &label, project)
                    .map(|_| ());
                if result.is_ok() {
                    self.new_label.clear();
                    self.new_project.clear();
                }
                result
            }
            Action::Close(task_id) => chronicle_core::storage::close_task(conn, now, task_id),
            Action::Reassign {
                interval_id,
                to_task,
            } => chronicle_core::storage::reassign_interval(conn, now, interval_id, to_task),
            Action::Merge { from_task, to_task } => {
                chronicle_core::storage::merge_task(conn, now, from_task, to_task)
            }
            Action::Reopen(task_id) => chronicle_core::storage::reopen_task(conn, task_id),
        };
        match result {
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
                for (view, label) in [(View::Timeline, "timeline"), (View::Reports, "reports")] {
                    if ui.selectable_label(self.view == view, label).clicked() && self.view != view
                    {
                        self.view = view;
                        self.loaded_at = None;
                    }
                }
                ui.separator();
                match self.view {
                    View::Timeline => {
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
                    }
                    View::Reports => {
                        if ui.button("\u{25c0}").clicked() {
                            self.shift_week(-1);
                        }
                        if ui.button("\u{25b6}").clicked() {
                            self.shift_week(1);
                        }
                        if ui.button("this week").clicked() {
                            let today = Zoned::now().with_time_zone(self.tz.clone()).date();
                            self.week_anchor =
                                chronicle_core::timeref::week_start(today).unwrap_or(today);
                            self.loaded_at = None;
                        }
                        let sunday = self.week_anchor.checked_add(6.days()).ok();
                        ui.strong(match sunday {
                            Some(sun) => format!("{} \u{2013} {sun}", self.week_anchor),
                            None => self.week_anchor.to_string(),
                        });
                    }
                }
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
                        self.groups.len(),
                        self.spans.len()
                    ));
                });
            });
        });

        self.chat_panel_ui(ui);
        self.settings_window(ui.ctx());

        if self.view == View::Reports {
            self.reports_ui(ui);
            return;
        }

        self.timeline_ui(ui);

        // Only scheduled wake-up; no unconditional repaint.
        ui.ctx().request_repaint_after(WAKE_EVERY);
    }
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
