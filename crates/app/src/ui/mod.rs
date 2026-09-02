//! Tray-popup widget window, run as a `chronicle ui` child process. The daemon
//! writes "toggle\n" to our stdin to show/hide the window; a close request
//! hides it (the process stays alive for the next toggle). The window stays
//! open on focus loss unless the settings toggle (meta `ui_autohide`) or
//! `CHRONICLE_UI_AUTOHIDE=1` opts in.

mod chat;
mod connections;
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
/// Transparent margin around the card when composited: room for the drop
/// shadow. The window grows by 2× this; the visible card stays WIDGET_W/H.
const SHADOW_PAD: f32 = 12.0;

/// True when an X11 compositor owns `_NET_WM_CM_S<screen>`. Without one
/// (bare Openbox) a transparent window degrades to opaque garbage, so the
/// rounded chrome falls back to the old square card. Wayland (connect
/// failure) also falls back — this widget targets X11.
fn compositor_active() -> bool {
    let Ok((conn, screen_num)) = x11rb::connect(None) else {
        return false;
    };
    let name = format!("_NET_WM_CM_S{screen_num}");
    let Ok(cookie) = x11rb::protocol::xproto::intern_atom(&conn, false, name.as_bytes()) else {
        return false;
    };
    let Ok(atom) = cookie.reply() else {
        return false;
    };
    x11rb::protocol::xproto::get_selection_owner(&conn, atom.atom)
        .ok()
        .and_then(|c| c.reply().ok())
        .is_some_and(|r| r.owner != x11rb::NONE)
}

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
    // Last dragged-to position ("x,y" logical px); restored at boot so the
    // window comes back where the user left it, re-clamped once the monitor
    // size is known (see `logic`).
    let saved_pos = boot_conn
        .as_ref()
        .and_then(|c| {
            chronicle_core::storage::get_meta(c, "ui_window_pos")
                .ok()
                .flatten()
        })
        .and_then(|s| {
            let (x, y) = s.split_once(',')?;
            Some(egui::pos2(x.parse().ok()?, y.parse().ok()?))
        });
    // Popover hide is opt-in: the settings toggle (meta `ui_autohide`) or
    // `CHRONICLE_UI_AUTOHIDE=1` for test runs.
    let autohide = std::env::var_os("CHRONICLE_UI_AUTOHIDE").is_some()
        || boot_conn
            .as_ref()
            .and_then(|c| {
                chronicle_core::storage::get_meta(c, "ui_autohide")
                    .ok()
                    .flatten()
            })
            .is_some_and(|v| v == "1");
    drop(boot_conn);
    let composited = compositor_active();
    let pad = if composited { SHADOW_PAD } else { 0.0 };
    let (win_w, win_h) = (WIDGET_W + 2.0 * pad, WIDGET_H + 2.0 * pad);
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Chronicle")
        .with_inner_size([win_w, win_h])
        // min == max == inner: some X11 WMs ignore resizable(false) but
        // honor WM_SIZE_HINTS, so pin all three.
        .with_min_inner_size([win_w, win_h])
        .with_max_inner_size([win_w, win_h])
        .with_resizable(false)
        .with_decorations(false)
        .with_transparent(composited)
        .with_always_on_top()
        .with_window_type(egui::X11WindowType::Utility);
    if let Some(p) = saved_pos {
        viewport = viewport.with_position(p);
    }
    let options = eframe::NativeOptions {
        viewport,
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
                BootPrefs {
                    saved_pos,
                    autohide,
                },
                composited,
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

/// Focus time no interval covers, clustered by app + title for Home's
/// "Unassigned" list.
struct UnassignedRow {
    app: String,
    title: String,
    ms: i64,
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
    /// 1-2 sentence summary from `tasks.description` (AI-written on close,
    /// user-editable).
    ai_summary: Option<String>,
    /// A description job for this task is queued or running.
    ai_pending: bool,
    /// External anchor (ticket key from `tasks.external_ref`).
    external_ref: Option<String>,
    /// Activity evidence overlapping this task's intervals today (commits,
    /// AI sessions, PR events, calls), oldest first.
    activity: Vec<ActivityRow>,
    /// MCP-fetched context bundle as (fetched_ts, content).
    task_context: Option<(i64, String)>,
    /// A fetch_context job for this task is queued or running.
    context_pending: bool,
    /// Journal tail, oldest first.
    journal: Vec<JournalRow>,
    /// Latest "where I am / what's next".
    checkpoint: Option<chronicle_core::storage::Checkpoint>,
    /// Short, undeclared, unanchored scrap of a task (Wordle-scale); folds
    /// into the timeline's collapsed background strip.
    background: bool,
}

/// One journal entry of the selected task.
struct JournalRow {
    id: i64,
    ts: i64,
    /// "Mon 09:41".
    time: String,
    entry: String,
}

/// One activity event shown as task evidence.
struct ActivityRow {
    time: Zoned,
    kind: chronicle_core::types::ActivityKind,
    /// Subject line / first prompt / PR title / calling app; the short hash
    /// when a commit had no subject.
    summary: String,
    /// Span kinds only.
    duration_ms: Option<i64>,
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
    description: String,
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
    /// Queue an interactive declare-suggestion job.
    SuggestTask,
    /// Copy the ready suggestion into the declare inputs.
    UseSuggestion,
    DismissSuggestion,
    /// Queue (or re-queue) the week-narrative job for the shown report.
    GenerateNarrative,
    /// Queue (or re-queue) yesterday's standup-draft job.
    GenerateStandup,
    /// Queue a context (re-)fetch for an anchored task.
    FetchContext(i64),
    /// Save an in-place journal/checkpoint correction from the detail pane.
    SaveWorkspaceEdit(WorkspaceEdit),
    /// Open task-scoped chat for the task.
    ChatAboutTask(i64),
}

/// In-flight inline edit of a workspace artifact in the detail pane.
/// Saving logs a correction row (kind 'journal'/'checkpoint').
enum WorkspaceEdit {
    Journal {
        entry_id: i64,
        text: String,
    },
    Checkpoint {
        task_id: i64,
        state: String,
        next_steps: String,
    },
}

/// Declare-suggestion lifecycle (home view chip).
enum SuggestionState {
    Pending(i64),
    Ready(chronicle_core::types::SuggestedTask),
    Failed(String),
}

/// Week-level insight aggregates computed alongside the report.
struct WeekInsights {
    range: (i64, i64),
    metrics: chronicle_core::insights::FocusMetrics,
    top_apps: Vec<(String, i64)>,
    /// Focus time across every app in the range (share-bar denominator).
    apps_total_ms: i64,
    delta: Option<chronicle_core::insights::Delta>,
    /// Cached narrative whose hash matches the current report.
    narrative: Option<String>,
    /// A cached narrative exists but the data moved on.
    narrative_stale: bool,
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
    /// Insight strip + narrative state for the shown report week.
    week_insights: Option<WeekInsights>,
    /// In-flight narrative job for the shown week.
    narrative_job: Option<i64>,
    /// Declare-suggestion chip state (home view).
    suggestion: Option<SuggestionState>,
    /// Suggestion description carried into the next Declare.
    pending_declare_description: Option<String>,
    spans: Vec<SpanRow>,
    groups: Vec<TaskGroup>,
    open_tasks: Vec<OpenRow>,
    closed_tasks: Vec<OpenRow>,
    /// Largest unassigned clusters of the shown day, and the day's whole
    /// unassigned total.
    unassigned: Vec<UnassignedRow>,
    unassigned_ms: i64,
    /// "recently closed" expander state.
    show_closed: bool,
    /// Raw spans section expander state (collapsed by default; debug-grade).
    show_spans: bool,
    /// Timeline's background strip is expanded (session-local).
    show_background: bool,
    /// Timeline chart mode (meta `ui_band_mode`).
    band_mode: timeline::BandMode,
    /// Meta flag `ui_show_spans_debug`: raw spans list visible on Home.
    spans_debug: bool,
    /// Case-insensitive substring filter over the day's rows.
    filter: String,
    new_label: String,
    new_project: String,
    edit: Option<EditState>,
    /// Task whose inline "merge into" picker is open (from a card/row menu).
    merge_pick: Option<i64>,
    /// In-flight journal/checkpoint inline edit (detail pane).
    ws_edit: Option<WorkspaceEdit>,
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
    /// Hide on focus loss (popover behavior). Off by default — the window
    /// stays open until closed; the settings toggle (meta `ui_autohide`) or
    /// `CHRONICLE_UI_AUTOHIDE=1` opts back in.
    autohide: bool,
    /// Corner/restore placement done (needs monitor size, so not at boot).
    positioned: bool,
    /// Last persisted window position (meta `ui_window_pos`); written when a
    /// drag settles, restored at boot.
    saved_pos: Option<egui::Pos2>,
    /// Pending "chat about task" click, consumed by the chat view.
    chat_task_request: Option<i64>,
    /// Resume card: newest checkpoint written since the previous UI open
    /// (loaded once per launch; ✕ or "open workspace" clears it).
    resume: Option<ResumeRow>,
    /// The once-per-launch resume check ran (meta `ui_last_open_ts` rotated).
    resume_checked: bool,
    /// Yesterday's standup draft (Home card; refreshed every load).
    standup: Option<StandupRow>,
    /// In-flight standup job queued from the Home card button.
    standup_job: Option<i64>,
    /// Why the last standup job failed (shown on the card; cleared on retry).
    standup_error: Option<String>,
    /// Standup card expanded (collapsible; long drafts otherwise bury Home).
    standup_open: bool,
    /// X11 compositor present at boot: transparent window, rounded card,
    /// shadow. False = square opaque fallback (bare WM / Wayland).
    composited: bool,
}

/// Home standup card data.
struct StandupRow {
    /// Civil day the draft summarizes (ISO).
    day: String,
    content: String,
}

/// Home resume card data.
struct ResumeRow {
    task_id: i64,
    /// Checkpoint write time (ms); "open workspace" jumps to its civil day.
    ts: i64,
    label: String,
    external_ref: Option<String>,
    state: String,
    next_steps: String,
}

/// Per-user prefs read from `meta` before the window exists.
struct BootPrefs {
    saved_pos: Option<egui::Pos2>,
    autohide: bool,
}

impl TimelineApp {
    fn new(
        data_dir: PathBuf,
        db_path: PathBuf,
        sock_path: PathBuf,
        config_path: PathBuf,
        visible: Arc<AtomicBool>,
        prefs: BootPrefs,
        composited: bool,
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
            week_insights: None,
            narrative_job: None,
            suggestion: None,
            pending_declare_description: None,
            spans: Vec::new(),
            groups: Vec::new(),
            open_tasks: Vec::new(),
            closed_tasks: Vec::new(),
            unassigned: Vec::new(),
            unassigned_ms: 0,
            show_closed: false,
            show_spans: false,
            show_background: false,
            band_mode: timeline::BandMode::default(),
            spans_debug: false,
            filter: String::new(),
            new_label: String::new(),
            new_project: String::new(),
            edit: None,
            merge_pick: None,
            ws_edit: None,
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
            autohide: prefs.autohide,
            positioned: false,
            saved_pos: prefs.saved_pos,
            chat_task_request: None,
            resume: None,
            resume_checked: false,
            standup: None,
            standup_job: None,
            standup_error: None,
            standup_open: true,
            composited,
        }
    }

    /// Transparent margin around the visible card (0 when not composited).
    fn shadow_pad(&self) -> f32 {
        if self.composited { SHADOW_PAD } else { 0.0 }
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
            let unassigned = self.load_unassigned()?;
            Ok((spans, groups, open, closed, unassigned))
        }) {
            Ok((spans, groups, open, closed, (unassigned, unassigned_ms))) => {
                self.spans = spans;
                self.groups = groups;
                self.open_tasks = open;
                self.closed_tasks = closed;
                self.unassigned = unassigned;
                self.unassigned_ms = unassigned_ms;
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
        self.poll_ai_jobs();
        if self.view == View::Reports {
            match self.load_report() {
                Ok(r) => {
                    self.week_insights = self.load_week_insights(&r).ok();
                    self.report = Some(r);
                }
                Err(e) => self.error = Some(e.to_string()),
            }
        }
        if let Some(conn) = self.conn.as_ref()
            && !self.resume_checked
        {
            // Once per launch: card shows checkpoints newer than the last
            // open, then the marker rotates to now.
            self.resume_checked = true;
            let last_open = chronicle_core::storage::get_meta(conn, "ui_last_open_ts")
                .ok()
                .flatten()
                .and_then(|v| v.parse::<i64>().ok())
                .unwrap_or(0);
            self.resume = chronicle_core::storage::latest_checkpoint_since(conn, last_open)
                .ok()
                .flatten()
                .map(|(task_id, label, external_ref, cp)| ResumeRow {
                    task_id,
                    ts: cp.ts,
                    label,
                    external_ref,
                    state: cp.state,
                    next_steps: cp.next_steps,
                });
            let now_ms = jiff::Timestamp::now().as_millisecond().to_string();
            let _ = chronicle_core::storage::set_meta(conn, "ui_last_open_ts", Some(&now_ms));
        }
        if let Some(conn) = self.conn.as_ref() {
            // Refresh every load (unlike the once-per-launch resume card):
            // the background job may finish between passes.
            let yesterday = jiff::Timestamp::now()
                .to_zoned(self.tz.clone())
                .date()
                .checked_sub(1.day())
                .map(|d| d.to_string());
            self.standup = yesterday.ok().and_then(|day| {
                chronicle_core::storage::get_standup_draft(conn, &day)
                    .ok()
                    .flatten()
                    .map(|(_, content)| StandupRow { day, content })
            });
        }
        if let Some(conn) = self.conn.as_ref() {
            self.warning = chronicle_core::storage::get_meta(conn, "server_error")
                .ok()
                .flatten();
            self.spans_debug = chronicle_core::storage::get_meta(conn, "ui_show_spans_debug")
                .ok()
                .flatten()
                .is_some();
            self.band_mode = chronicle_core::storage::get_meta(conn, "ui_band_mode")
                .ok()
                .flatten()
                .and_then(|s| timeline::BandMode::parse(&s))
                .unwrap_or_default();
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

    /// Advance in-flight AI job state (suggestion chip, narrative spinner).
    fn poll_ai_jobs(&mut self) {
        let Some(conn) = self.conn.as_ref() else {
            return;
        };
        if let Some(SuggestionState::Pending(job)) = self.suggestion {
            match chronicle_core::storage::ai_job_status(conn, job) {
                Ok(Some((status, result))) => match status.as_str() {
                    "done" => {
                        self.suggestion = result
                            .as_deref()
                            .and_then(|r| serde_json::from_str(r).ok())
                            .map(SuggestionState::Ready)
                            .or(Some(SuggestionState::Failed("bad suggestion".into())));
                    }
                    "failed" => {
                        self.suggestion =
                            Some(SuggestionState::Failed("no suggestion (see logs)".into()));
                    }
                    _ => {}
                },
                _ => self.suggestion = None,
            }
        }
        if let Some(job) = self.narrative_job {
            match chronicle_core::storage::ai_job_status(conn, job) {
                Ok(Some((status, _))) if status == "done" || status == "failed" => {
                    // The narrative (or its absence) is picked up by the next
                    // load_week_insights pass below.
                    self.narrative_job = None;
                }
                Ok(Some(_)) => {}
                _ => self.narrative_job = None,
            }
        }
        if let Some(job) = self.standup_job {
            match chronicle_core::storage::ai_job_status(conn, job) {
                Ok(Some((status, result))) if status == "done" || status == "failed" => {
                    // The draft itself is re-read from standup_drafts on the
                    // next load pass; a failure surfaces on the card.
                    self.standup_error = (status == "failed")
                        .then(|| result.unwrap_or_else(|| "job failed (see logs)".into()));
                    self.standup_job = None;
                }
                Ok(Some(_)) => {}
                _ => self.standup_job = None,
            }
        }
    }

    /// Metrics, top apps, prior-week delta, and cached-narrative state for
    /// the report being shown.
    fn load_week_insights(
        &self,
        r: &chronicle_core::report::RangeReport,
    ) -> anyhow::Result<WeekInsights> {
        use chronicle_core::{insights, storage};
        let conn = self.conn.as_ref().expect("connection opened by load_spans");
        let lo = r.days[0]
            .to_zoned(self.tz.clone())?
            .timestamp()
            .as_millisecond();
        let hi = r.days[r.days.len() - 1]
            .to_zoned(self.tz.clone())?
            .checked_add(1.day())?
            .timestamp()
            .as_millisecond();
        let tasks = storage::tasks_in_range(conn, lo, hi)?;
        let sessions = insights::sessions_from_tasks(&tasks, lo, hi);
        let metrics = insights::focus_metrics(&sessions, &self.tz);
        let spans = storage::spans_in_range(conn, lo, hi)?;
        let apps = insights::top_apps(&spans, lo, hi, usize::MAX);
        let apps_total_ms = apps.iter().map(|(_, ms)| ms).sum();
        let top_apps = apps.into_iter().take(3).collect();
        let delta = insights::prior_period(&r.days).and_then(|pd| {
            let plo = pd
                .first()?
                .to_zoned(self.tz.clone())
                .ok()?
                .timestamp()
                .as_millisecond();
            let ptasks = storage::tasks_in_range(conn, plo, lo).ok()?;
            let pr = chronicle_core::report::build(&ptasks, pd, &self.tz).ok()?;
            (pr.grand_total_ms > 0).then(|| insights::delta(r, &pr))
        });
        let hash = insights::report_data_hash(r);
        let cached = storage::get_narrative(conn, lo, hi)?;
        let (narrative, narrative_stale) = match cached {
            Some((h, text)) if h == hash => (Some(text), false),
            Some(_) => (None, true),
            None => (None, false),
        };
        Ok(WeekInsights {
            range: (lo, hi),
            metrics,
            top_apps,
            apps_total_ms,
            delta,
            narrative,
            narrative_stale,
        })
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
                        ai_summary: t.description.clone(),
                        ai_pending: false,
                        external_ref: t.external_ref.clone(),
                        activity: Vec::new(),
                        task_context: None,
                        context_pending: false,
                        journal: Vec::new(),
                        checkpoint: None,
                        background: false,
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
        let mut activity: std::collections::HashMap<i64, Vec<ActivityRow>> =
            std::collections::HashMap::new();
        for (task_id, a) in chronicle_core::storage::activity_in_range_by_task(conn, lo, hi)? {
            let summary = a.summary.unwrap_or_else(|| match a.kind {
                chronicle_core::types::ActivityKind::AiSession => {
                    format!("session in {}@{}", a.repo, a.branch)
                }
                _ => a
                    .ext_id
                    .map(|h| h.chars().take(12).collect())
                    .unwrap_or_default(),
            });
            activity.entry(task_id).or_default().push(ActivityRow {
                time: a.ts.to_zoned(self.tz.clone()),
                kind: a.kind,
                summary,
                duration_ms: a.end_ts.map(|e| e.as_millisecond() - a.ts.as_millisecond()),
            });
        }
        for group in &mut groups {
            if let Some(mut apps) = evidence.remove(&group.task_id) {
                apps.sort_by_key(|a| std::cmp::Reverse(a.ms));
                group.evidence = apps;
            }
            if let Some(rows) = activity.remove(&group.task_id) {
                group.activity = rows;
            }
            if group.ai_summary.is_none() {
                group.ai_pending =
                    chronicle_core::storage::pending_description_job(conn, group.task_id)
                        .unwrap_or(false);
            }
            if group.external_ref.is_some() {
                group.task_context =
                    chronicle_core::storage::task_context(conn, group.task_id).unwrap_or(None);
                group.context_pending =
                    chronicle_core::storage::pending_fetch_context_job(conn, group.task_id)
                        .unwrap_or(false);
            }
            group.journal = chronicle_core::storage::journal_tail(conn, group.task_id, 15)
                .unwrap_or_default()
                .into_iter()
                .map(|e| JournalRow {
                    id: e.id,
                    ts: e.start_ts,
                    time: chronicle_core::types::ms_to_ts(e.start_ts)
                        .to_zoned(self.tz.clone())
                        .strftime("%a %H:%M")
                        .to_string(),
                    entry: e.entry,
                })
                .collect();
            group.checkpoint =
                chronicle_core::storage::get_checkpoint(conn, group.task_id).unwrap_or(None);
        }
        // Background classification last: it needs journal/checkpoint state.
        let background_ms = i64::from(
            chronicle_core::config::Config::load(&self.config_path)
                .map(|c| c.background_minutes)
                .unwrap_or(10),
        ) * 60_000;
        if background_ms > 0 {
            for group in &mut groups {
                group.background = !group.declared
                    && group.external_ref.is_none()
                    && group.journal.is_empty()
                    && group.checkpoint.is_none()
                    && group.total_ms < background_ms;
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

    /// Focus spans of the shown day that no interval overlaps (the derive
    /// pass left them unclaimed), clustered by app + title, largest first:
    /// the top 8 clusters and the whole-day total. Same day membership as
    /// [`Self::load_spans`] (starts in the day, end clamped to it), so an
    /// overnight span counts where the timeline counts it.
    fn load_unassigned(&mut self) -> anyhow::Result<(Vec<UnassignedRow>, i64)> {
        let (lo, hi) = self.day_range_ms()?;
        let conn = self.conn.as_ref().expect("connection opened by load_spans");
        let mut stmt = conn.prepare(
            "SELECT app, title, SUM(MIN(end_ts, ?2) - start_ts) AS ms
             FROM spans s
             WHERE kind = 'focus' AND start_ts >= ?1 AND start_ts < ?2
               AND NOT EXISTS (
                   SELECT 1 FROM intervals i
                   WHERE i.start_ts < s.end_ts AND i.end_ts > s.start_ts)
             GROUP BY app, title
             ORDER BY ms DESC",
        )?;
        let rows = stmt.query_map([lo, hi], |row| {
            Ok(UnassignedRow {
                app: row.get(0)?,
                title: row.get(1)?,
                ms: row.get(2)?,
            })
        })?;
        let mut total = 0;
        let mut top = Vec::new();
        for row in rows {
            let row = row?;
            total += row.ms;
            if top.len() < 8 {
                top.push(row);
            }
        }
        Ok((top, total))
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
                let group = self.groups.iter().find(|g| g.task_id == edit.task_id);
                let identity_changed =
                    group.is_none_or(|g| g.label != label || g.project.as_deref() != project);
                // Description edits are separate from the correction few-shot
                // mechanism: a description-only save records no 'rename'.
                let desc = edit.description.trim();
                let desc_changed =
                    group.is_none_or(|g| g.ai_summary.as_deref().unwrap_or("") != desc);
                if !identity_changed && !desc_changed {
                    return;
                }
                let result = if identity_changed {
                    chronicle_core::storage::insert_correction(
                        conn,
                        now,
                        edit.task_id,
                        &label,
                        project,
                    )
                } else {
                    Ok(())
                };
                if result.is_ok() && desc_changed {
                    let _ = chronicle_core::storage::set_task_description(
                        conn,
                        edit.task_id,
                        (!desc.is_empty()).then_some(desc),
                    );
                }
                result
            }
            Action::Declare => {
                let input = self.new_label.trim().to_owned();
                if input.is_empty() {
                    return;
                }
                // The declare input also accepts a ticket key or ticket URL:
                // the key becomes the anchor (and the label, when the input
                // is nothing but the key/URL) and context fetch starts.
                let ticket = chronicle_core::config::Config::load(&self.config_path)
                    .ok()
                    .and_then(|c| regex::Regex::new(&c.ticket_regex).ok())
                    .and_then(|re| re.find(&input).map(|m| m.as_str().to_owned()));
                let label = match &ticket {
                    Some(key) if input == *key || input.starts_with("http") => key.clone(),
                    _ => input,
                };
                let project = self.new_project.trim();
                let project = (!project.is_empty()).then_some(project);
                let result = chronicle_core::storage::insert_user_task(conn, now, &label, project);
                if let Ok(task_id) = result {
                    self.new_label.clear();
                    self.new_project.clear();
                    if let Some(desc) = self.pending_declare_description.take() {
                        let _ = chronicle_core::storage::set_task_description(
                            conn,
                            task_id,
                            Some(&desc),
                        );
                    }
                    if let Some(key) = ticket
                        && chronicle_core::storage::set_task_external_ref(conn, task_id, &key)
                            .unwrap_or(false)
                    {
                        let _ = chronicle_core::storage::enqueue_ai_job(
                            conn,
                            now,
                            "fetch_context",
                            chronicle_core::storage::AI_JOB_INTERACTIVE,
                            &chronicle_core::storage::task_description_payload(task_id),
                        );
                        let _ = crate::send_ctrl(&self.sock_path, "derive");
                    }
                }
                result.map(|_| ())
            }
            Action::Close(task_id) => {
                let closed = chronicle_core::storage::close_task(conn, now, task_id);
                if closed.is_ok() {
                    // Best-effort: an AI description for the finished task.
                    // The daemon's scheduler picks it up; the 5s reload shows
                    // the result when it lands.
                    let _ = chronicle_core::storage::enqueue_ai_job(
                        conn,
                        now,
                        "task_description",
                        chronicle_core::storage::AI_JOB_INTERACTIVE,
                        &chronicle_core::storage::task_description_payload(task_id),
                    );
                }
                closed
            }
            Action::ReassignSession {
                interval_ids,
                to_task,
            } => chronicle_core::storage::reassign_intervals(conn, now, &interval_ids, to_task),
            Action::Merge { from_task, to_task } => {
                chronicle_core::storage::merge_task(conn, now, from_task, to_task)
            }
            Action::Reopen(task_id) => chronicle_core::storage::reopen_task(conn, task_id),
            Action::SuggestTask => {
                let result = chronicle_core::storage::enqueue_ai_job(
                    conn,
                    now,
                    "suggest_task",
                    10,
                    "{\"lookback_min\":15}",
                );
                match result {
                    Ok(job) => {
                        self.suggestion = Some(SuggestionState::Pending(job));
                        // Poke the daemon so the job runs now, not next tick.
                        let _ = crate::send_ctrl(&self.sock_path, "derive");
                        return;
                    }
                    Err(e) => Err(e),
                }
            }
            Action::FetchContext(task_id) => {
                let result = chronicle_core::storage::enqueue_ai_job(
                    conn,
                    now,
                    "fetch_context",
                    chronicle_core::storage::AI_JOB_INTERACTIVE,
                    &chronicle_core::storage::task_description_payload(task_id),
                );
                if result.is_ok() {
                    // Poke the daemon so the job runs now, not next tick.
                    let _ = crate::send_ctrl(&self.sock_path, "derive");
                }
                result.map(|_| ())
            }
            Action::SaveWorkspaceEdit(w) => {
                self.ws_edit = None;
                match w {
                    WorkspaceEdit::Journal { entry_id, text } => {
                        let text = text.trim().to_owned();
                        if text.is_empty() {
                            return;
                        }
                        chronicle_core::storage::update_journal_entry(conn, now, entry_id, &text)
                    }
                    WorkspaceEdit::Checkpoint {
                        task_id,
                        state,
                        next_steps,
                    } => {
                        let state = state.trim().to_owned();
                        if state.is_empty() {
                            return;
                        }
                        chronicle_core::storage::update_checkpoint(
                            conn,
                            now,
                            task_id,
                            &state,
                            next_steps.trim(),
                        )
                    }
                }
            }
            Action::ChatAboutTask(task_id) => {
                self.chat_task_request = Some(task_id);
                self.view = View::Chat;
                return;
            }
            Action::UseSuggestion => {
                if let Some(SuggestionState::Ready(s)) = self.suggestion.take() {
                    self.new_label = s.label;
                    self.new_project = s.project.unwrap_or_default();
                    self.pending_declare_description = s.description;
                }
                return;
            }
            Action::DismissSuggestion => {
                self.suggestion = None;
                return;
            }
            Action::GenerateNarrative => {
                let Some(wi) = &self.week_insights else {
                    return;
                };
                let (lo, hi) = wi.range;
                let result = chronicle_core::storage::enqueue_ai_job(
                    conn,
                    now,
                    "narrative",
                    0,
                    &format!("{{\"lo\":{lo},\"hi\":{hi}}}"),
                );
                match result {
                    Ok(job) => {
                        self.narrative_job = Some(job);
                        let _ = crate::send_ctrl(&self.sock_path, "derive");
                        return;
                    }
                    Err(e) => Err(e),
                }
            }
            Action::GenerateStandup => {
                let Ok(day) = jiff::Timestamp::now()
                    .to_zoned(self.tz.clone())
                    .date()
                    .checked_sub(1.day())
                else {
                    return;
                };
                let result = chronicle_core::storage::enqueue_ai_job(
                    conn,
                    now,
                    "standup",
                    0,
                    &chronicle_core::storage::standup_payload(&day.to_string()),
                );
                match result {
                    Ok(job) => {
                        self.standup_job = Some(job);
                        self.standup_error = None;
                        let _ = crate::send_ctrl(&self.sock_path, "derive");
                        return;
                    }
                    Err(e) => Err(e),
                }
            }
        };
        match result {
            Ok(()) => self.loaded_at = None,
            Err(e) => self.error = Some(e.to_string()),
        }
    }

    fn load_spans(&mut self) -> anyhow::Result<Vec<SpanRow>> {
        if self.conn.is_none() {
            let conn = chronicle_core::storage::open(&self.db_path)?;
            // Rows are created by a chat's first question now; drop the empties
            // older builds made on every chat-view open.
            let _ = chronicle_core::storage::delete_empty_conversations(&conn);
            self.conn = Some(conn);
            // `CHRONICLE_UI_VIEW=settings` opens the takeover once the DB is
            // up (visual-test loop; Connections reads status from it).
            if std::env::var("CHRONICLE_UI_VIEW").as_deref() == Ok("settings") {
                self.toggle_settings();
            }
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
        // Place the window once the monitor size is known: the saved position
        // when one exists and still lands on-screen, else parked bottom-right
        // above the tray (SNI hosts don't report icon coordinates; a fixed
        // corner beats the WM's arbitrary placement). No-op on Wayland.
        // Unit soup: `monitor_size`, the fixed widget size, and winit's boot
        // `with_position` are all winit-logical px, but `OuterPosition` and
        // `outer_rect` are egui points, which the zoom factor shrinks
        // (physical = points × pixels_per_point, and pixels_per_point =
        // native × zoom). Positions are computed and persisted in
        // winit-logical px and converted at the egui boundary — skipping the
        // conversion is the old "window half off-screen at zoom 1.15" bug.
        let to_points = ctx
            .input(|i| i.viewport().native_pixels_per_point)
            .unwrap_or(1.0)
            / ctx.pixels_per_point();
        let placed_before = self.positioned;
        if !self.positioned
            && let Some(monitor) = ctx.input(|i| i.viewport().monitor_size)
        {
            let margin = 12.0;
            // Uniform hover gap on right and bottom; bottom additionally
            // clears a typical bottom panel (tint2 ~24px) since
            // _NET_WORKAREA isn't exposed through egui.
            let panel = 24.0;
            // Window = card + transparent shadow pad on every side; the
            // margins below meter the gap to the *card* edge, so the pad
            // cancels one margin-width per axis.
            let pad = self.shadow_pad();
            let (win_w, win_h) = (WIDGET_W + 2.0 * pad, WIDGET_H + 2.0 * pad);
            // On-screen = at least a grabbable slice of the top bar visible.
            let usable = |p: egui::Pos2| {
                p.x > -(win_w - 60.0)
                    && p.x + 60.0 < monitor.x
                    && p.y >= 0.0
                    && p.y + 60.0 < monitor.y
            };
            let pos = self.saved_pos.filter(|&p| usable(p)).unwrap_or_else(|| {
                egui::pos2(
                    (monitor.x - win_w - margin + pad).max(0.0),
                    (monitor.y - panel - win_h - margin + pad).max(0.0),
                )
            });
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(
                (pos.to_vec2() * to_points).to_pos2(),
            ));
            self.positioned = true;
        }
        // Persist the position once a drag settles (pointer up, moved since
        // the last save). Skipped until a frame after placement ran so
        // neither the WM's initial spot nor a stale pre-park rect is saved.
        if placed_before
            && let Some(rect) = ctx.input(|i| i.viewport().outer_rect)
            && !ctx.input(|i| i.pointer.any_down())
            && let pos = (rect.min.to_vec2() / to_points).to_pos2()
            && self.saved_pos.is_none_or(|p| (p - pos).length_sq() > 4.0)
            && let Some(conn) = self.conn.as_ref()
        {
            let val = format!("{},{}", pos.x.round(), pos.y.round());
            let _ = chronicle_core::storage::set_meta(conn, "ui_window_pos", Some(&val));
            self.saved_pos = Some(pos);
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
            if std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                // Standalone run (no daemon stdin pipe): a hidden window
                // could never be re-toggled, so close really quits.
                return;
            }
            // CancelClose must be queued the same frame as the close event.
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            hide(self);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.reload_if_stale();

        // m19 window chrome: one rounded card with border + shadow, painted
        // here because panels can't round their own corners. Content lives in
        // a child ui inset by the shadow pad; square fallback when there is
        // no compositor (panel_fill is transparent either way — this card is
        // the only window background).
        let card = ui.max_rect().shrink(self.shadow_pad());
        let radius = if self.composited {
            egui::CornerRadius::same(theme::RADIUS_WINDOW)
        } else {
            egui::CornerRadius::ZERO
        };
        if self.composited {
            ui.painter().add(
                egui::epaint::Shadow {
                    offset: [0, 2],
                    blur: 18,
                    spread: 0,
                    color: egui::Color32::from_black_alpha(110),
                }
                .as_shape(card, radius),
            );
        }
        ui.painter().rect_filled(card, radius, theme::palette::BG);
        let mut content = ui.new_child(egui::UiBuilder::new().max_rect(card));
        self.window_ui(&mut content);
        // Border last so panel/card fills can't overpaint the 1px edge.
        ui.painter().rect_stroke(
            card,
            radius,
            egui::Stroke::new(1.0, theme::palette::BORDER),
            egui::StrokeKind::Inside,
        );
    }

    // The card in `ui` is the only opaque window content; everything outside
    // it (the shadow pad) must clear to transparent. Opaque BG otherwise.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.composited {
            egui::Rgba::TRANSPARENT.to_array()
        } else {
            theme::palette::BG.to_normalized_gamma_f32()
        }
    }
}

impl TimelineApp {
    /// Everything inside the window card (top bar + active view).
    fn window_ui(&mut self, ui: &mut egui::Ui) {
        // Settings takeover: replaces the whole window, top bar included.
        if self.settings.is_some() {
            self.settings_ui(ui);
            return;
        }

        let top_frame = egui::Frame::new()
            .fill(theme::palette::SURFACE)
            .corner_radius(egui::CornerRadius {
                nw: if self.composited {
                    theme::RADIUS_WINDOW
                } else {
                    0
                },
                ne: if self.composited {
                    theme::RADIUS_WINDOW
                } else {
                    0
                },
                sw: 0,
                se: 0,
            })
            .inner_margin(egui::Margin::symmetric(12, 8));
        egui::Panel::top("day_picker")
            .frame(top_frame)
            .show(ui, |ui| {
                // Pin the bar's width up front so an over-long child (filter
                // chip, date label) can't inflate the row past the window and
                // displace the right-aligned menu's hitbox.
                ui.set_max_width(theme::content_width(ui));
                // Empty bar space drags the window (decoration-less window is
                // otherwise unmovable). Ui-background sense registers BEFORE
                // the children, so buttons stay on top of the hit test; the
                // old interact-after-children overlay sat on top and ate
                // every press in the bar.
                let bar =
                    ui.scope_builder(egui::UiBuilder::new().sense(egui::Sense::drag()), |ui| {
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
                                        if theme::selectable(ui, self.view == view, label).clicked()
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
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    // Routes through the close_requested
                                    // handler: daemon child hides (reopen via
                                    // `chronicle toggle`), standalone quits.
                                    if theme::ghost_button(ui, "\u{d7}")
                                        .on_hover_text("close (reopen: chronicle toggle)")
                                        .clicked()
                                    {
                                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                                    }
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
                                },
                            );
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
                                            self.day =
                                                Zoned::now().with_time_zone(self.tz.clone()).date();
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
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                // Chart mode cycles band → lanes → hours.
                                                if theme::ghost_button(ui, self.band_mode.as_str())
                                                    .on_hover_text(
                                                        "chart: band \u{b7} lanes \u{b7} hours",
                                                    )
                                                    .clicked()
                                                {
                                                    self.band_mode = self.band_mode.next();
                                                    if let Some(conn) = self.conn.as_ref() {
                                                        let _ = chronicle_core::storage::set_meta(
                                                            conn,
                                                            "ui_band_mode",
                                                            Some(self.band_mode.as_str()),
                                                        );
                                                    }
                                                }
                                            },
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
                                            let today =
                                                Zoned::now().with_time_zone(self.tz.clone()).date();
                                            self.week_anchor =
                                                chronicle_core::timeref::week_start(today)
                                                    .unwrap_or(today);
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
                // Hand off to the WM only once egui itself calls the press a
                // drag (6pt moved or 0.8s held). The bar is the drag target
                // from the press down even over a button (buttons only sense
                // clicks), so gating on any movement let a 1px jitter mid-
                // click give the pointer to the WM: the release never came
                // back and the button "needed a double click".
                if bar.response.dragged() && ui.input(|i| i.pointer.is_decidedly_dragging()) {
                    ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
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
