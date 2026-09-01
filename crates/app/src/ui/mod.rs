//! Tray-popup widget window, run as a `chronicle ui` child process. The daemon
//! writes "toggle\n" to our stdin to show/hide the window; losing focus or a
//! close request hides it (the process stays alive for the next toggle).

mod chat;
mod home;
mod onboarding;
mod reports;
mod settings;
mod theme;
mod timeline;

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use eframe::egui;
use jiff::civil;
use jiff::tz::TimeZone;
use jiff::{ToSpan, Zoned};
use rusqlite::Connection;

use chat::ChatPanel;
use onboarding::ModelDownload;
use settings::SettingsPanel;

const RELOAD_EVERY: Duration = Duration::from_secs(5);
/// Idle wake-up cadence; the only repaint source besides user input.
const WAKE_EVERY: Duration = Duration::from_secs(10);
/// Fixed widget size (logical px); the window is not resizable.
const WIDGET_W: f32 = 400.0;
const WIDGET_H: f32 = 640.0;

pub fn run(data_dir: &Path) -> anyhow::Result<()> {
    // Bare-WM desktops (Openbox et al.) render 1:1, but winit derives an X11
    // scale factor from the monitor's physical DPI, ballooning the widget
    // (~1.65x on a 158-DPI panel). Pin 1:1 unless the user overrides.
    if std::env::var_os("WINIT_X11_SCALE_FACTOR").is_none() {
        // SAFETY: before eframe::run_native, no other threads yet.
        unsafe { std::env::set_var("WINIT_X11_SCALE_FACTOR", "1") };
    }
    let db_path = data_dir.join("chronicle.db");
    let config_path = data_dir.join("config.toml");
    // Zoom factor is remembered in `meta`; window size is fixed.
    let boot_conn = chronicle_core::storage::open(&db_path).ok();
    let zoom = boot_conn
        .as_ref()
        .and_then(|c| {
            chronicle_core::storage::get_meta(c, "ui_zoom_factor")
                .ok()
                .flatten()
        })
        .and_then(|s| s.parse::<f32>().ok())
        .filter(|z| (0.5..=2.0).contains(z));
    drop(boot_conn);
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Chronicle")
            .with_inner_size([WIDGET_W, WIDGET_H])
            // min == max == inner: some X11 WMs ignore resizable(false) but
            // honor WM_SIZE_HINTS, so pin all three.
            .with_min_inner_size([WIDGET_W, WIDGET_H])
            .with_max_inner_size([WIDGET_W, WIDGET_H])
            .with_resizable(false)
            .with_decorations(false)
            .with_always_on_top()
            .with_window_type(egui::X11WindowType::Utility),
        ..Default::default()
    };
    let sock_path = crate::socket_path(data_dir);
    let data_dir = data_dir.to_path_buf();
    eframe::run_native(
        "chronicle",
        options,
        Box::new(move |cc| {
            theme::apply(&cc.egui_ctx);
            if let Some(z) = zoom {
                cc.egui_ctx.set_zoom_factor(z);
            }
            // Shared visibility ground truth: flipped by the stdin toggle
            // thread, cleared by the app when it hides itself.
            let visible = Arc::new(AtomicBool::new(true));
            spawn_stdin_listener(cc.egui_ctx.clone(), visible.clone());
            Ok(Box::new(TimelineApp::new(
                data_dir,
                db_path,
                sock_path,
                config_path,
                visible,
            )))
        }),
    )
    .map_err(|e| anyhow::anyhow!("eframe: {e}"))
}

