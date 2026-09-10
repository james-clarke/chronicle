//! Tray-popup widget window, run as a `chronicle ui` child process. The daemon
//! writes "toggle\n" to our stdin to show/hide the window; a close request
//! hides it (the process stays alive for the next toggle). The window stays
//! open on focus loss unless the settings toggle (meta `ui_autohide`) or
//! `CHRONICLE_UI_AUTOHIDE=1` opts in.

mod chat;
mod cloud;
mod connections;
mod home;
mod onboarding;
mod projects;
mod reports;
mod settings;
mod tasks;
mod theme;
mod timeline;
mod triage;

use std::collections::{HashMap, HashSet};
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

use chronicle_core::intent::Intent;
use chronicle_core::proposals::Proposal;
use chronicle_core::storage::FeedBlock;

use chat::ChatPanel;
use onboarding::ModelDownload;
use settings::SettingsPanel;
use timeline::PostDialog;

const RELOAD_EVERY: Duration = Duration::from_secs(5);
/// Idle wake-up cadence; the only repaint source besides user input.
const WAKE_EVERY: Duration = Duration::from_secs(10);
/// Default and minimum widget size (logical px). The window resizes from
/// the corner grip (m25); the last size is remembered in meta
/// `ui_window_size` and restored at boot.
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
    // Zoom factor, window position and size are remembered in `meta`.
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
    let text_zoom = zoom.unwrap_or(1.0);
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
    // Last resized-to card size ("w,h" logical px, shadow pad excluded),
    // never below the widget default.
    let saved_size = boot_conn
        .as_ref()
        .and_then(|c| {
            chronicle_core::storage::get_meta(c, "ui_window_size")
                .ok()
                .flatten()
        })
        .and_then(|s| {
            let (w, h) = s.split_once(',')?;
            Some(egui::vec2(w.parse().ok()?, h.parse().ok()?))
        })
        .map(|v| egui::vec2(v.x.max(WIDGET_W * text_zoom), v.y.max(WIDGET_H * text_zoom)));
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
    if let Some(d) = boot_conn
        .as_ref()
        .and_then(|c| {
            chronicle_core::storage::get_meta(c, theme::Density::META_KEY)
                .ok()
                .flatten()
        })
        .and_then(|s| theme::Density::parse(&s))
    {
        theme::set_density(d);
    }
    drop(boot_conn);
    let composited = compositor_active();
    let pad = if composited { SHADOW_PAD } else { 0.0 };
    // Window sizes are winit logical px, which egui divides by the zoom
    // factor to get the points the layout is written in: at zoom 1.1 a
    // WIDGET_W-px window is only 364 pt of card. So the floor and the
    // default card scale with the boot zoom; a size the user dragged to is
    // stored in the same px and comes back as it is.
    let (min_w, min_h) = (
        WIDGET_W * text_zoom + 2.0 * pad,
        WIDGET_H * text_zoom + 2.0 * pad,
    );
    let card = saved_size.unwrap_or(egui::vec2(WIDGET_W, WIDGET_H) * text_zoom);
    let (win_w, win_h) = (card.x + 2.0 * pad, card.y + 2.0 * pad);
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Chronicle")
        .with_inner_size([win_w, win_h])
        // The widget size is the floor (WM_SIZE_HINTS); no ceiling.
        .with_min_inner_size([min_w, min_h])
        .with_resizable(true)
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
                    saved_size,
                    autohide,
                    zoom: text_zoom,
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

/// mcp.toml as the daemon resolves it: the config override when set, else
/// the data dir's copy.
fn mcp_path(config: Option<&chronicle_core::config::Config>, data_dir: &Path) -> PathBuf {
    config.map_or_else(|| data_dir.join("mcp.toml"), |c| c.mcp_path(data_dir))
}

struct SpanRow {
    start: Zoned,
    end: Zoned,
    app: String,
    title: String,
    kind: String,
}

/// A feed block's identity across reloads: `(true, interval id)` or
/// `(false, run start)`.
type FeedKey = (bool, i64);

/// How long a newly arrived feed block fades in.
const FEED_FADE_SECS: f32 = 0.35;
/// Newest blocks the Home feed lists.
const FEED_CAP: usize = 12;

fn feed_key(block: &FeedBlock) -> FeedKey {
    match &block.claim {
        Some(c) => (true, c.interval_id),
        None => (false, block.start_ts),
    }
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
    /// Milliseconds per kind of work (m30 chunk 5), biggest first.
    by_kind: Vec<(String, i64)>,
    /// 1-2 sentence summary from `tasks.description` (AI-written on close,
    /// user-editable).
    ai_summary: Option<String>,
    /// The summary's claims JSON (m36 chunk 4), for the evidence popover.
    ai_summary_claims: Option<String>,
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
    /// Nothing has moved it for `task_stuck_days` (`storage::stuck_tasks`).
    stuck: bool,
}

/// One journal entry of the selected task.
struct JournalRow {
    id: i64,
    ts: i64,
    /// "Mon 09:41".
    time: String,
    entry: String,
    /// Claims JSON (m36 chunk 4), for the evidence popover.
    claims: Option<String>,
}

/// One activity event shown as task evidence.
struct ActivityRow {
    time: Zoned,
    kind: chronicle_core::types::ActivityKind,
    /// Empty when the kind has none.
    branch: String,
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
    /// Interval time inside the shown day (0 when none).
    today_ms: i64,
    /// End of the task's newest interval in the shown day.
    last_touched: Option<Zoned>,
    /// Ticket key (`tasks.external_ref`), else the newest branch seen in
    /// today's activity for the task.
    anchor: Option<String>,
    /// First line of the newest checkpoint's next steps.
    next_step: Option<String>,
    /// Named in today's intent: pins first, wears the "intent" chip.
    intent: bool,
    /// Nothing has moved it for `task_stuck_days` (`storage::stuck_tasks`).
    stuck: bool,
    /// Marked current in its project (m35 chunk 1): the project's sink.
    current: bool,
}

/// One Home project line (m35 chunk 3): the project, what is on screen
/// now, today's minutes, its current task and the tasks under it.
struct ProjectGroup {
    /// `None` is the unfiled group (tasks with no project) at the bottom.
    name: Option<String>,
    /// Named in `[[projects]]`; an unconfigured name is one a task still
    /// carries from before the config existed, or typed on declare.
    configured: bool,
    /// A focus span of the project ended inside the last two minutes.
    live: bool,
    /// Interval time inside today across its tasks and general task.
    today_ms: i64,
    /// Today's time on the project's general task ("not on a task").
    general_ms: i64,
    /// The current declared task (`tasks.current`).
    current: Option<i64>,
    /// Collectors that fed the project this week, in `SOURCE_ORDER`.
    sources: Vec<&'static str>,
    /// Indices into `open_tasks`, in that list's order.
    tasks: Vec<usize>,
}

/// `s` cut to `max` chars with an ellipsis (chip text stays chip-sized).
fn clip_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_owned()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('\u{2026}');
        out
    }
}

impl OpenRow {
    fn from_task(t: chronicle_core::types::OpenTask) -> Self {
        Self {
            task_id: t.id,
            label: t.label,
            project: t.project,
            declared: t.declared,
            today_ms: 0,
            last_touched: None,
            anchor: None,
            next_step: None,
            intent: false,
            stuck: false,
            current: false,
        }
    }
}

/// In-flight label/project edit of one task identity; committing writes a
/// `corrections` row (M5 few-shot source) and updates the task.
struct EditState {
    task_id: i64,
    label: String,
    project: String,
    /// `None` leaves the description alone (the Home row edit has no
    /// description field; the timeline card's has).
    description: Option<String>,
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
    /// Mark a declared task current in its project (m35 chunk 1's sink).
    SetCurrent(i64),
    /// Delete a derived task outright; its time goes back to unassigned.
    DeleteDerived(i64),
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
    /// Select the task and show it in the timeline.
    OpenTask(i64),
    /// Claim unassigned runs `(start_ms, end_ms)` for a task (triage).
    AssignRuns {
        runs: Vec<(i64, i64)>,
        to_task: i64,
    },
    /// Declare a task and claim the runs for it in one go.
    AssignRunsNew {
        runs: Vec<(i64, i64)>,
        label: String,
    },
    /// Declare a proposed task under `label` and claim its runs.
    AcceptProposal {
        id: i64,
        label: String,
    },
    /// "Not a task": park the proposal for the day.
    DismissProposal(i64),
    /// Confirm a provisional feed block (m24): its interval becomes a user row.
    KeepBlock(i64),
    /// Home's "tidy today": ask the daemon for a day-tier run (m27).
    TidyToday,
    /// Reverse today's consolidation run (its `consolidate` correction id).
    UndoTidy(i64),
    /// Put back the rows the last same-day re-score moved (m30 chunk 4).
    UndoRescore(i64),
    /// Pull a feed block out of its task ('eject' correction).
    EjectBlock {
        interval_id: i64,
        start_ts: i64,
        end_ts: i64,
        label: String,
    },
    /// Store today's intent from the morning picker (an empty one is
    /// "skip today": the picker stops asking).
    SetIntent(Intent),
    /// Run the open post dialog's action call on a thread (m26). Only ever
    /// reached from the dialog's "post" button.
    Post,
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

/// An open task already owns the ticket key the declare form typed, so the
/// form says so instead of opening a second task on the same ref (m29
/// chunk 7: two open tasks on one ref file each other's blocks).
struct DeclareConflict {
    task_id: i64,
    label: String,
    key: String,
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
    /// Its claims JSON (m36 chunk 4), for the evidence popover.
    narrative_claims: Option<String>,
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
    /// `day.strftime("%a %-d %b %Y")`, cached by `set_day`.
    day_header: String,
    view: View,
    /// Monday of the week the Reports view shows.
    week_anchor: civil::Date,
    /// The week's date range as shown in the nav row, cached by
    /// `set_week_anchor`.
    week_range: String,
    report: Option<chronicle_core::report::RangeReport>,
    /// Insight strip + narrative state for the shown report week.
    week_insights: Option<WeekInsights>,
    /// In-flight narrative job for the shown week.
    narrative_job: Option<i64>,
    /// Declare-suggestion chip state (home view).
    suggestion: Option<SuggestionState>,
    /// Suggestion description carried into the next Declare.
    pending_declare_description: Option<String>,
    /// The declare form's refusal: an open task already owns the typed key.
    declare_conflict: Option<DeclareConflict>,
    spans: Vec<SpanRow>,
    groups: Vec<TaskGroup>,
    /// The shown day's AI sessions for the timeline's agents lane (m32
    /// chunk 3), oldest first.
    agents: Vec<chronicle_core::storage::AgentLane>,
    /// The shown day's activity overlapping no task (m22): calls between
    /// tasks, PRs reviewed in a gap. Oldest first.
    unplaced: Vec<ActivityRow>,
    open_tasks: Vec<OpenRow>,
    closed_tasks: Vec<OpenRow>,
    /// Home's project lines (m35 chunk 3), rebuilt with the reload.
    project_groups: Vec<ProjectGroup>,
    /// Projects collapsed on Home (meta `ui_projects_collapsed`, one name
    /// per line; `""` is the unfiled group).
    project_collapsed: HashSet<String>,
    /// `project_collapsed` read from meta once.
    collapsed_loaded: bool,
    /// Next frame gives the declare label input focus ("+" on a project
    /// line pre-filled the project).
    declare_focus: bool,
    /// Some = task manager takeover open (Home › Working on › manage).
    tasks: Option<tasks::TaskManager>,
    /// `CHRONICLE_UI_VIEW=tasks`: open the manager once tasks are loaded.
    tasks_requested: bool,
    /// Reassignment targets (open tasks + today's groups, deduped): rebuilt
    /// with the reload, not per frame.
    merge_candidates: Vec<(i64, String)>,
    /// The shown day's feed (m24): newest blocks first, intervals of any
    /// source and unassigned runs alike; plus the day's whole unassigned
    /// total.
    feed: Vec<FeedBlock>,
    unassigned_ms: i64,
    /// What the resident worker is deriving right now (m27), or None.
    progress: Option<chronicle_core::storage::DeriveProgress>,
    /// Today's consolidation stamp when the shown day is today: None = not
    /// run, Some(0) = ran with no change, Some(id) = undoable.
    tidy: Option<Option<i64>>,
    /// The shown day's last same-day re-score (m30 chunk 4): the `rescore`
    /// correction id and the rows it moved, for the "undo" button.
    rescore: Option<(i64, usize)>,
    /// Settings › Derivation's Pipeline card (m27 chunk 7), refreshed with
    /// the reload while the panel is open.
    pipeline: Option<settings::PipelineInfo>,
    /// Open proposed tasks of the shown day (m24), newest first.
    proposals: Vec<Proposal>,
    /// When each feed block was first seen (drives the arrival fade).
    feed_seen: HashMap<FeedKey, Instant>,
    /// First feed load done: later arrivals fade in, the initial page doesn't.
    feed_primed: bool,
    /// Blocks ejected this session, by start → the task's label (the row
    /// says where it came from until something re-claims it).
    feed_ejected: HashMap<i64, String>,
    /// "recently closed" expander state.
    show_closed: bool,
    /// Raw spans section expander state (collapsed by default; debug-grade).
    show_spans: bool,
    /// Timeline's background strip is expanded (session-local).
    show_background: bool,
    /// Timeline's unplaced-activity strip is expanded (session-local).
    show_unplaced: bool,
    /// Timeline chart mode (meta `ui_band_mode`).
    band_mode: timeline::BandMode,
    /// Meta flag `ui_show_spans_debug`: raw spans list visible on Home.
    spans_debug: bool,
    /// Case-insensitive substring filter over the day's rows.
    filter: String,
    /// `filter.trim().to_lowercase()`, recomputed only when `filter`
    /// changes rather than every frame.
    filter_lc: String,
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
    /// config.toml as of the last reload; None when it failed to parse.
    config: Option<chronicle_core::config::Config>,
    /// Some = settings window open.
    settings: Option<SettingsPanel>,
    /// Some = unassigned-triage takeover open (Home → Unassigned → organize).
    triage: Option<triage::TriagePanel>,
    /// `CHRONICLE_UI_VIEW=triage`: open the takeover once tasks are loaded.
    triage_requested: bool,
    /// No usable model resolved (config override or default preset).
    model_missing: bool,
    /// Job kinds whose `models.toml` route resolves to a live cloud
    /// backend (m31 c7); refreshed with `model_missing` and again after
    /// Settings saves the file.
    cloud_kinds: Vec<String>,
    /// Any `[backends.*]` configured, even before a route names one —
    /// gates the onboarding "download model" card off.
    has_cloud_backend: bool,
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
    /// Last persisted card size (meta `ui_window_size`, logical px, shadow
    /// pad excluded); written when a resize settles.
    saved_size: Option<egui::Vec2>,
    /// Zoom factor as of the previous frame; a change (Settings' text size,
    /// Ctrl +/\u{2212}/0) resizes the window to keep the card's point size.
    zoom_seen: f32,
    /// Zoom last written to meta `ui_zoom_factor`; the write waits for the
    /// zoom to stop moving (Ctrl+scroll steps it every frame).
    zoom_saved: f32,
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
    /// Draft day whose `standup_read:<day>` meta is known written.
    standup_read_day: Option<String>,
    /// Standup card shows every task block (else the first plus "N more").
    standup_show_all: bool,
    /// X11 compositor present at boot: transparent window, rounded card,
    /// shadow. False = square opaque fallback (bare WM / Wayland).
    composited: bool,
    /// Today's intent (meta `intent:<date>`); None = the morning picker is
    /// due. An empty one ("skip today") still counts as set.
    intent: Option<Intent>,
    /// Morning picker: task ids ticked so far.
    intent_pick: HashSet<i64>,
    /// Morning picker: the free-text line.
    intent_text: String,
    /// Open tasks nothing has moved for `task_stuck_days`.
    stuck: HashSet<i64>,
    /// Action calls from mcp.toml (m26): the only writes the UI can make.
    /// Reloaded with the day so a preset added in Settings shows up.
    mcp_actions: Vec<chronicle_mcp::ActionCall>,
    /// Open post-confirm dialog (detail pane); Some = the user is looking
    /// at what would be sent.
    post: Option<PostDialog>,
}

/// Home standup card data.
struct StandupRow {
    /// Civil day the draft summarizes (ISO).
    day: String,
    content: String,
    /// Non-meta task blocks in `content`, against the tasks known when the
    /// draft loaded; the collapsed title's count (avoids reparsing per frame).
    task_count: usize,
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
    saved_size: Option<egui::Vec2>,
    autohide: bool,
    /// Zoom the window was sized for (meta `ui_zoom_factor`, 1.0 unset).
    zoom: f32,
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
        let day_header = day.strftime("%a %-d %b %Y").to_string();
        let week_anchor = chronicle_core::timeref::week_start(day).unwrap_or(day);
        let week_range = match week_anchor.checked_add(6.days()) {
            Ok(sun) => format!(
                "{} \u{2013} {}",
                week_anchor.strftime("%-d %b"),
                sun.strftime("%-d %b %Y")
            ),
            Err(_) => week_anchor.to_string(),
        };
        Self {
            data_dir,
            db_path,
            sock_path,
            conn: None,
            tz,
            day,
            day_header,
            // `CHRONICLE_UI_VIEW` picks the start tab (visual-test loop).
            view: match std::env::var("CHRONICLE_UI_VIEW").as_deref() {
                Ok("timeline") => View::Timeline,
                Ok("reports") => View::Reports,
                Ok("chat") => View::Chat,
                _ => View::Home,
            },
            week_anchor,
            week_range,
            report: None,
            week_insights: None,
            narrative_job: None,
            suggestion: None,
            pending_declare_description: None,
            declare_conflict: None,
            spans: Vec::new(),
            groups: Vec::new(),
            agents: Vec::new(),
            unplaced: Vec::new(),
            open_tasks: Vec::new(),
            closed_tasks: Vec::new(),
            project_groups: Vec::new(),
            project_collapsed: HashSet::new(),
            collapsed_loaded: false,
            declare_focus: false,
            tasks: None,
            tasks_requested: false,
            merge_candidates: Vec::new(),
            feed: Vec::new(),
            progress: None,
            tidy: None,
            rescore: None,
            pipeline: None,
            unassigned_ms: 0,
            proposals: Vec::new(),
            feed_seen: HashMap::new(),
            feed_primed: false,
            feed_ejected: HashMap::new(),
            show_closed: false,
            show_spans: false,
            show_background: false,
            show_unplaced: false,
            band_mode: timeline::BandMode::default(),
            spans_debug: false,
            filter: String::new(),
            filter_lc: String::new(),
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
            config: None,
            settings: None,
            triage: None,
            triage_requested: false,
            model_missing: false,
            cloud_kinds: Vec::new(),
            has_cloud_backend: false,
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
            saved_size: prefs.saved_size,
            zoom_seen: prefs.zoom,
            zoom_saved: prefs.zoom,
            chat_task_request: None,
            resume: None,
            resume_checked: false,
            standup: None,
            standup_job: None,
            standup_error: None,
            standup_open: true,
            standup_read_day: None,
            standup_show_all: false,
            composited,
            intent: None,
            intent_pick: HashSet::new(),
            intent_text: String::new(),
            stuck: HashSet::new(),
            mcp_actions: Vec::new(),
            post: None,
        }
    }