fn spawn_stdin_listener(ctx: egui::Context, visible: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            if line.trim() == "toggle" {
                let now_visible = !visible.fetch_xor(true, Ordering::SeqCst);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(now_visible));
                if now_visible {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
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
    /// Adjacent/near-adjacent intervals merged for display (gap ≤ [`SESSION_GAP_MS`]).
    sessions: Vec<SessionRow>,
    /// Per-app focus time inside this task's intervals, largest first.
    evidence: Vec<EvidenceApp>,
    /// Sum of interval durations clamped to the shown day.
    total_ms: i64,
    /// AI-written 1-2 sentence summary; layout placeholder until the
    /// description pipeline (phase 2) populates it from `tasks.description`.
    ai_summary: Option<String>,
}

struct IntervalRow {
    start: Zoned,
    end: Zoned,
    confidence: f64,
}

/// Display gap under which adjacent intervals merge into one session.
const SESSION_GAP_MS: i64 = 5 * 60 * 1000;

/// A run of merged intervals shown as one row/chip.
struct SessionRow {
    start: Zoned,
    end: Zoned,
    /// Member intervals, for whole-session reassign.
    interval_ids: Vec<i64>,
    /// Lowest member-interval confidence.
    confidence: f64,
}

/// One app's overlap-joined focus time within a task's intervals.
struct EvidenceApp {
    app: String,
    ms: i64,
    /// Title with the most overlap time under this app.
    top_title: String,
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
    /// Reassign a display session (all its member intervals) to another task.
    ReassignSession {
        interval_ids: Vec<i64>,
        to_task: i64,
    },
    Merge {
        from_task: i64,
        to_task: i64,
    },
    Reopen(i64),
}

#[derive(PartialEq, Clone, Copy)]
enum View {
    Home,
    Timeline,
    Reports,
    Chat,
}

struct TimelineApp {
    data_dir: PathBuf,
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
    /// Raw spans section expander state (collapsed by default; debug-grade).
    show_spans: bool,
    /// Meta flag `ui_show_spans_debug`: raw spans list visible on Home.
    spans_debug: bool,
    /// Case-insensitive substring filter over the day's rows.
    filter: String,
    new_label: String,
    new_project: String,
    edit: Option<EditState>,
    /// Task whose detail pane is open (card click toggles).
    selected_task: Option<i64>,
    loaded_at: Option<Instant>,
    error: Option<String>,
    /// Daemon status flag from `meta` (e.g. AW endpoint port conflict).
    warning: Option<String>,
    /// Some = chat panel open, warm worker child alive.
    chat: Option<ChatPanel>,
    config_path: PathBuf,
    /// Some = settings window open.
    settings: Option<SettingsPanel>,
    /// No usable model resolved (config override or default preset).
    model_missing: bool,
    /// Some = model download in flight or just finished.
    model_dl: Option<ModelDownload>,
    /// Selected PRESETS index in the onboarding card.
    preset_pick: usize,
    /// "run at login" card eligible (systemctl present, no user unit yet).
    service_card: bool,
    /// Meta flag: user dismissed the service card.
    service_dismissed: bool,
    /// Result of the last in-UI service install attempt.
    service_status: Option<Result<String, String>>,
    /// Window visibility, shared with the stdin toggle thread.
    visible: Arc<AtomicBool>,
    /// Startup instant; focus-loss hiding waits out WM map-time focus flapping.
    started: Instant,
    /// Focus was observed at least once; hide-on-focus-loss stays disarmed
    /// until then (the WM may map us unfocused, e.g. Openbox).
    was_focused: bool,
    /// `CHRONICLE_UI_NO_AUTOHIDE` disables hiding (visual-test loop).
    autohide: bool,
    /// Top-right corner placement done (needs monitor size, so not at boot).
    positioned: bool,
}

impl TimelineApp {
    fn new(
        data_dir: PathBuf,
        db_path: PathBuf,
        sock_path: PathBuf,
        config_path: PathBuf,
        visible: Arc<AtomicBool>,
    ) -> Self {
        let tz = TimeZone::system();
        let day = Zoned::now().with_time_zone(tz.clone()).date();
        Self {
            data_dir,
            db_path,
            sock_path,
            conn: None,
            tz,
            day,
            // `CHRONICLE_UI_VIEW` picks the start tab (visual-test loop).
            view: match std::env::var("CHRONICLE_UI_VIEW").as_deref() {
                Ok("timeline") => View::Timeline,
                Ok("reports") => View::Reports,
                Ok("chat") => View::Chat,
                _ => View::Home,
            },
            week_anchor: chronicle_core::timeref::week_start(day).unwrap_or(day),
            report: None,
            spans: Vec::new(),
            groups: Vec::new(),
            open_tasks: Vec::new(),
            closed_tasks: Vec::new(),
            show_closed: false,
            show_spans: false,
            spans_debug: false,
            filter: String::new(),
            new_label: String::new(),
            new_project: String::new(),
            edit: None,
            selected_task: None,
            loaded_at: None,
            error: None,
            warning: None,
            chat: None,
            config_path,
            settings: None,
            model_missing: false,
            model_dl: None,
            preset_pick: 0,
            service_card: onboarding::systemd_available() && !onboarding::service_unit_exists(),
            service_dismissed: false,
            service_status: None,
            visible,
            started: Instant::now(),
            was_focused: false,
            autohide: std::env::var_os("CHRONICLE_UI_NO_AUTOHIDE").is_none(),
            positioned: false,
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

    /// Reassignment targets: every task in sight (open + today's).
    fn merge_candidates(&self) -> Vec<(i64, String)> {
        let mut candidates: Vec<(i64, String)> = Vec::new();
        for t in &self.open_tasks {
            candidates.push((t.task_id, t.label.clone()));
        }
        for g in &self.groups {
            if !candidates.iter().any(|(id, _)| *id == g.task_id) {
                candidates.push((g.task_id, g.label.clone()));
            }
        }
        candidates
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
            self.spans_debug = chronicle_core::storage::get_meta(conn, "ui_show_spans_debug")
                .ok()
                .flatten()
                .is_some();
        }
        let model_path = chronicle_core::config::Config::load(&self.config_path)
            .ok()
            .and_then(|c| c.model_path);
        self.model_missing =
            chronicle_derive::model::resolve(model_path.as_deref(), &self.data_dir).is_none();
        if let Some(conn) = self.conn.as_ref()
            && !self.service_dismissed
        {
            self.service_dismissed =
                chronicle_core::storage::get_meta(conn, "onboard_service_dismissed")
                    .ok()
                    .flatten()
                    .is_some();
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
            // Clamp to the viewed day so totals, session rows, and card time
            // ranges all agree for intervals crossing midnight.
            let start_ms = t.start_ts.as_millisecond().max(lo);
            let end_ms = t.end_ts.as_millisecond().min(hi);
            let start = chronicle_core::types::ms_to_ts(start_ms).to_zoned(self.tz.clone());
            let end = chronicle_core::types::ms_to_ts(end_ms).to_zoned(self.tz.clone());
            let group = match groups.iter_mut().find(|g| g.task_id == t.id) {
                Some(g) => g,
                None => {
                    groups.push(TaskGroup {
                        task_id: t.id,
                        label: t.label.clone(),
                        project: t.project.clone(),
                        declared: t.declared,
                        intervals: Vec::new(),
                        sessions: Vec::new(),
                        evidence: Vec::new(),
                        total_ms: 0,
                        ai_summary: None,
                    });
                    groups.last_mut().expect("just pushed")
                }
            };
            group.total_ms += end_ms - start_ms;
            match group.sessions.last_mut() {
                Some(s) if start_ms - s.end.timestamp().as_millisecond() <= SESSION_GAP_MS => {
                    if end.timestamp() > s.end.timestamp() {
                        s.end = end.clone();
                    }
                    s.interval_ids.push(t.interval_id);
                    s.confidence = s.confidence.min(t.confidence);
                }
                _ => group.sessions.push(SessionRow {
                    start: start.clone(),
                    end: end.clone(),
                    interval_ids: vec![t.interval_id],
                    confidence: t.confidence,
                }),
            }
            group.intervals.push(IntervalRow {
                start,
                end,
                confidence: t.confidence,
            });
        }
        // Rows arrive per task in overlap-descending order, so the first title
        // seen for an app is that app's top title.
        let mut evidence: std::collections::HashMap<i64, Vec<EvidenceApp>> =
            std::collections::HashMap::new();
        for row in chronicle_core::storage::evidence_in_range(conn, lo, hi)? {
            let apps = evidence.entry(row.task_id).or_default();
            match apps.iter_mut().find(|a| a.app == row.app) {
                Some(a) => a.ms += row.ms,
                None => apps.push(EvidenceApp {
                    app: row.app,
                    ms: row.ms,
                    top_title: row.title,
                }),
            }
        }
        for group in &mut groups {
            if let Some(mut apps) = evidence.remove(&group.task_id) {
                apps.sort_by_key(|a| std::cmp::Reverse(a.ms));
                group.evidence = apps;
            }
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
            Action::ReassignSession {
                interval_ids,
                to_task,
            } => chronicle_core::storage::reassign_intervals(conn, now, &interval_ids, to_task),
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
                // Clamp to the day so header away/switching totals can't count
                // a span running past midnight.
                end: chronicle_core::types::ms_to_ts(end_ms.min(hi)).to_zoned(self.tz.clone()),
                app: row.get(2)?,
                title: row.get(3)?,
                kind: row.get(4)?,
            });
        }
        Ok(spans)
    }
}