    /// Transparent margin around the visible card (0 when not composited).
    fn shadow_pad(&self) -> f32 {
        if self.composited { SHADOW_PAD } else { 0.0 }
    }

    /// Sets the shown day and its cached header string together — the
    /// timeline nav row reads `day_header` instead of formatting per frame.
    fn set_day(&mut self, day: civil::Date) {
        self.day = day;
        self.day_header = day.strftime("%a %-d %b %Y").to_string();
    }

    /// Sets the shown week and its cached range string together — the
    /// reports nav row reads `week_range` instead of formatting per frame.
    fn set_week_anchor(&mut self, anchor: civil::Date) {
        self.week_anchor = anchor;
        self.week_range = match anchor.checked_add(6.days()) {
            Ok(sun) => format!(
                "{} \u{2013} {}",
                anchor.strftime("%-d %b"),
                sun.strftime("%-d %b %Y")
            ),
            Err(_) => anchor.to_string(),
        };
    }

    fn shift_day(&mut self, days: i64) {
        if let Ok(day) = self.day.checked_add(days.days()) {
            self.set_day(day);
            self.loaded_at = None;
        }
    }

    fn shift_week(&mut self, weeks: i64) {
        if let Ok(anchor) = self.week_anchor.checked_add((weeks * 7).days()) {
            self.set_week_anchor(anchor);
            self.loaded_at = None;
        }
    }

    /// Reassignment targets: every task in sight (open + today's). Cheap
    /// clone of the cache rebuilt in `reload_if_stale` — call sites need an
    /// owned copy (they hold it alongside other `&mut self` field borrows).
    fn merge_candidates(&self) -> Vec<(i64, String)> {
        self.merge_candidates.clone()
    }

    /// Rebuilds the `merge_candidates` cache from the current groups/open
    /// tasks. Called from `reload_if_stale`, not per frame.
    fn rebuild_merge_candidates(&mut self) {
        let mut candidates: Vec<(i64, String)> = Vec::new();
        for t in &self.open_tasks {
            candidates.push((t.task_id, t.label.clone()));
        }
        for g in &self.groups {
            if !candidates.iter().any(|(id, _)| *id == g.task_id) {
                candidates.push((g.task_id, g.label.clone()));
            }
        }
        self.merge_candidates = candidates;
    }

    /// The posting thread's verdict (m26): the dialog's result line, and a
    /// successful post counted in meta `posts:<date>`.
    fn poll_post(&mut self) {
        let Some(dialog) = self.post.as_mut() else {
            return;
        };
        let Some(rx) = dialog.rx.as_ref() else {
            return;
        };
        let result = match rx.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            // The thread panicked or dropped the sender: end `sending` with
            // an error line rather than leaving the modal spinning forever.
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                Err("the post thread died".to_owned())
            }
        };
        dialog.rx = None;
        let ok = result.is_ok();
        dialog.result =
            Some(result.map(|()| format!("posted \u{b7} {}", Zoned::now().strftime("%H:%M"))));
        if ok && let Some(conn) = self.conn.as_ref() {
            crate::bump_day_counter(conn, "posts");
        }
    }

    fn reload_if_stale(&mut self) {
        if self.loaded_at.is_some_and(|t| t.elapsed() < RELOAD_EVERY) {
            return;
        }
        self.loaded_at = Some(Instant::now());
        self.config = chronicle_core::config::Config::load(&self.config_path).ok();
        match self.load_spans().and_then(|spans| {
            self.load_intent()?;
            let groups = self.load_groups()?;
            let agents = self.load_agents()?;
            let unplaced = self.load_unplaced()?;
            let open = self.load_open(&groups)?;
            let closed = self.load_closed()?;
            let feed = self.load_feed()?;
            let proposals = self.load_proposals()?;
            Ok((
                spans, groups, agents, unplaced, open, closed, feed, proposals,
            ))
        }) {
            Ok((
                spans,
                groups,
                agents,
                unplaced,
                open,
                closed,
                (feed, unassigned_ms),
                proposals,
            )) => {
                self.spans = spans;
                self.groups = groups;
                self.agents = agents;
                self.unplaced = unplaced;
                self.open_tasks = open;
                self.closed_tasks = closed;
                self.load_project_groups();
                self.set_feed(feed);
                self.unassigned_ms = unassigned_ms;
                self.proposals = proposals;
                self.error = None;
            }
            Err(e) => self.error = Some(e.to_string()),
        }
        self.rebuild_merge_candidates();
        if let Some(conn) = self.conn.as_ref()
            && let Ok(order) = chronicle_core::storage::project_order(conn)
        {
            theme::set_project_order(order);
        }
        self.progress = self
            .conn
            .as_ref()
            .and_then(|c| chronicle_core::storage::derive_progress(c).ok().flatten());
        if let Some(panel) = &self.settings {
            self.pipeline = self.conn.as_ref().map(|c| {
                settings::PipelineInfo::load(c, &self.data_dir, &self.sock_path, panel.config())
            });
        }
        if let (Some(conn), Some(chat)) = (self.conn.as_ref(), self.chat.as_mut()) {
            chat.refresh_history(conn);
        }
        let today = jiff::Zoned::now().with_time_zone(self.tz.clone()).date();
        self.tidy = if self.day == today {
            self.conn.as_ref().map(|c| {
                chronicle_core::storage::consolidation_of_day(c, &today.to_string())
                    .ok()
                    .flatten()
            })
        } else {
            None
        };
        self.rescore = self.conn.as_ref().and_then(|c| {
            chronicle_core::storage::rescore_of_day(c, &self.day.to_string())
                .ok()
                .flatten()
        });
        if std::mem::take(&mut self.triage_requested) {
            self.open_triage();
        }
        if std::mem::take(&mut self.tasks_requested) {
            self.open_tasks_panel();
        }
        if let (Some(conn), Some(panel)) = (self.conn.as_ref(), self.tasks.as_mut()) {
            panel.reload(conn, &self.tz);
        }
        self.mcp_actions =
            chronicle_mcp::McpConfig::load(&mcp_path(self.config.as_ref(), &self.data_dir))
                .map(|c| c.action_calls)
                .unwrap_or_default();
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
            // Several checkpoints can qualify; today's intent picks which.
            let prefer: Vec<i64> = self
                .intent
                .as_ref()
                .map(|i| i.task_ids.clone())
                .unwrap_or_default();
            self.resume =
                chronicle_core::storage::latest_checkpoint_since(conn, last_open, &prefer)
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
            let prev_day = self.standup.as_ref().map(|s| s.day.clone());
            let labels: Vec<&str> = self
                .open_tasks
                .iter()
                .chain(&self.closed_tasks)
                .map(|t| t.label.as_str())
                .collect();
            self.standup = yesterday.ok().and_then(|day| {
                chronicle_core::storage::get_standup_draft(conn, &day)
                    .ok()
                    .flatten()
                    .map(|(_, content)| {
                        let task_count = home::standup_task_count(&content, &labels);
                        StandupRow {
                            day,
                            content,
                            task_count,
                        }
                    })
            });
            // A draft first seen this pass starts collapsed when an earlier
            // launch already showed it (`standup_read:<day>`), open when new.
            if let Some(s) = &self.standup
                && prev_day.as_deref() != Some(s.day.as_str())
            {
                let read =
                    chronicle_core::storage::get_meta(conn, &format!("standup_read:{}", s.day))
                        .ok()
                        .flatten()
                        .is_some();
                self.standup_read_day = read.then(|| s.day.clone());
                self.standup_open = !read;
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
            self.band_mode = chronicle_core::storage::get_meta(conn, "ui_band_mode")
                .ok()
                .flatten()
                .and_then(|s| timeline::BandMode::parse(&s))
                .unwrap_or_default();
        }
        let model_path = self.config.as_ref().and_then(|c| c.model_path.clone());
        self.model_missing =
            chronicle_derive::model::resolve(model_path.as_deref(), &self.data_dir).is_none();
        self.reload_cloud_kinds();
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

    /// Re-reads `models.toml` and refreshes `cloud_kinds` / `has_cloud_backend`
    /// (m31 c7): called on the reload cadence and again after Settings saves
    /// the file. A parse error is treated as "no cloud backends" — the
    /// Settings panel surfaces the parse error itself.
    pub(super) fn reload_cloud_kinds(&mut self) {
        let cfg =
            chronicle_core::models_config::ModelsConfig::load(&self.data_dir).unwrap_or_default();
        self.has_cloud_backend = !cfg.backends.is_empty();
        self.cloud_kinds = cfg.cloud_kinds();
    }

    /// A job kind can run: a local model resolves, or its route points at a
    /// live cloud backend (m31 c7).
    pub(super) fn can_run(&self, kind: &str) -> bool {
        !self.model_missing || self.cloud_kinds.iter().any(|k| k == kind)
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
        let top_apps = apps.into_iter().take(5).collect();
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
        let narrative_claims = narrative
            .as_ref()
            .and_then(|_| storage::claims_for(conn, "narrative", &format!("{lo}:{hi}")).ok())
            .flatten();
        Ok(WeekInsights {
            range: (lo, hi),
            metrics,
            top_apps,
            apps_total_ms,
            delta,
            narrative,
            narrative_stale,
            narrative_claims,
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
        let mut r = chronicle_core::report::build(&tasks, days, &self.tz)?;
        r.self_ms = chronicle_core::storage::self_window_ms(conn, lo, hi)?;
        Ok(r)
    }

    /// The day's AI sessions for the agents lane.
    fn load_agents(&mut self) -> anyhow::Result<Vec<chronicle_core::storage::AgentLane>> {
        let (lo, hi) = self.day_range_ms()?;
        let conn = self.conn.as_ref().expect("connection opened by load_spans");
        Ok(chronicle_core::storage::agent_lanes(conn, lo, hi)?)
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
                        ai_summary_claims: None,
                        ai_pending: false,
                        external_ref: t.external_ref.clone(),
                        activity: Vec::new(),
                        task_context: None,
                        context_pending: false,
                        journal: Vec::new(),
                        checkpoint: None,
                        background: false,
                        stuck: self.stuck.contains(&t.id),
                        by_kind: Vec::new(),
                    });
                    groups.last_mut().expect("just pushed")
                }
            };
            let ms = t.weigh(end_ms - start_ms);
            group.total_ms += ms;
            if let Some(kind) = &t.kind {
                match group.by_kind.iter_mut().find(|(k, _)| k == kind) {
                    Some(e) => e.1 += ms,
                    None => group.by_kind.push((kind.clone(), ms)),
                }
                group.by_kind.sort_by_key(|(_, ms)| std::cmp::Reverse(*ms));
            }
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
            activity
                .entry(task_id)
                .or_default()
                .push(self.activity_row(a));
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
                    claims: e.claims,
                })
                .collect();
            group.ai_summary_claims = chronicle_core::storage::claims_for(
                conn,
                "description",
                &group.task_id.to_string(),
            )
            .unwrap_or(None);
            group.checkpoint =
                chronicle_core::storage::get_checkpoint(conn, group.task_id).unwrap_or(None);
        }
        // Background classification last: it needs journal/checkpoint state.
        let background_ms =
            i64::from(self.config.as_ref().map_or(10, |c| c.background_minutes)) * 60_000;
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

    fn activity_row(&self, a: chronicle_core::types::ActivityEvent) -> ActivityRow {
        let summary = a.summary.unwrap_or_else(|| match a.kind {
            chronicle_core::types::ActivityKind::AiSession => {
                format!("session in {}@{}", a.repo, a.branch)
            }
            _ => a
                .ext_id
                .map(|h| h.chars().take(12).collect())
                .unwrap_or_default(),
        });
        ActivityRow {
            time: a.ts.to_zoned(self.tz.clone()),
            kind: a.kind,
            branch: a.branch,
            summary,
            duration_ms: a.end_ts.map(|e| e.as_millisecond() - a.ts.as_millisecond()),
        }
    }

    fn load_unplaced(&mut self) -> anyhow::Result<Vec<ActivityRow>> {
        let (lo, hi) = self.day_range_ms()?;
        let conn = self.conn.as_ref().expect("connection opened by load_spans");
        Ok(
            chronicle_core::storage::activity_unplaced_in_range(conn, lo, hi)?
                .into_iter()
                .map(|a| self.activity_row(a))
                .collect(),
        )
    }

    /// Today's intent (the morning picker's answer) and the open tasks
    /// nothing has moved for `task_stuck_days`. Both decorate the Working-on
    /// rows and the detail pane, so they load before either.
    fn load_intent(&mut self) -> anyhow::Result<()> {
        let stuck_days = self.config.as_ref().map_or_else(
            || chronicle_core::config::Config::default().task_stuck_days,
            |c| c.task_stuck_days,
        );
        let now = jiff::Timestamp::now();
        let today = now.to_zoned(self.tz.clone()).date().to_string();
        let conn = self.conn.as_ref().expect("connection opened by load_spans");
        self.intent = chronicle_core::intent::get(conn, &today)?;
        self.stuck = chronicle_core::storage::stuck_tasks(conn, now, stuck_days)?
            .into_iter()
            .collect();
        Ok(())
    }

    fn load_open(&mut self, groups: &[TaskGroup]) -> anyhow::Result<Vec<OpenRow>> {
        let conn = self.conn.as_ref().expect("connection opened by load_spans");
        // Home groups by project now, so the derived cap is the list's
        // bound, not the page's: a collapsed project costs one line.
        let open = chronicle_core::storage::open_tasks(conn, 40)?;
        let current: HashSet<i64> = chronicle_core::storage::open_declared(conn)?
            .into_iter()
            .filter(|t| t.current)
            .map(|t| t.id)
            .collect();
        // "Today" is the calendar day, not the shown day: the timeline's day
        // nav leaves `self.day` on a past day, and the groups follow it.
        let today = jiff::Timestamp::now().to_zoned(self.tz.clone()).date();
        let today_start = today.to_zoned(self.tz.clone())?;
        let lo = today_start.timestamp().as_millisecond();
        let hi = today_start
            .checked_add(1.day())?
            .timestamp()
            .as_millisecond();
        let mut today_ms: HashMap<i64, (i64, i64)> = HashMap::new();
        for t in chronicle_core::storage::tasks_in_range(conn, lo, hi)? {
            let (s, e) = (
                t.start_ts.as_millisecond().max(lo),
                t.end_ts.as_millisecond().min(hi),
            );
            if e > s {
                let cell = today_ms.entry(t.id).or_insert((0, e));
                cell.0 += t.weigh(e - s);
                cell.1 = cell.1.max(e);
            }
        }
        let groups_today = self.day == today;
        let intent_ids: &[i64] = self.intent.as_ref().map_or(&[], |i| &i.task_ids);
        let mut rows: Vec<OpenRow> = open
            .into_iter()
            .map(|t| {
                let mut row = OpenRow::from_task(t);
                if let Some(&(ms, end)) = today_ms.get(&row.task_id) {
                    row.today_ms = ms;
                    row.last_touched = jiff::Timestamp::from_millisecond(end)
                        .ok()
                        .map(|ts| ts.to_zoned(self.tz.clone()));
                }
                // Branch fallback from today's activity (groups are the
                // shown day's; only trusted when that is today).
                if groups_today && let Some(g) = groups.iter().find(|g| g.task_id == row.task_id) {
                    row.anchor = g
                        .activity
                        .iter()
                        .rev()
                        .find(|a| !a.branch.is_empty())
                        .map(|a| clip_chars(&a.branch, 24));
                }
                if let Ok(Some(key)) = chronicle_core::storage::task_external_ref(conn, row.task_id)
                {
                    row.anchor = Some(key);
                }
                row.intent = intent_ids.contains(&row.task_id);
                row.stuck = self.stuck.contains(&row.task_id);
                row.current = current.contains(&row.task_id);
                row.next_step = chronicle_core::storage::get_checkpoint(conn, row.task_id)
                    .ok()
                    .flatten()
                    .and_then(|c| {
                        c.next_steps
                            .lines()
                            .map(|l| l.trim().trim_start_matches(['-', '*', ' ']))
                            .find(|l| !l.is_empty())
                            .map(str::to_owned)
                    });
                row
            })
            .collect();
        rows.sort_by_key(|r| std::cmp::Reverse(r.today_ms));
        // Today's intent pins first; time order holds inside each group.
        rows.sort_by_key(|r| !r.intent);
        Ok(rows)
    }

    /// The shown day's feed plus its whole unassigned total. Same day
    /// membership as [`Self::load_spans`] (starts in the day, end clamped to
    /// it), so an overnight span counts where the timeline counts it.
    fn load_feed(&mut self) -> anyhow::Result<(Vec<FeedBlock>, i64)> {
        let (lo, hi) = self.day_range_ms()?;
        let conn = self.conn.as_ref().expect("connection opened by load_spans");
        let feed = chronicle_core::storage::feed_blocks(
            conn,
            lo,
            hi,
            chronicle_core::prepass::RUN_GAP_MS,
            FEED_CAP,
        )?;
        let total: i64 = conn.query_row(
            "SELECT COALESCE(SUM(MIN(end_ts, ?2) - start_ts), 0) FROM spans s
             WHERE kind = 'focus' AND start_ts >= ?1 AND start_ts < ?2
               AND NOT EXISTS (
                   SELECT 1 FROM intervals i
                   WHERE i.start_ts < s.end_ts AND i.end_ts > s.start_ts)",
            [lo, hi],
            |r| r.get(0),
        )?;
        Ok((feed, total))
    }

    fn load_proposals(&mut self) -> anyhow::Result<Vec<Proposal>> {
        let (lo, hi) = self.day_range_ms()?;
        let conn = self.conn.as_ref().expect("connection opened by load_spans");
        Ok(chronicle_core::proposals::open_proposals(conn, lo, hi)?)
    }

    /// Swap in a freshly loaded feed: blocks not seen before start their
    /// arrival fade (except on the first load), departed ones are forgotten.
    fn set_feed(&mut self, feed: Vec<FeedBlock>) {
        let now = Instant::now();
        let arrived = if self.feed_primed {
            now
        } else {
            now.checked_sub(Duration::from_secs_f32(FEED_FADE_SECS))
                .unwrap_or(now)
        };
        let keys: Vec<FeedKey> = feed.iter().map(feed_key).collect();
        self.feed_seen.retain(|k, _| keys.contains(k));
        for k in keys {
            self.feed_seen.entry(k).or_insert(arrived);
        }
        // A claimed block is no longer "ejected"; an unclaimed one keeps its
        // note until it leaves the day.
        let starts: Vec<i64> = feed
            .iter()
            .filter(|b| b.claim.is_none())
            .map(|b| b.start_ts)
            .collect();
        self.feed_ejected.retain(|s, _| starts.contains(s));
        self.feed = feed;
        self.feed_primed = true;
    }

    /// Home's project lines from the loaded open tasks (m35 chunk 3):
    /// configured projects in config order, then names no rule knows,
    /// then the unfiled group; each with today's minutes (tasks plus the
    /// general task), the live flag, its current task and this week's
    /// sources. A configured project with nothing at all still shows,
    /// collapsed to its line.
    fn load_project_groups(&mut self) {
        let Some(conn) = self.conn.as_ref() else {
            return;
        };
        if !self.collapsed_loaded {
            self.collapsed_loaded = true;
            self.project_collapsed =
                chronicle_core::storage::get_meta(conn, "ui_projects_collapsed")
                    .ok()
                    .flatten()
                    .map(|v| v.lines().map(str::to_owned).collect())
                    .unwrap_or_default();
        }
        let matcher = self
            .config
            .as_ref()
            .map(chronicle_core::project::Matcher::from_config)
            .unwrap_or_default();
        let now = jiff::Timestamp::now();
        let now_ms = now.as_millisecond();
        let today = now.to_zoned(self.tz.clone()).date();
        let week_lo = chronicle_core::timeref::week_start(today)
            .unwrap_or(today)
            .to_zoned(self.tz.clone())
            .map(|z| z.timestamp().as_millisecond())
            .unwrap_or(now_ms);
        let (day_lo, day_hi) = today
            .to_zoned(self.tz.clone())
            .ok()
            .and_then(|s| {
                let e = s.checked_add(1.day()).ok()?;
                Some((
                    s.timestamp().as_millisecond(),
                    e.timestamp().as_millisecond(),
                ))
            })
            .unwrap_or((now_ms, now_ms));
        let live: HashSet<String> =
            chronicle_core::storage::span_projects_since(conn, now_ms - LIVE_MS)
                .unwrap_or_default()
                .into_iter()
                .collect();
        let screen: HashSet<String> = chronicle_core::storage::span_projects_since(conn, week_lo)
            .unwrap_or_default()
            .into_iter()
            .collect();
        // Sources per project: each repo's kinds land on the project the
        // repo resolves to (its folder name is a place).
        let mut sources: HashMap<String, HashSet<&'static str>> = HashMap::new();
        for name in &screen {
            sources.entry(name.clone()).or_default().insert("screen");
        }
        for (repo, kind) in chronicle_core::storage::activity_kinds_by_repo(conn, week_lo, now_ms)
            .unwrap_or_default()
        {
            let Some(word) = source_word(kind) else {
                continue;
            };
            if let Some(name) = matcher.resolve(&repo) {
                sources.entry(name.to_owned()).or_default().insert(word);
            }
        }
        // Today's minutes on each project's general task.
        let general: HashMap<i64, String> = chronicle_core::storage::general_tasks(conn)
            .unwrap_or_default()
            .into_iter()
            .collect();
        let mut general_ms: HashMap<String, i64> = HashMap::new();
        if !general.is_empty()
            && let Ok(rows) = chronicle_core::storage::tasks_in_range(conn, day_lo, day_hi)
        {
            for t in rows {
                let Some(name) = general.get(&t.id) else {
                    continue;
                };
                let (s, e) = (
                    t.start_ts.as_millisecond().max(day_lo),
                    t.end_ts.as_millisecond().min(day_hi),
                );
                if e > s {
                    *general_ms.entry(name.clone()).or_default() += t.weigh(e - s);
                }
            }
        }
        let current: HashMap<String, i64> = chronicle_core::storage::open_declared(conn)
            .unwrap_or_default()
            .into_iter()
            .filter(|t| t.current)
            .filter_map(|t| t.project.map(|p| (p, t.id)))
            .collect();
        let mut order: Vec<(Option<String>, bool)> = matcher
            .projects
            .iter()
            .map(|p| (Some(p.name.clone()), true))
            .collect();
        for t in &self.open_tasks {
            let key = t.project.clone().filter(|p| !p.trim().is_empty());
            if !order.iter().any(|(n, _)| *n == key) {
                order.push((key, false));
            }
        }
        // The unfiled group closes the list even when it is empty today.
        if !order.iter().any(|(n, _)| n.is_none()) {
            order.push((None, false));
        }
        order.sort_by_key(|(n, _)| n.is_none());
        self.project_groups = order
            .into_iter()
            .map(|(name, configured)| {
                let tasks: Vec<usize> = self
                    .open_tasks
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| t.project.clone().filter(|p| !p.trim().is_empty()) == name)
                    .map(|(i, _)| i)
                    .collect();
                let g_ms = name
                    .as_ref()
                    .and_then(|n| general_ms.get(n))
                    .copied()
                    .unwrap_or(0);
                let today_ms = tasks
                    .iter()
                    .map(|&i| self.open_tasks[i].today_ms)
                    .sum::<i64>()
                    + g_ms;
                let mut srcs: Vec<&'static str> = name
                    .as_ref()
                    .and_then(|n| sources.get(n))
                    .map(|s| s.iter().copied().collect())
                    .unwrap_or_default();
                srcs.sort_by_key(|w| SOURCE_ORDER.iter().position(|o| o == w));
                ProjectGroup {
                    live: name.as_ref().is_some_and(|n| live.contains(n)),
                    current: name.as_ref().and_then(|n| current.get(n)).copied(),
                    general_ms: g_ms,
                    today_ms,
                    sources: srcs,
                    tasks,
                    name,
                    configured,
                }
            })
            .collect();
    }

    /// Flip a project line's collapsed state and persist the set.
    fn toggle_project_collapsed(&mut self, key: &str) {
        if !self.project_collapsed.remove(key) {
            self.project_collapsed.insert(key.to_owned());
        }
        if let Some(conn) = self.conn.as_ref() {
            let mut names: Vec<&str> = self.project_collapsed.iter().map(String::as_str).collect();
            names.sort_unstable();
            let value = names.join("\n");
            let _ = chronicle_core::storage::set_meta(
                conn,
                "ui_projects_collapsed",
                (!value.is_empty()).then_some(value.as_str()),
            );
        }
    }

    /// Open the task manager takeover (m35 chunk 3).
    pub(super) fn open_tasks_panel(&mut self) {
        let Some(conn) = self.conn.as_ref() else {
            return;
        };
        let mut panel = tasks::TaskManager::default();
        panel.reload(conn, &self.tz);
        self.tasks = Some(panel);
    }

    fn load_closed(&mut self) -> anyhow::Result<Vec<OpenRow>> {
        let conn = self.conn.as_ref().expect("connection opened by load_spans");
        let closed = chronicle_core::storage::recently_closed(conn, 10)?;
        Ok(closed.into_iter().map(OpenRow::from_task).collect())
    }

    fn apply_action(&mut self, action: Action) {
        let Some(conn) = self.conn.as_mut() else {
            return;
        };
        let now = jiff::Timestamp::now();
        // Some = a triage assign ran; ms claimed feeds the panel's status.
        let mut claimed: Option<i64> = None;
        let teaches = matches!(
            action,
            Action::AssignRuns { .. }
                | Action::KeepBlock(_)
                | Action::EjectBlock { .. }
                | Action::ReassignSession { .. }
                | Action::Merge { .. }
        );
        let result = match action {
            Action::AssignRuns { runs, to_task } => {
                assign_runs(conn, now, &runs, to_task, &mut claimed)
            }
            Action::AcceptProposal { id, label } => {
                chronicle_core::proposals::accept(conn, now, id, &label).map(|_| ())
            }
            Action::DismissProposal(id) => chronicle_core::proposals::dismiss(conn, id),
            Action::TidyToday => {
                let _ = crate::send_ctrl(&self.sock_path, "consolidate");
                Ok(())
            }
            Action::UndoTidy(id) => chronicle_core::storage::consolidate_undo(conn, id),
            Action::UndoRescore(id) => {
                chronicle_core::storage::rescore_undo(conn, id, &self.day.to_string()).map(|_| ())
            }
            Action::KeepBlock(interval_id) => {
                chronicle_core::storage::keep_interval(conn, now, interval_id)
            }
            Action::EjectBlock {
                interval_id,
                start_ts,
                end_ts,
                label,
            } => {
                let ejected = chronicle_core::storage::split_interval(
                    conn,
                    now,
                    interval_id,
                    start_ts,
                    end_ts,
                );
                if ejected.is_ok() {
                    self.feed_ejected.insert(start_ts, label);
                }
                ejected.map(|_| ())
            }
            Action::AssignRunsNew { runs, label } => {
                match chronicle_core::storage::insert_user_task(conn, now, &label, None) {
                    Ok(task_id) => assign_runs(conn, now, &runs, task_id, &mut claimed),
                    Err(e) => Err(e),
                }
            }
            Action::Rename(edit) => {
                let label = edit.label.trim().to_owned();
                if label.is_empty() {
                    return;
                }
                let project = edit.project.trim();
                let project = (!project.is_empty()).then_some(project);
                let group = self.groups.iter().find(|g| g.task_id == edit.task_id);
                // A Home row's task may have no group today; its row still
                // knows the identity, so an unchanged save writes nothing.
                let current = group
                    .map(|g| (g.label.as_str(), g.project.as_deref()))
                    .or_else(|| {
                        self.open_tasks
                            .iter()
                            .find(|t| t.task_id == edit.task_id)
                            .map(|t| (t.label.as_str(), t.project.as_deref()))
                    });
                let identity_changed = current.is_none_or(|(l, p)| l != label || p != project);
                // Description edits are separate from the correction few-shot
                // mechanism: a description-only save records no 'rename'.
                let desc = edit.description.as_deref().map(str::trim);
                let desc_changed = desc.is_some_and(|d| {
                    group.is_none_or(|g| g.ai_summary.as_deref().unwrap_or("") != d)
                });
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
                if result.is_ok()
                    && desc_changed
                    && let Some(desc) = desc
                {
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
                let ticket = self
                    .config
                    .as_ref()
                    .and_then(|c| regex::Regex::new(&c.ticket_regex).ok())
                    .and_then(|re| re.find(&input).map(|m| m.as_str().to_owned()));
                let label = match &ticket {
                    Some(key) if input == *key || input.starts_with("http") => key.clone(),
                    _ => input,
                };
                // Two open tasks on one ticket key file each other's blocks
                // (m29 chunk 7), so the form points at the owner instead of
                // opening a second one; the typed text stays for a retry.
                self.declare_conflict = None;
                if let Some(key) = &ticket
                    && let Ok(owners) = chronicle_core::storage::open_tasks_by_ref(conn, key)
                    && let Some(owner) = owners.first()
                {
                    self.declare_conflict = Some(DeclareConflict {
                        task_id: owner.id,
                        label: owner.label.clone(),
                        key: key.clone(),
                    });
                    return;
                }
                // The typed project resolves to a configured one when it
                // names one or its repo folder (m35 chunk 1); anything else
                // is kept as typed and counts as unfiled until mapped.
                let typed = self.new_project.trim().to_owned();
                let project = self
                    .config
                    .as_ref()
                    .map(chronicle_core::project::Matcher::from_config)
                    .and_then(|m| m.resolve(&typed).map(str::to_owned))
                    .unwrap_or(typed);
                let project = (!project.is_empty()).then_some(project.as_str());
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
                    if let Some(cfg) = self.config.as_ref()
                        && let Err(e) =
                            chronicle_core::segmenter::seed_task_evidence(conn, cfg, now, task_id)
                    {
                        tracing::warn!("seeding declared task {task_id}: {e}");
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
            Action::SetCurrent(task_id) => {
                chronicle_core::storage::set_current_task(conn, task_id).map(|_| ())
            }
            Action::DeleteDerived(task_id) => {
                chronicle_core::storage::delete_derived_task(conn, task_id).map(|_| ())
            }
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
            Action::Post => {
                let path = mcp_path(self.config.as_ref(), &self.data_dir);
                let Some(dialog) = self.post.as_mut() else {
                    return;
                };
                let action = dialog.action.clone();
                let (tx, rx) = std::sync::mpsc::channel();
                let (key, body) = (dialog.key.clone(), dialog.body.clone());
                let spawned =
                    std::thread::Builder::new()
                        .name("mcp-action".into())
                        .spawn(move || {
                            let result = chronicle_mcp::run_action(&path, &action, &key, &body)
                                .map(|_| ())
                                .map_err(|e| format!("{e:#}"));
                            let _ = tx.send(result);
                        });
                match spawned {
                    Ok(_) => dialog.rx = Some(rx),
                    Err(e) => dialog.result = Some(Err(e.to_string())),
                }
                return;
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
            Action::OpenTask(task_id) => {
                self.declare_conflict = None;
                self.selected_task = Some(task_id);
                self.view = View::Timeline;
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
            Action::SetIntent(intent) => {
                let day = jiff::Timestamp::now()
                    .to_zoned(self.tz.clone())
                    .date()
                    .to_string();
                let result = chronicle_core::intent::set(conn, &day, &intent);
                if result.is_ok() {
                    self.intent = Some(intent);
                    self.intent_pick.clear();
                    self.intent_text.clear();
                }
                result
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
        // In segmenter mode a correction teaches the day: re-score it now so
        // every stretch the correction speaks to moves with it.
        if result.is_ok()
            && teaches
            && let Some(config) = self.config.as_ref()
            && config.derive_mode == "segmenter"
            && let Some(conn) = self.conn.as_mut()
        {
            let distractions =
                chronicle_core::evidence::compile_patterns(&config.distraction_patterns);
            let day = self.day.to_string();
            let lo = self
                .day
                .to_zoned(self.tz.clone())
                .map(|z| z.timestamp().as_millisecond())
                .unwrap_or(0);
            let hi = lo + 86_400_000;
            match chronicle_core::segmenter::rescore_day(
                conn,
                config,
                now,
                &distractions,
                &day,
                lo,
                hi,
            ) {
                Ok(moved) => {
                    if moved.is_some() {
                        self.rescore = moved;
                    }
                }
                Err(e) => tracing::warn!("same-day re-score failed: {e}"),
            }
        }
        if let Some(panel) = &mut self.triage
            && let Some(ms) = claimed
        {
            panel.dirty = true;
            panel.set_status(match &result {
                Ok(()) if ms > 0 => Ok(format!("assigned {}", fmt_dur(ms))),
                Ok(()) => Ok("nothing claimed \u{2014} that stretch is not batched yet".into()),
                Err(e) => Err(e.to_string()),
            });
        }
        if let Some(panel) = &mut self.tasks {
            panel.dirty = true;
            if let Err(e) = &result {
                panel.status_line = Some(Err(e.to_string()));
            }
        }
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
            match std::env::var("CHRONICLE_UI_VIEW").as_deref() {
                Ok("settings") => self.toggle_settings(),
                // Needs the task lists (pick candidates): opens after this load.
                Ok("triage") => self.triage_requested = true,
                Ok("tasks") => self.tasks_requested = true,
                _ => {}
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
        // A zoom change (Settings' text size, Ctrl +/\u{2212}/0) leaves the
        // window at its pixel size, so the card silently loses or gains
        // points: give it back the point size it had. The size commands are
        // points, and the shadow pad is fixed px like the boot floor, so it
        // goes in divided by the zoom.
        let zoom = ctx.zoom_factor();
        if (zoom - self.zoom_seen).abs() > 0.001 {
            let pad = 2.0 * self.shadow_pad() / zoom;
            ctx.send_viewport_cmd(egui::ViewportCommand::MinInnerSize(egui::vec2(
                WIDGET_W + pad,
                WIDGET_H + pad,
            )));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(
                ctx.viewport_rect().size() * (zoom / self.zoom_seen),
            ));
            self.zoom_seen = zoom;
        } else if (zoom - self.zoom_saved).abs() > 0.001
            && let Some(conn) = self.conn.as_ref()
        {
            // Settled (a Ctrl+scroll ramp steps the zoom every frame): one
            // write, not one per step.
            let _ = chronicle_core::storage::set_meta(
                conn,
                "ui_zoom_factor",
                Some(&format!("{zoom:.2}")),
            );
            self.zoom_saved = zoom;
        }
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
            let card = self
                .saved_size
                .unwrap_or(egui::vec2(WIDGET_W, WIDGET_H) * ctx.zoom_factor());
            let (win_w, win_h) = (card.x + 2.0 * pad, card.y + 2.0 * pad);
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
        // Likewise the card size once a resize settles (same unit dance).
        if placed_before
            && let Some(rect) = ctx.input(|i| i.viewport().outer_rect)
            && !ctx.input(|i| i.pointer.any_down())
            && let size = rect.size() / to_points - egui::Vec2::splat(2.0 * self.shadow_pad())
            && size.x >= WIDGET_W * zoom - 1.0
            && size.y >= WIDGET_H * zoom - 1.0
            && self.saved_size.is_none_or(|s| (s - size).length_sq() > 4.0)
            && let Some(conn) = self.conn.as_ref()
        {
            let val = format!("{},{}", size.x.round(), size.y.round());
            let _ = chronicle_core::storage::set_meta(conn, "ui_window_size", Some(&val));
            self.saved_size = Some(size);
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
        self.poll_post();

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
        // Resize grip in the card's bottom-right corner: the decoration-less
        // window has no frame to grab, so a drag here hands the WM a
        // south-east resize (the same route the top bar uses to move).
        // Registered BEFORE the view so any control that reaches into the
        // corner (chat's send, the detail pane's close task) wins the hit
        // test; the grip only owns the bare corner. Painted after.
        let grip = egui::Rect::from_min_max(card.max - egui::Vec2::splat(18.0), card.max);
        let grip_resp = ui.interact(grip, ui.id().with("resize_grip"), egui::Sense::drag());
        let mut content = ui.new_child(egui::UiBuilder::new().max_rect(card));
        self.window_ui(&mut content);
        if grip_resp.drag_started() {
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::BeginResize(
                    egui::ResizeDirection::SouthEast,
                ));
        }
        let grip_color = if grip_resp.hovered() || grip_resp.dragged() {
            theme::palette::TEXT_DIM
        } else {
            theme::palette::BORDER
        };
        let grip_resp = grip_resp.on_hover_cursor(egui::CursorIcon::ResizeSouthEast);
        let _ = grip_resp;
        let painter = ui.painter();
        for (i, inset) in [5.0, 9.0].iter().enumerate() {
            let a = egui::pos2(card.max.x - 4.0 - inset, card.max.y - 4.0);
            let b = egui::pos2(card.max.x - 4.0, card.max.y - 4.0 - inset);
            painter.line_segment(
                [a, b],
                egui::Stroke::new(if i == 0 { 1.5 } else { 1.0 }, grip_color),
            );
        }
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
        if self.triage.is_some() {
            self.triage_ui(ui);
            return;
        }
        if self.tasks.is_some() {
            self.tasks_ui(ui);
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
                                            self.declare_conflict = None;
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
                                                if ui
                                                    .add(
                                                        egui::TextEdit::singleline(
                                                            &mut self.filter,
                                                        )
                                                        .desired_width(120.0)
                                                        .hint_text("filter\u{2026}"),
                                                    )
                                                    .changed()
                                                {
                                                    self.filter_lc =
                                                        self.filter.trim().to_lowercase();
                                                }
                                                if !self.filter.is_empty()
                                                    && ui.small_button("\u{d7}").clicked()
                                                {
                                                    self.filter.clear();
                                                    self.filter_lc.clear();
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
                                            self.filter_lc.clear();
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
                                            let today =
                                                Zoned::now().with_time_zone(self.tz.clone()).date();
                                            self.set_day(today);
                                            self.loaded_at = None;
                                        }
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&self.day_header)
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
                                            let anchor = chronicle_core::timeref::week_start(today)
                                                .unwrap_or(today);
                                            self.set_week_anchor(anchor);
                                            self.loaded_at = None;
                                        }
                                        ui.label(
                                            egui::RichText::new(&self.week_range)
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

/// A project whose focus span ended inside this horizon is "on screen now".
const LIVE_MS: i64 = 2 * 60_000;

/// Display order of the Home sources row.
const SOURCE_ORDER: [&str; 10] = [
    "screen", "git", "ai", "prs", "editor", "shell", "browser", "calls", "calendar", "notes",
];

/// The sources-row word for a collector's activity kind; `None` for kinds
/// that are anchors only.
fn source_word(kind: chronicle_core::types::ActivityKind) -> Option<&'static str> {
    use chronicle_core::types::ActivityKind as K;
    Some(match kind {
        K::Checkout | K::Commit => "git",
        K::AiSession => "ai",
        K::PrAuthored | K::PrReviewed => "prs",
        K::Edit => "editor",
        K::Shell => "shell",
        K::Call => "calls",
        K::Meeting => "calendar",
        K::Note => "notes",
        K::Browse => "browser",
        K::Cwd => return None,
    })
}

/// The one duration rule for every UI surface: `2h41m` from an hour up,
/// `30m` from a minute up (seconds dropped — they were noise on every row),
/// `41s` under a minute.
fn fmt_dur(ms: i64) -> String {
    let s = ms / 1000;
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 || sec == 0 {
        format!("{m}m")
    } else {
        format!("{sec}s")
    }
}

/// Claim each run for the task, summing the ms claimed into `claimed`.
fn assign_runs(
    conn: &mut Connection,
    now: jiff::Timestamp,
    runs: &[(i64, i64)],
    to_task: i64,
    claimed: &mut Option<i64>,
) -> Result<(), chronicle_core::storage::StorageError> {
    let mut total = 0;
    for &(s, e) in runs {
        total += chronicle_core::storage::assign_unassigned(conn, now, s, e, to_task)?;
    }
    *claimed = Some(total);
    Ok(())
}