impl eframe::App for TimelineApp {
    // Runs even while the window is hidden (unlike `ui`), so the stdin toggle
    // can bring the window back after a hide.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Park the widget in the bottom-right corner, above the tray, once
        // the monitor size is known (SNI hosts don't report icon coordinates;
        // a fixed corner beats the WM's arbitrary placement). No-op on
        // Wayland.
        if !self.positioned
            && let Some(monitor) = ctx.input(|i| i.viewport().monitor_size)
        {
            let margin = 12.0;
            // Uniform hover gap on right and bottom; bottom additionally
            // clears a typical bottom panel (tint2 ~24px) since
            // _NET_WORKAREA isn't exposed through egui.
            let panel = 24.0;
            let pos = egui::pos2(
                (monitor.x - WIDGET_W - margin).max(0.0),
                (monitor.y - panel - WIDGET_H - margin).max(0.0),
            );
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(pos));
            self.positioned = true;
        }
        let hide = |app: &Self| {
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            app.visible.store(false, Ordering::SeqCst);
        };
        // `unwrap_or(true)`: unknown focus state must not hide the window.
        let focused = ctx.input(|i| i.viewport().focused).unwrap_or(true);
        if focused {
            self.was_focused = true;
        } else if self.autohide
            && self.was_focused
            && self.visible.load(Ordering::SeqCst)
            && self.started.elapsed() > Duration::from_millis(300)
        {
            self.was_focused = false;
            hide(self);
        }
        if ctx.input(|i| i.viewport().close_requested()) {
            // CancelClose must be queued the same frame as the close event.
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            hide(self);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.reload_if_stale();

        // Settings takeover: replaces the whole window, top bar included.
        if self.settings.is_some() {
            self.settings_ui(ui);
            return;
        }

        let top_frame = egui::Frame::new()
            .fill(theme::palette::SURFACE)
            .inner_margin(egui::Margin::symmetric(12, 8));
        egui::Panel::top("day_picker")
            .frame(top_frame)
            .show(ui, |ui| {
                // Pin the bar's width up front so an over-long child (filter
                // chip, date label) can't inflate the row past the window and
                // displace the right-aligned menu's hitbox.
                ui.set_max_width(theme::content_width(ui));
                ui.horizontal(|ui| {
                    // Segmented view switcher.
                    egui::Frame::new()
                        .fill(theme::palette::INPUT_BG)
                        .corner_radius(egui::CornerRadius::same(8))
                        .inner_margin(egui::Margin::same(3))
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.x = 2.0;
                            for (view, label) in [
                                (View::Home, "home"),
                                (View::Timeline, "timeline"),
                                (View::Reports, "reports"),
                                (View::Chat, "chat"),
                            ] {
                                if ui.selectable_label(self.view == view, label).clicked()
                                    && self.view != view
                                {
                                    if self.view == View::Chat {
                                        // Leaving chat kills the warm worker
                                        // (model resident only while visible).
                                        self.chat = None;
                                    }
                                    self.view = view;
                                    self.loaded_at = None;
                                }
                            }
                        });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Widget width: actions fold into one menu.
                        ui.menu_button("\u{2026}", |ui| {
                            if matches!(self.view, View::Timeline | View::Home) {
                                ui.horizontal(|ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.filter)
                                            .desired_width(120.0)
                                            .hint_text("filter\u{2026}"),
                                    );
                                    if !self.filter.is_empty()
                                        && ui.small_button("\u{d7}").clicked()
                                    {
                                        self.filter.clear();
                                    }
                                });
                                ui.separator();
                            }
                            if ui.button("settings").clicked() {
                                self.toggle_settings();
                                ui.close();
                            }
                            if ui.button("derive now").clicked() {
                                if !crate::send_ctrl(&self.sock_path, "derive") {
                                    self.error = Some("daemon not reachable".into());
                                }
                                ui.close();
                            }
                        });
                        if !self.filter.is_empty() {
                            // Active-filter cue while the menu is closed.
                            if ui
                                .small_button(format!("\u{d7} {}", self.filter))
                                .on_hover_text("clear filter")
                                .clicked()
                            {
                                self.filter.clear();
                            }
                        }
                    });
                });
                // Per-view controls on a second row; tabs + nav don't fit
                // side by side at 400px.
                if self.view != View::Home {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        match self.view {
                            // Home is day-independent: no nav controls.
                            View::Home => {}
                            View::Chat => {
                                if ui.button("new chat").clicked() {
                                    self.chat_new();
                                }
                                self.chat_history_menu(ui);
                                if self.chat_warming() {
                                    ui.weak("loading model\u{2026}");
                                }
                            }
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
                                ui.add(
                                    egui::Label::new(
                                        egui::RichText::new(
                                            self.day.strftime("%a %-d %b %Y").to_string(),
                                        )
                                        .text_style(egui::TextStyle::Heading)
                                        .color(theme::palette::TEXT),
                                    )
                                    .truncate(),
                                );
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
                                let range = match sunday {
                                    Some(sun) => format!(
                                        "{} \u{2013} {}",
                                        self.week_anchor.strftime("%-d %b"),
                                        sun.strftime("%-d %b %Y")
                                    ),
                                    None => self.week_anchor.to_string(),
                                };
                                ui.label(
                                    egui::RichText::new(range)
                                        .text_style(egui::TextStyle::Heading)
                                        .color(theme::palette::TEXT),
                                );
                            }
                        }
                    });
                }
            });

        match self.view {
            View::Home => self.home_ui(ui),
            View::Timeline => self.timeline_ui(ui),
            View::Chat => self.chat_ui(ui),
            View::Reports => {
                self.reports_ui(ui);
                return;
            }
        }

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
