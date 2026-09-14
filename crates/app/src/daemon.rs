use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::capture::{Filters, GAP_MARKER_SECS, spawn_capture};
use crate::derive::deriveproto;
use crate::status::init_logging;
use anyhow::{Context, bail};
use chronicle_core::config::Config;
use chronicle_core::types::CaptureEvent;
use crossbeam_channel::Sender;
use jiff::{Timestamp, tz::TimeZone};

/// Per-day counter in `meta` (`fetches:2026-09-03`, `posts:2026-09-03`):
/// what Settings › Storage reports as having left this machine today.
/// Best-effort — a meta write must never block the thing it counts.
pub(crate) fn bump_day_counter(conn: &rusqlite::Connection, prefix: &str) {
    bump_day_counter_by(conn, prefix, 1);
}

/// [`bump_day_counter`] for a batch: one mcp gather runs several calls.
pub(crate) fn bump_day_counter_by(conn: &rusqlite::Connection, prefix: &str, by: usize) {
    use chronicle_core::storage;
    if by == 0 {
        return;
    }
    let key = day_counter_key(prefix);
    let n = storage::get_meta(conn, &key)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let _ = storage::set_meta(conn, &key, Some(&(n + by as i64).to_string()));
}

/// The key [`bump_day_counter`] writes and the settings panel reads.
pub(crate) fn day_counter_key(prefix: &str) -> String {
    format!("{prefix}:{}", jiff::Zoned::now().date())
}

pub(crate) fn socket_path(data_dir: &Path) -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.to_path_buf())
        .join("chronicle.sock")
}

pub(crate) fn send_ctrl(sock: &Path, msg: &str) -> bool {
    use std::io::Write;
    match std::os::unix::net::UnixStream::connect(sock) {
        Ok(mut stream) => stream.write_all(format!("{msg}\n").as_bytes()).is_ok(),
        Err(_) => false,
    }
}

pub(crate) enum CtrlMsg {
    Toggle,
    DeriveNow,
    Consolidate,
    /// A surface wrote config.toml: pick up the projects and rules now.
    Reload,
    /// Capture went blind (focus provider exit, screen lock): the ledger row
    /// ends at `ts` with `reason` (m32 chunk 0).
    CaptureLost {
        reason: &'static str,
        ts: Timestamp,
    },
    /// Capture resumed (unlock): a new ledger row opens at `ts`.
    CaptureBack(Timestamp),
    /// One-shot reply channel; the listener writes the JSON back to the client.
    Status(Sender<String>),
    /// Only in-process senders (signal thread, tray "Quit") — not part of the
    /// socket protocol, so a stray client can't stop the daemon.
    Shutdown,
}

#[derive(Debug, PartialEq)]
pub(crate) enum CtrlCmd {
    Toggle,
    DeriveNow,
    /// Home's "tidy today": run the day tier now.
    Consolidate,
    Status,
    /// Re-read config.toml for the parts that apply without a restart:
    /// the projects and their rules (m44 chunk 3).
    Reload,
}

pub(crate) fn parse_ctrl_cmd(line: &str) -> Option<CtrlCmd> {
    match line.trim() {
        "toggle" => Some(CtrlCmd::Toggle),
        "derive" => Some(CtrlCmd::DeriveNow),
        "consolidate" => Some(CtrlCmd::Consolidate),
        "status" => Some(CtrlCmd::Status),
        "reload" => Some(CtrlCmd::Reload),
        _ => None,
    }
}

pub(crate) fn spawn_ctrl_listener(
    listener: std::os::unix::net::UnixListener,
    tx: Sender<CtrlMsg>,
) -> anyhow::Result<()> {
    use std::io::{BufRead, Write};
    std::thread::Builder::new()
        .name("ctrl".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut line = String::new();
                if std::io::BufReader::new(&stream)
                    .read_line(&mut line)
                    .is_err()
                {
                    continue;
                }
                match parse_ctrl_cmd(&line) {
                    Some(CtrlCmd::Status) => {
                        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
                        if tx.send(CtrlMsg::Status(reply_tx)).is_err() {
                            return;
                        }
                        // Bounded wait: a wedged main loop must not hang the
                        // listener; no reply reads as "unresponsive" client-side.
                        if let Ok(json) = reply_rx.recv_timeout(Duration::from_secs(1)) {
                            let _ = writeln!(stream, "{json}");
                        }
                    }
                    Some(cmd) => {
                        let msg = match cmd {
                            CtrlCmd::Toggle => CtrlMsg::Toggle,
                            CtrlCmd::DeriveNow => CtrlMsg::DeriveNow,
                            CtrlCmd::Consolidate => CtrlMsg::Consolidate,
                            CtrlCmd::Reload => CtrlMsg::Reload,
                            CtrlCmd::Status => unreachable!("handled above"),
                        };
                        if tx.send(msg).is_err() {
                            return;
                        }
                    }
                    None => continue,
                }
            }
        })?;
    Ok(())
}

pub(crate) fn spawn_signal_handler(ctrl_tx: Sender<CtrlMsg>) -> anyhow::Result<()> {
    use signal_hook::consts::{SIGINT, SIGTERM};
    let mut signals = signal_hook::iterator::Signals::new([SIGTERM, SIGINT])?;
    std::thread::Builder::new()
        .name("signal".into())
        .spawn(move || {
            let mut requested = false;
            for _ in signals.forever() {
                if requested {
                    tracing::warn!("second shutdown signal; forcing exit");
                    std::process::exit(1);
                }
                requested = true;
                tracing::info!("shutdown signal received");
                if ctrl_tx.send(CtrlMsg::Shutdown).is_err() {
                    std::process::exit(1); // main loop already gone
                }
            }
        })?;
    Ok(())
}

/// StatusNotifierItem tray icon: left-click / "Show/Hide" toggles the UI
/// child, "Quit" shuts the daemon down (same paths as socket + signals).
#[cfg(target_os = "linux")]
pub(crate) struct ChronicleTray {
    ctrl_tx: Sender<CtrlMsg>,
}

#[cfg(target_os = "linux")]
impl ksni::Tray for ChronicleTray {
    fn id(&self) -> String {
        "chronicle".into()
    }

    fn title(&self) -> String {
        "Chronicle".into()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.ctrl_tx.send(CtrlMsg::Toggle);
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![tray_icon()]
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "Show/Hide".into(),
                enabled: true,
                visible: true,
                activate: Box::new(|t: &mut Self| {
                    let _ = t.ctrl_tx.send(CtrlMsg::Toggle);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit".into(),
                enabled: true,
                visible: true,
                activate: Box::new(|t: &mut Self| {
                    let _ = t.ctrl_tx.send(CtrlMsg::Shutdown);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Procedural icon (filled circle, UI accent blue) — no image asset/dep.
/// Platform-neutral RGBA8 pixels: ksni's [`tray_icon`] packs them to ARGB,
/// tray-icon on macOS (`tray_macos.rs`) takes RGBA (and a black/alpha
/// variant as its template icon) directly via `Icon::from_rgba`. `size` is
/// the square's edge in pixels: ksni wants 22, tray-icon on macOS wants 44
/// (it rescales its template icon to 18pt, so 44px stays crisp on Retina).
pub(crate) fn tray_pixels(size: u32) -> (u32, u32, Vec<u8>) {
    let (r, g, b) = (0x5e_u8, 0x87_u8, 0xea_u8);
    let c = (size - 1) as f32 / 2.0;
    let radius = c - 1.0;
    let mut data = Vec::with_capacity((size * size * 4) as usize);
    for y in 0..size {
        for x in 0..size {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            // 1px soft edge.
            let a = ((radius + 0.5 - d).clamp(0.0, 1.0) * 255.0) as u8;
            data.extend_from_slice(&[r, g, b, a]);
        }
    }
    (size, size, data)
}

#[cfg(target_os = "linux")]
pub(crate) fn tray_icon() -> ksni::Icon {
    let (width, height, rgba) = tray_pixels(22);
    // ARGB32 in network byte order, ksni's own format.
    let mut data = Vec::with_capacity(rgba.len());
    let (pixels, _) = rgba.as_chunks::<4>();
    for &[r, g, b, a] in pixels {
        data.extend_from_slice(&[a, r, g, b]);
    }
    ksni::Icon {
        width: width as i32,
        height: height as i32,
        data,
    }
}

/// Host the tray's D-Bus service on a background thread. No tray host running
/// is non-fatal: log and continue, like the AW endpoint port conflict.
#[cfg(target_os = "linux")]
pub(crate) fn spawn_tray(ctrl_tx: Sender<CtrlMsg>) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("tray runtime failed: {e}");
                    return;
                }
            };
            rt.block_on(async move {
                use ksni::TrayMethods;
                match (ChronicleTray { ctrl_tx }).spawn().await {
                    // Keep the reactor (and the D-Bus service) alive for the
                    // daemon's lifetime; dropping the handle removes the icon.
                    Ok(_handle) => std::future::pending::<()>().await,
                    Err(e) => tracing::warn!("tray unavailable: {e}"),
                }
            });
        })?;
    Ok(())
}

pub(crate) fn toggle_ui(slot: &mut Option<Child>) {
    use std::io::Write;
    if let Some(child) = slot {
        let alive = matches!(child.try_wait(), Ok(None));
        if alive
            && let Some(stdin) = child.stdin.as_mut()
            && stdin
                .write_all(b"toggle\n")
                .and_then(|()| stdin.flush())
                .is_ok()
        {
            return;
        }
        *slot = None; // exited or pipe broken — respawn
    }
    match spawn_ui_child() {
        Ok(child) => *slot = Some(child),
        Err(e) => tracing::error!("failed to spawn ui child: {e}"),
    }
}

/// Own binary path for spawning children. When the binary is replaced while
/// the daemon runs (dev rebuild, upgrade), /proc/self/exe reads
/// "<path> (deleted)"; fall back to the current binary at the same path —
/// mild version skew beats a spawn failure.
pub(crate) fn own_exe() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    if !exe.exists()
        && let Some(stripped) = exe.to_str().and_then(|s| s.strip_suffix(" (deleted)"))
    {
        let replaced = PathBuf::from(stripped);
        if replaced.exists() {
            return Ok(replaced);
        }
    }
    Ok(exe)
}

pub(crate) fn spawn_ui_child() -> std::io::Result<Child> {
    Command::new(own_exe()?)
        .arg("ui")
        .stdin(Stdio::piped())
        .spawn()
}

/// llama/ggml noise goes to the log file, not the UI's terminal.
pub(crate) fn spawn_chat_worker(
    conversation_id: i64,
    task_scope: Option<i64>,
) -> std::io::Result<Child> {
    let mut cmd = Command::new(own_exe()?);
    cmd.args([
        "chat-worker",
        "--conversation",
        &conversation_id.to_string(),
    ]);
    if let Some(task_id) = task_scope {
        cmd.args(["--task", &task_id.to_string()]);
    }
    cmd.stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
}

pub(crate) fn reap_ui(slot: &mut Option<Child>) {
    if let Some(child) = slot
        && matches!(child.try_wait(), Ok(Some(_)))
    {
        *slot = None;
    }
}

pub(crate) fn run(data_dir: &Path) -> anyhow::Result<()> {
    // Single instance: a second `chronicle` just toggles the running daemon.
    let sock = socket_path(data_dir);
    if send_ctrl(&sock, "toggle") {
        println!("chronicle daemon already running \u{2014} toggled UI");
        return Ok(());
    }
    let _ = std::fs::remove_file(&sock); // stale socket from an unclean exit
    let listener = std::os::unix::net::UnixListener::bind(&sock)
        .with_context(|| format!("failed to bind {}", sock.display()))?;
    let (ctrl_tx, ctrl_rx) = crossbeam_channel::unbounded();

    #[cfg(target_os = "linux")]
    return run_loop(data_dir, listener, ctrl_tx, ctrl_rx);

    // NSStatusItem and NSApplication must live on the main thread; the loop
    // owns nothing that needs it, so it moves to its own thread and the main
    // thread hosts the tray for the rest of the process's life.
    #[cfg(target_os = "macos")]
    {
        // A panic on the daemon-main thread must not leave the main thread
        // parked in `NSApplication::run()` with a dead daemon behind it:
        // launchd only relaunches on process exit, and a panicked thread
        // alone doesn't produce one. Run the default hook first (still logs
        // to stderr/log file), then force the exit ourselves.
        let prev_hook = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            prev_hook(info);
            tracing::error!("panic on daemon-main thread: {info}");
            std::process::exit(101);
        }));
        // No GUI session (over ssh, or run as a LaunchDaemon rather than a
        // LaunchAgent): `NSApplication::sharedApplication` aborts in
        // `_RegisterApplication` without one, so skip the tray and run the
        // loop on this thread instead.
        if !chronicle_capture::macos::lock::gui_session() {
            return run_loop(data_dir, listener, ctrl_tx, ctrl_rx);
        }
        let data_dir = data_dir.to_path_buf();
        let tray_ctrl_tx = ctrl_tx.clone();
        std::thread::Builder::new()
            .name("daemon-main".into())
            .spawn(
                move || match run_loop(&data_dir, listener, ctrl_tx, ctrl_rx) {
                    Ok(()) => std::process::exit(0),
                    Err(e) => {
                        tracing::error!("daemon loop failed: {e}");
                        std::process::exit(1);
                    }
                },
            )
            .context("failed to spawn daemon-main thread")?;
        crate::tray_macos::run_main(tray_ctrl_tx)
    }
}

/// A tick more than the interval plus the idle threshold late on the wall
/// clock is a suspend: the ledger row ends there (the AFK poller marks the
/// idle stretch on its own). From `config` each tick, so a reload's
/// `afk_close_secs` counts.
fn sleep_gap_ms(config: &Config) -> i64 {
    SESSIONIZE_EVERY.as_millis() as i64 + i64::from(config.afk_close_secs) * 1000
}

fn run_loop(
    data_dir: &Path,
    listener: std::os::unix::net::UnixListener,
    ctrl_tx: Sender<CtrlMsg>,
    ctrl_rx: crossbeam_channel::Receiver<CtrlMsg>,
) -> anyhow::Result<()> {
    let _guard = init_logging(data_dir)?;
    let mut config = Config::load(&data_dir.join("config.toml"))?;
    if let Some(issue) = config.project_issue() {
        tracing::warn!("config.toml [[projects]]: {issue}");
    }
    let mut filters = Filters::new(&config)?;
    let mut conn = chronicle_core::storage::open(&data_dir.join("chronicle.db"))?;
    // Daemon downtime must not read as focus time: mark a gap as AFK-from-the-
    // last-event; the AFK poller's initial state announcement closes it.
    let last_ms = chronicle_core::storage::latest_event_ts(&conn)?;
    if let Some(last_ms) = last_ms
        && Timestamp::now().as_millisecond() - last_ms > GAP_MARKER_SECS * 1000
    {
        let ts = chronicle_core::types::ms_to_ts(last_ms + 1);
        chronicle_core::storage::insert_event(&conn, &CaptureEvent::Afk { idle: true, ts })?;
    }
    // A `running` batch with no live worker (unclean daemon exit) burns its
    // attempt and falls back to `failed` so the retry cap still holds.
    chronicle_core::storage::reset_stale_running(&conn)?;
    // Capture ledger (m32 chunk 0): a row the last run never closed ends at
    // its last event as a crash; spans that outlived their events get cut.
    chronicle_core::storage::close_crashed_runs(&conn, last_ms)?;
    let cut = chronicle_core::storage::clamp_quiet_spans(&mut conn, config.afk_close_secs)?;
    if cut > 0 {
        tracing::info!(n = cut, "cut spans that outlived their last event");
    }
    let mut run_id = Some(chronicle_core::storage::open_run(
        &conn,
        Timestamp::now().as_millisecond(),
    )?);
    let started = Instant::now();
    let (tx, rx) = crossbeam_channel::unbounded();
    spawn_signal_handler(ctrl_tx.clone())?;
    #[cfg(target_os = "linux")]
    spawn_tray(ctrl_tx.clone())?;
    spawn_capture(&config, data_dir, tx.clone(), ctrl_tx.clone())?;
    spawn_ctrl_listener(listener, ctrl_tx)?;
    // Port taken (a real aw-server?) must not kill capture: log, warn in UI.
    let api_key = wakapi_api_key(&conn)?;
    // The key exists either way so Settings can show it; the switch decides
    // whether the routes accept it.
    let server_error =
        match chronicle_server::spawn(&config, tx, config.editor_heartbeats.then_some(api_key)) {
            Ok(()) => None,
            Err(e) => {
                tracing::error!("AW endpoint failed on 127.0.0.1:{}: {e}", config.port);
                Some(format!("AW endpoint failed on port {}: {e}", config.port))
            }
        };
    chronicle_core::storage::set_meta(&conn, "server_error", server_error.as_deref())?;
    tracing::info!(?data_dir, "chronicle daemon running");
    let mut ui_child: Option<Child> = None;
    // First run without a model: derivation can't start, so surface the UI
    // (and its onboarding card) instead of sitting silent in the background.
    if chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir).is_none() {
        toggle_ui(&mut ui_child);
    }
    let mut scheduler = Scheduler::new();
    let _ = chronicle_core::storage::set_derive_progress(&conn, None);
    let mut distractions = chronicle_core::evidence::compile_patterns(&config.distraction_patterns);
    // The soft tier's embedding model (m30 chunk 6), loaded on first use
    // when `embed_model` names a file that exists; `Some(None)` = tried and
    // failed, do not retry every tick.
    let mut embedder: Option<Option<chronicle_derive::embed::Embedder>> = None;
    let mut idle_since: Option<i64> = None;
    // Fires once per idle stretch: remembers which idle_since epoch already
    // queued checkpoints, cleared when the user comes back.
    let mut checkpointed_idle: Option<i64> = None;
    let mut next_refresh = Instant::now() + SESSIONIZE_EVERY;
    let mut next_sources = Instant::now();
    let mut last_prepass = Instant::now();
    let mut last_tick = Timestamp::now();
    let exit_reason = 'daemon: loop {
        let timeout = next_refresh.saturating_duration_since(Instant::now());
        // Cloned per iteration so the select borrow does not pin the scheduler.
        let resident_rx = scheduler.resident_rx();
        crossbeam_channel::select! {
            recv(resident_rx) -> reply => match reply {
                Ok(reply) => scheduler.on_reply(&conn, reply),
                Err(_) => scheduler.resident_disconnected(&conn),
            },
            recv(rx) -> event => {
                let Ok(event) = event else { break 'daemon ExitReason::CaptureDied };
                if filters.excluded(&event) {
                    continue;
                }
                if let CaptureEvent::Afk { idle, ts } | CaptureEvent::Lock { locked: idle, ts } =
                    &event
                {
                    if *idle {
                        idle_since.get_or_insert(ts.as_millisecond());
                    } else {
                        idle_since = None;
                    }
                }
                if let Err(e) = chronicle_core::storage::insert_event(&conn, &event) {
                    tracing::error!("event insert failed: {e}");
                }
            }
            recv(ctrl_rx) -> cmd => {
                match cmd {
                    Ok(CtrlMsg::Toggle) => {
                        // Fresh spans the moment the window appears, not up to
                        // a tick later.
                        if let Err(e) = chronicle_core::sessionizer::refresh(&mut conn, &config, Timestamp::now()) {
                            tracing::error!("sessionize refresh failed: {e}");
                        }
                        toggle_ui(&mut ui_child);
                    }
                    Ok(CtrlMsg::DeriveNow) => scheduler.tick(&conn, &config, data_dir, idle_since, true),
                    Ok(CtrlMsg::Consolidate) => {
                        scheduler.consolidate_requested = true;
                        scheduler.tick(&conn, &config, data_dir, idle_since, true);
                    }
                    Ok(CtrlMsg::Reload) => {
                        // The projects and their rules apply to the next
                        // sessionize and placement; capture threads keep
                        // the settings they started with.
                        match Config::load(&data_dir.join("config.toml")) {
                            Ok(fresh) => {
                                config = fresh;
                                if let Some(issue) = config.project_issue() {
                                    tracing::warn!("config.toml [[projects]]: {issue}");
                                }
                                match Filters::new(&config) {
                                    Ok(f) => filters = f,
                                    Err(e) => tracing::error!("exclusion filters not reloaded: {e:#}"),
                                }
                                distractions = chronicle_core::evidence::compile_patterns(&config.distraction_patterns);
                                tracing::info!("config.toml reloaded: {} projects", config.projects_effective().len());
                            }
                            Err(e) => tracing::error!("config.toml reload failed, keeping the loaded one: {e}"),
                        }
                    }
                    Ok(CtrlMsg::Status(reply)) => {
                        let _ = reply.send(status_json(&scheduler, idle_since, &mut ui_child, started));
                    }
                    Ok(CtrlMsg::CaptureLost { reason, ts }) => {
                        if let Some(id) = run_id.take()
                            && let Err(e) = chronicle_core::storage::close_run(&conn, id, ts.as_millisecond(), reason)
                        {
                            tracing::error!("ledger close failed: {e}");
                        }
                    }
                    Ok(CtrlMsg::CaptureBack(ts)) => {
                        if run_id.is_none() {
                            run_id = chronicle_core::storage::open_run(&conn, ts.as_millisecond())
                                .map_err(|e| tracing::error!("ledger open failed: {e}"))
                                .ok();
                        }
                    }
                    Ok(CtrlMsg::Shutdown) => break 'daemon ExitReason::Signal,
                    Err(_) => {}
                }
            }
            default(timeout) => {
                let now = Timestamp::now();
                if now.as_millisecond() - last_tick.as_millisecond() > sleep_gap_ms(&config)
                    && let Some(id) = run_id.take()
                {
                    tracing::info!(since = %last_tick, "wall clock jumped: machine slept");
                    if let Err(e) = chronicle_core::storage::close_run(&conn, id, last_tick.as_millisecond(), "sleep") {
                        tracing::error!("ledger close failed: {e}");
                    }
                    run_id = chronicle_core::storage::open_run(&conn, now.as_millisecond())
                        .map_err(|e| tracing::error!("ledger open failed: {e}"))
                        .ok();
                }
                last_tick = now;
                if let Err(e) = chronicle_core::sessionizer::refresh(&mut conn, &config, now) {
                    tracing::error!("sessionize refresh failed: {e}");
                }
                embed_new_spans(&mut conn, &config, data_dir, &mut embedder);
                embed_new_examples(&mut conn, &config, data_dir, &mut embedder);
                let segmenter = config.derive_mode == "segmenter";
                if config.prepass_secs > 0
                    && last_prepass.elapsed() >= Duration::from_secs(u64::from(config.prepass_secs))
                {
                    last_prepass = Instant::now();
                    if segmenter {
                        match chronicle_core::segmenter::run(&mut conn, &config, now, &distractions) {
                            Ok(placed) if !placed.is_empty() => {
                                tracing::debug!(n = placed.len(), "segmenter placed segments");
                            }
                            Ok(_) => {}
                            Err(e) => tracing::error!("segmenter failed: {e}"),
                        }
                    } else {
                        match chronicle_core::prepass::run(&mut conn, &config, now) {
                            Ok(placed) if !placed.is_empty() => {
                                tracing::debug!(n = placed.len(), "pre-pass placed runs");
                            }
                            Ok(_) => {}
                            Err(e) => tracing::error!("pre-pass failed: {e}"),
                        }
                    }
                    if let Err(e) =
                        chronicle_core::proposals::refresh(&mut conn, now, &distractions)
                    {
                        tracing::error!("proposals refresh failed: {e}");
                    }
                    if !segmenter {
                        scheduler.maybe_live(&conn, &config, data_dir, idle_since);
                    }
                }
                if segmenter {
                    reconcile_due(&mut conn, &config, &distractions);
                    if scheduler.cloud_kinds.iter().any(|k| k == "advise") {
                        advise_due(&conn);
                    }
                    if let Err(e) = chronicle_core::segmenter::daily(&mut conn, &config, now) {
                        tracing::error!("segmenter daily housekeeping failed: {e}");
                    }
                }
                match chronicle_core::self_score::daily(&conn, now, &TimeZone::system()) {
                    Ok(true) => tracing::info!("self-score refreshed"),
                    Ok(false) => {}
                    Err(e) => tracing::error!("self-score failed: {e}"),
                }
                if Instant::now() >= next_sources {
                    refresh_sources(&mut conn, &config, now);
                    next_sources = Instant::now() + SOURCES_EVERY;
                }
                scheduler.tick(&conn, &config, data_dir, idle_since, false);
                maybe_enqueue_checkpoints(&conn, &config, idle_since, &mut checkpointed_idle, now);
                maybe_enqueue_standup(
                    &conn,
                    now,
                    scheduler.cloud_kinds.iter().any(|k| k == "reconcile_day"),
                );
                reap_ui(&mut ui_child);
                next_refresh = Instant::now() + SESSIONIZE_EVERY;
            }
        }
    };
    tracing::info!("shutting down");
    let now = Timestamp::now();
    let reason = match exit_reason {
        ExitReason::Signal => "shutdown",
        ExitReason::CaptureDied => "provider exit",
    };
    if let Some(id) = run_id
        && let Err(e) = chronicle_core::storage::close_run(&conn, id, now.as_millisecond(), reason)
    {
        tracing::error!("ledger close failed: {e}");
    }
    // The open span ends with capture, not at the next start's refresh.
    if let Err(e) = chronicle_core::storage::insert_event(
        &conn,
        &CaptureEvent::Afk {
            idle: true,
            ts: now,
        },
    ) {
        tracing::error!("shutdown afk marker failed: {e}");
    }
    // Child drop leaks the OS process — kills must be explicit.
    scheduler.shutdown(&conn);
    if let Some(mut child) = ui_child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    match exit_reason {
        ExitReason::Signal => Ok(()),
        ExitReason::CaptureDied => bail!("capture threads exited"),
    }
}

pub(crate) enum ExitReason {
    Signal,
    CaptureDied,
}

/// Lunch-scale AFK (m16): once per idle stretch, queue a background
/// checkpoint job for every task with activity since its last checkpoint.
/// Day end needs no separate trigger — the evening's long idle is one.
pub(crate) fn maybe_enqueue_checkpoints(
    conn: &rusqlite::Connection,
    config: &Config,
    idle_since: Option<i64>,
    checkpointed_idle: &mut Option<i64>,
    now: Timestamp,
) {
    use chronicle_core::storage;
    if config.checkpoint_afk_secs == 0 {
        return;
    }
    let Some(since) = idle_since else {
        *checkpointed_idle = None;
        return;
    };
    if *checkpointed_idle == Some(since)
        || now.as_millisecond() - since < i64::from(config.checkpoint_afk_secs) * 1000
    {
        return;
    }
    *checkpointed_idle = Some(since);
    let tasks = match storage::tasks_needing_checkpoint(conn, since) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("checkpoint eligibility query failed: {e}");
            return;
        }
    };
    for task_id in tasks {
        if let Err(e) = storage::enqueue_ai_job(
            conn,
            now,
            "checkpoint",
            0,
            &storage::task_description_payload(task_id),
        ) {
            tracing::error!(task_id, "checkpoint enqueue failed: {e}");
        }
    }
}

/// Standup draft trigger: once yesterday has journal entries and no draft or
/// queued job exists for it, queue a background standup job. The gate is
/// pure DB state (draft row + job dedupe), so it is restart-safe and cheap
/// to re-check every tick.
pub(crate) fn maybe_enqueue_standup(conn: &rusqlite::Connection, now: Timestamp, nightly: bool) {
    use chronicle_core::storage;
    let tz = TimeZone::system();
    let today = now.to_zoned(tz.clone()).date();
    let Ok(yd) = today.checked_sub(jiff::Span::new().days(1)) else {
        return;
    };
    let day = yd.to_string();
    // With a night-pass route (m36 chunk 4) yesterday goes to the batch as
    // one job — advisories, unnamed tasks and the standup — once per day.
    if nightly {
        let payload = storage::standup_payload(&day);
        if storage::job_seen(conn, "reconcile_day", &payload).unwrap_or(true) {
            return;
        }
        if let Err(e) = storage::enqueue_ai_job(conn, now, "reconcile_day", 0, &payload) {
            tracing::error!(%day, "night pass enqueue failed: {e}");
        }
        return;
    }
    if storage::get_standup_draft(conn, &day)
        .map(|d| d.is_some())
        .unwrap_or(true)
        || storage::pending_standup_job(conn, &day).unwrap_or(true)
    {
        return;
    }
    let (Ok(lo), Ok(hi)) = (yd.to_zoned(tz.clone()), today.to_zoned(tz)) else {
        return;
    };
    let (lo, hi) = (
        lo.timestamp().as_millisecond(),
        hi.timestamp().as_millisecond(),
    );
    match storage::standup_digest(conn, lo, hi) {
        Ok(rows) if !rows.is_empty() => {
            if let Err(e) =
                storage::enqueue_ai_job(conn, now, "standup", 0, &storage::standup_payload(&day))
            {
                tracing::error!(%day, "standup enqueue failed: {e}");
            }
        }
        Ok(_) => {}
        Err(e) => tracing::error!(%day, "standup eligibility query failed: {e}"),
    }
}

pub(crate) const DERIVE_TIMEOUT: Duration = Duration::from_secs(300);
/// The night pass polls a message batch that may take an hour or more.
pub(crate) const NIGHT_PASS_TIMEOUT: Duration = Duration::from_secs(4 * 3600);
/// "Low 1-min load" gate for deriving while the user is active.
pub(crate) const LOW_LOAD: f64 = 1.0;
pub(crate) const BATTERY_DEFER_PCT: u32 = 30;

/// The resident derive worker (m27 chunk 3): one process holding the model
/// and a KV cache with the instruction prefix. Requests go down its stdin; a
/// reader thread relays its replies.
pub(crate) struct Resident {
    child: Child,
    stdin: std::process::ChildStdin,
    rx: crossbeam_channel::Receiver<deriveproto::Reply>,
    ready: bool,
    /// The request in flight and when it was sent.
    busy: Option<(Job, Instant)>,
    last_used: Instant,
    /// The feed's "deriving…" row, mirrored to meta `derive_progress`.
    progress: Option<chronicle_core::storage::DeriveProgress>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) enum Job {
    Batch(i64),
    Live { lo: i64, hi: i64 },
    Consolidate { lo: i64, hi: i64 },
}

/// The day tier runs at the first long AFK after this local hour …
pub(crate) const CONSOLIDATE_AFTER_HOUR: i8 = 14;
/// … or unconditionally (under the batch gates) after this one.
pub(crate) const CONSOLIDATE_FALLBACK_HOUR: i8 = 18;

/// The live tier looks back at most this far for the current stretch.
pub(crate) const LIVE_LOOKBACK_MS: i64 = 15 * 60_000;
/// A live pass needs this much focus in its window.
pub(crate) const LIVE_MIN_FOCUS_MS: i64 = 5 * 60_000;
/// A live pass slower than this skips the next one (self-throttle).
pub(crate) const LIVE_SLOW: Duration = Duration::from_secs(60);

/// Daemon-side backstop over the worker's own idle exit.
pub(crate) const RESIDENT_IDLE_GRACE_SECS: u64 = 60;

/// A cloud job's own retries fit in 120 s; past this the worker is stuck.
pub(crate) const CLOUD_JOB_TIMEOUT: Duration = Duration::from_secs(180);
pub(crate) const CLOUD_BACKOFF_MIN: Duration = Duration::from_secs(60);
pub(crate) const CLOUD_BACKOFF_MAX: Duration = Duration::from_secs(600);

pub(crate) struct Scheduler {
    /// AI jobs stay one-shot subprocesses (`Describer`/`ChatModel`); at most
    /// one inference process works at a time, derive or ai-job.
    /// The running ai-job worker, when it started, its id, and how long
    /// it may run (the night pass waits on a batch for hours).
    ai_job: Option<(Child, Instant, i64, Duration)>,
    /// A second slot for jobs routed to a cloud backend (m31): they wait on
    /// the network, not the CPU, so they neither queue behind a local batch
    /// nor hold one up, and they skip the idle and battery gates.
    cloud_job: Option<(Child, Instant, i64)>,
    /// Job kinds `models.toml` currently routes to a cloud backend, and the
    /// file's mtime they were read at.
    cloud_kinds: Vec<String>,
    cloud_kinds_mtime: Option<std::time::SystemTime>,
    /// After a cloud job came back deferred (`cloud: …`), hold the slot
    /// until then; doubles from 1 to 10 minutes and resets on success.
    cloud_backoff_until: Option<Instant>,
    cloud_backoff: Duration,
    resident: Option<Resident>,
    last_live: Option<Instant>,
    /// Wall time of the last live pass; over `LIVE_SLOW` skips one pass.
    last_live_took: Option<Duration>,
    /// Home's "tidy today" asked for a run regardless of time or stamp.
    consolidate_requested: bool,
    /// A run failed today; wait for a request rather than retrying.
    consolidate_failed_day: Option<String>,
}

pub(crate) static NEVER_REPLY: std::sync::LazyLock<
    crossbeam_channel::Receiver<deriveproto::Reply>,
> = std::sync::LazyLock::new(crossbeam_channel::never);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct DaemonStatus {
    pub(crate) uptime_secs: u64,
    pub(crate) derive_active: bool,
    /// What the inference slot is running ("derive batch 71", "ai job 12")
    /// and for how long; absent when idle or from a pre-m27 daemon.
    #[serde(default)]
    pub(crate) worker: Option<String>,
    #[serde(default)]
    pub(crate) worker_secs: Option<u64>,
    /// The resident derive worker is up with the model loaded.
    #[serde(default)]
    pub(crate) model_resident: bool,
    pub(crate) idle_secs: Option<u64>,
    pub(crate) ui_open: bool,
    /// Which focus provider capture is running (m39): `x11`, `wlr` or
    /// `kwin`. Absent on macOS, which has one.
    #[serde(default)]
    pub(crate) focus_route: Option<String>,
}

pub(crate) fn status_json(
    scheduler: &Scheduler,
    idle_since: Option<i64>,
    ui_child: &mut Option<Child>,
    started: Instant,
) -> String {
    let busy = scheduler.busy().or_else(|| {
        scheduler
            .cloud_job
            .as_ref()
            .map(|(_, started, id)| (format!("cloud job {id}"), *started))
    });
    let status = DaemonStatus {
        uptime_secs: started.elapsed().as_secs(),
        derive_active: busy.is_some(),
        worker: busy.as_ref().map(|(what, _)| what.clone()),
        worker_secs: busy.as_ref().map(|(_, since)| since.elapsed().as_secs()),
        model_resident: scheduler.resident.as_ref().is_some_and(|r| r.ready),
        idle_secs: idle_since
            .map(|since| ((Timestamp::now().as_millisecond() - since) / 1000).max(0) as u64),
        ui_open: ui_child
            .as_mut()
            .is_some_and(|c| matches!(c.try_wait(), Ok(None))),
        focus_route: crate::capture::focus_route_name().map(str::to_owned),
    };
    serde_json::to_string(&status).unwrap_or_default()
}

#[derive(Debug, PartialEq)]
pub(crate) enum ResidentAction {
    Keep,
    KillTimeout,
    KillIdle,
}

/// Pure state rule for the resident worker: a request past `timeout` kills
/// it; an idle worker past `idle` is reaped (it exits itself first).
pub(crate) fn resident_action(
    busy_since: Option<Instant>,
    last_used: Instant,
    now: Instant,
    timeout: Duration,
    idle: Duration,
) -> ResidentAction {
    match busy_since {
        Some(since) if now.duration_since(since) >= timeout => ResidentAction::KillTimeout,
        Some(_) => ResidentAction::Keep,
        None if now.duration_since(last_used) >= idle => ResidentAction::KillIdle,
        None => ResidentAction::Keep,
    }
}

impl Scheduler {
    fn new() -> Self {
        Self {
            ai_job: None,
            cloud_job: None,
            cloud_kinds: Vec::new(),
            cloud_kinds_mtime: None,
            cloud_backoff_until: None,
            cloud_backoff: CLOUD_BACKOFF_MIN,
            resident: None,
            last_live: None,
            last_live_took: None,
            consolidate_requested: false,
            consolidate_failed_day: None,
        }
    }

    /// The resident worker's reply channel for the daemon's select loop (a
    /// never-ready channel when no worker is up).
    fn resident_rx(&self) -> crossbeam_channel::Receiver<deriveproto::Reply> {
        match &self.resident {
            Some(r) => r.rx.clone(),
            None => NEVER_REPLY.clone(),
        }
    }

    /// What the local inference slot is doing, and since when. The cloud
    /// slot (m31) is separate: it never blocks derivation or a live pass.
    fn busy(&self) -> Option<(String, Instant)> {
        if let Some((_, started, id, _)) = &self.ai_job {
            return Some((format!("ai job {id}"), *started));
        }
        if let Some(r) = &self.resident
            && let Some((job, since)) = r.busy
        {
            let what = match job {
                Job::Batch(id) => format!("derive batch {id}"),
                Job::Live { .. } => "live pass".to_owned(),
                Job::Consolidate { .. } => "tidying today".to_owned(),
            };
            return Some((what, since));
        }
        None
    }

    /// Live tier (chunk 4), on the pre-pass timer: while the user is active
    /// and the slot is free, label the stretch since the last boundary.
    fn maybe_live(
        &mut self,
        conn: &rusqlite::Connection,
        config: &Config,
        data_dir: &Path,
        idle_since: Option<i64>,
    ) {
        use chronicle_core::storage;
        if config.live_secs == 0 || idle_since.is_some() || self.busy().is_some() {
            return;
        }
        let every = Duration::from_secs(u64::from(config.live_secs));
        if self.last_live.is_some_and(|t| t.elapsed() < every) {
            return;
        }
        if let Some(took) = self.last_live_took.take()
            && took > LIVE_SLOW
        {
            tracing::info!(took_secs = took.as_secs(), "live pass slow; skipping one");
            self.last_live = Some(Instant::now());
            return;
        }
        if !load_1min().is_some_and(|l| l < LOW_LOAD) || on_low_battery() {
            return;
        }
        if chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir).is_none() {
            return;
        }
        let now = Timestamp::now().as_millisecond();
        // The live tier labels the tail only: a stretch inside a closed batch
        // belongs to that batch's derive, which replaces live rows it covers.
        let tail_start = storage::latest_batch_end(conn).ok().flatten().unwrap_or(0);
        let floor = (now - LIVE_LOOKBACK_MS).max(tail_start);
        let lo = match storage::live_window_start(conn, floor, now) {
            Ok(lo) => lo,
            Err(e) => {
                tracing::error!("live window query failed: {e}");
                return;
            }
        };
        let focus = storage::focus_ms_in(conn, lo, now).unwrap_or(0);
        if focus < LIVE_MIN_FOCUS_MS {
            return;
        }
        self.last_live = Some(Instant::now());
        self.dispatch(conn, Job::Live { lo, hi: now });
    }

    fn tick(
        &mut self,
        conn: &rusqlite::Connection,
        config: &Config,
        data_dir: &Path,
        idle_since: Option<i64>,
        force: bool,
    ) {
        use chronicle_core::storage;
        self.reap_ai_job(conn);
        self.reap_cloud_job(conn);
        self.poll_resident(conn, config);
        self.refresh_cloud_kinds(data_dir);
        self.maybe_cloud_job(conn);
        if self.busy().is_some() {
            return; // one local inference process at a time
        }
        let cloud_owned = self.cloud_kinds.clone();
        let cloud: Vec<&str> = cloud_owned.iter().map(String::as_str).collect();
        // An interactive AI job (a user actively waiting on a suggestion)
        // jumps the idle gate; background jobs and derivation respect it.
        let interactive =
            storage::next_eligible_ai_job_in(conn, storage::AI_JOB_INTERACTIVE, &cloud, false)
                .ok()
                .flatten();
        if interactive.is_none() && !force && !derive_gates_open(config, idle_since) {
            return;
        }
        if on_low_battery() {
            tracing::debug!("inference deferred: battery low");
            return;
        }
        prune_if_due(conn, config);
        if chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir).is_none() {
            tracing::debug!("inference skipped: no model downloaded");
            return;
        }
        if let Some(job_id) = interactive {
            self.spawn_ai_job(conn, job_id);
            return;
        }
        // Derivation stays ahead of background summarization.
        match storage::next_eligible_batch(conn) {
            Ok(Some(batch_id)) => {
                self.dispatch(conn, Job::Batch(batch_id));
                return;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!("eligible-batch query failed: {e}");
                return;
            }
        }
        if self.maybe_consolidate(conn, config, idle_since) {
            return;
        }
        if let Ok(Some(job_id)) = storage::next_eligible_ai_job_in(conn, i64::MIN, &cloud, false) {
            self.spawn_ai_job(conn, job_id);
        }
    }

    /// Re-read the routing table when `models.toml` changed (Settings
    /// writes it while the daemon runs; config.toml needs a restart, this
    /// does not).
    fn refresh_cloud_kinds(&mut self, data_dir: &Path) {
        use chronicle_core::models_config::ModelsConfig;
        let mtime = std::fs::metadata(ModelsConfig::path(data_dir))
            .and_then(|m| m.modified())
            .ok();
        if mtime == self.cloud_kinds_mtime && (mtime.is_some() || self.cloud_kinds.is_empty()) {
            return;
        }
        self.cloud_kinds_mtime = mtime;
        self.cloud_kinds = match ModelsConfig::load(data_dir) {
            Ok(m) => m.cloud_kinds(),
            Err(e) => {
                tracing::error!("models.toml unreadable: {e:#}");
                Vec::new()
            }
        };
        tracing::info!(kinds = ?self.cloud_kinds, "cloud routes loaded");
    }

    /// Dispatch one cloud-routed job into the cloud slot: no idle, battery
    /// or local-model gate, no wait on the local slot.
    fn maybe_cloud_job(&mut self, conn: &rusqlite::Connection) {
        use chronicle_core::storage;
        if self.cloud_job.is_some() || self.cloud_kinds.is_empty() {
            return;
        }
        if self.cloud_backoff_until.is_some_and(|t| Instant::now() < t) {
            return;
        }
        let kinds: Vec<&str> = self.cloud_kinds.iter().map(String::as_str).collect();
        match storage::next_eligible_ai_job_in(conn, i64::MIN, &kinds, true) {
            Ok(Some(job_id)) => match spawn_ai_job_worker(job_id) {
                Ok(child) => {
                    tracing::info!(id = job_id, "cloud ai-job worker spawned");
                    self.cloud_job = Some((child, Instant::now(), job_id));
                }
                Err(e) => tracing::error!(id = job_id, "failed to spawn cloud ai-job worker: {e}"),
            },
            Ok(None) => {}
            Err(e) => tracing::error!("eligible cloud job query failed: {e}"),
        }
    }

    /// The cloud slot's reaper: a worker that left its job `pending` with a
    /// `cloud:` reason hit the provider or the cap; back off before the
    /// next one. A job that finished clears the backoff.
    fn reap_cloud_job(&mut self, conn: &rusqlite::Connection) {
        use chronicle_core::storage;
        let Some((child, started, id)) = &mut self.cloud_job else {
            return;
        };
        let id = *id;
        let finished = match child.try_wait() {
            Ok(Some(status)) => {
                if !status.success() {
                    tracing::warn!(id, %status, "cloud ai-job worker failed");
                }
                true
            }
            Ok(None) => {
                if started.elapsed() >= CLOUD_JOB_TIMEOUT {
                    tracing::warn!(id, "cloud ai-job worker timed out; killing");
                    let _ = child.kill();
                    let _ = child.wait();
                    true
                } else {
                    false
                }
            }
            Err(e) => {
                tracing::error!(id, "cloud ai-job worker wait failed: {e}");
                true
            }
        };
        if !finished {
            return;
        }
        self.cloud_job = None;
        match storage::ai_job_status(conn, id) {
            Ok(Some((status, _))) if status == "running" => {
                let _ = storage::fail_ai_job(conn, id, "worker died");
            }
            Ok(Some((status, detail)))
                if status == "pending"
                    && detail.as_deref().is_some_and(|d| d.starts_with("cloud:")) =>
            {
                let wait = self.cloud_backoff;
                self.cloud_backoff_until = Some(Instant::now() + wait);
                self.cloud_backoff = (wait * 2).min(CLOUD_BACKOFF_MAX);
                tracing::warn!(id, secs = wait.as_secs(), "cloud job deferred; backing off");
            }
            Ok(Some((status, _))) if status == "done" => {
                self.cloud_backoff = CLOUD_BACKOFF_MIN;
                self.cloud_backoff_until = None;
            }
            _ => {}
        }
    }

    /// Day tier (chunk 6): once per local day, at the first AFK ≥
    /// `checkpoint_afk_secs` after 14:00, at 18:00 regardless, or when Home
    /// asked. Returns true when a run was dispatched.
    fn maybe_consolidate(
        &mut self,
        conn: &rusqlite::Connection,
        config: &Config,
        idle_since: Option<i64>,
    ) -> bool {
        use chronicle_core::storage;
        let now = Timestamp::now();
        let local = now.to_zoned(TimeZone::system());
        let day = local.date().to_string();
        let requested = std::mem::take(&mut self.consolidate_requested);
        if !requested {
            if self.consolidate_failed_day.as_deref() == Some(&day)
                || matches!(storage::consolidation_of_day(conn, &day), Ok(Some(_)))
            {
                return false;
            }
            let idle_long = config.checkpoint_afk_secs > 0
                && idle_since.is_some_and(|since| {
                    now.as_millisecond() - since >= i64::from(config.checkpoint_afk_secs) * 1000
                });
            let hour = local.hour();
            let due =
                (hour >= CONSOLIDATE_AFTER_HOUR && idle_long) || hour >= CONSOLIDATE_FALLBACK_HOUR;
            if !due {
                return false;
            }
        }
        let Ok(start) = local.start_of_day() else {
            return false;
        };
        let lo = start.timestamp().as_millisecond();
        let hi = now.as_millisecond();
        tracing::info!(%day, requested, "consolidation dispatched");
        self.dispatch(conn, Job::Consolidate { lo, hi });
        true
    }

    fn reap_ai_job(&mut self, conn: &rusqlite::Connection) {
        use chronicle_core::storage;
        let Some((child, started, id, timeout)) = &mut self.ai_job else {
            return;
        };
        let (id, timeout) = (*id, *timeout);
        // A worker that died before reporting leaves its row `running`;
        // count that as the failed attempt it was.
        let mark_dead = |conn: &rusqlite::Connection| {
            let stuck = matches!(
                storage::ai_job_status(conn, id),
                Ok(Some((ref s, _))) if s == "running"
            );
            if stuck {
                let _ = storage::fail_ai_job(conn, id, "worker died");
            }
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                mark_dead(conn);
                if !status.success() {
                    tracing::warn!(id, %status, "ai-job worker failed");
                }
                self.ai_job = None;
            }
            Ok(None) => {
                if started.elapsed() >= timeout {
                    tracing::warn!(id, "ai-job worker timed out; killing");
                    let _ = child.kill();
                    let _ = child.wait();
                    mark_dead(conn);
                    self.ai_job = None;
                }
            }
            Err(e) => {
                tracing::error!(id, "ai-job worker wait failed: {e}");
                self.ai_job = None;
            }
        }
    }

    /// One reply from the resident worker (the daemon's select loop feeds
    /// these as they arrive; `poll_resident` drains stragglers).
    fn on_reply(&mut self, conn: &rusqlite::Connection, msg: deriveproto::Reply) {
        use chronicle_core::storage;
        use deriveproto::Reply;
        let Some(r) = &mut self.resident else {
            return;
        };
        match msg {
            Reply::Ready => {
                r.ready = true;
                // The request queued behind model load; the timeout clock
                // covers inference only.
                if let Some((_, since)) = &mut r.busy {
                    *since = Instant::now();
                }
                tracing::info!("derive worker ready");
            }
            Reply::Progress { label } => {
                if let Some(p) = &mut r.progress {
                    p.label = label;
                    if let Err(e) = storage::set_derive_progress(conn, Some(p)) {
                        tracing::warn!("derive progress write failed: {e}");
                    }
                }
            }
            Reply::Done {
                batch_id,
                intervals,
            } => {
                tracing::info!(?batch_id, intervals, "derive worker done");
                self.finish_job(conn);
            }
            Reply::Err { batch_id, message } => {
                tracing::warn!(?batch_id, "derive worker: {message}");
                if let Some((Job::Consolidate { lo, .. }, _)) = r.busy {
                    self.consolidate_failed_day = Some(
                        chronicle_core::types::ms_to_ts(lo)
                            .to_zoned(TimeZone::system())
                            .date()
                            .to_string(),
                    );
                }
                self.finish_job(conn);
            }
        }
    }

    /// The in-flight request is over: free the slot, clear the feed row,
    /// remember how long a live pass took.
    fn finish_job(&mut self, conn: &rusqlite::Connection) {
        let Some(r) = &mut self.resident else {
            return;
        };
        if let Some((Job::Live { .. }, since)) = r.busy.take() {
            self.last_live_took = Some(since.elapsed());
        }
        r.last_used = Instant::now();
        r.progress = None;
        let _ = chronicle_core::storage::set_derive_progress(conn, None);
    }

    /// The reader thread ended (worker stdout closed): treat as dead.
    fn resident_disconnected(&mut self, conn: &rusqlite::Connection) {
        if let Some(mut r) = self.resident.take() {
            tracing::warn!(busy = ?r.busy.map(|b| b.0), "derive worker disconnected");
            let _ = r.child.kill();
            let _ = r.child.wait();
            if let Some(day) = fail_stuck(conn, r.busy) {
                self.consolidate_failed_day = Some(day);
            }
            let _ = chronicle_core::storage::set_derive_progress(conn, None);
        }
    }

    /// Drop the resident: optionally kill (and wait) the child, mark any busy
    /// job failed, and clear the progress row.
    fn drop_resident(&mut self, conn: &rusqlite::Connection, kill: bool, wait: bool) {
        let Some(mut r) = self.resident.take() else {
            return;
        };
        if kill {
            let _ = r.child.kill();
        }
        if wait {
            let _ = r.child.wait();
        }
        let lost = fail_stuck(conn, r.busy);
        let _ = chronicle_core::storage::set_derive_progress(conn, None);
        if lost.is_some() {
            self.consolidate_failed_day = lost;
        }
    }

    /// Drain stragglers, then apply the timeout/idle rule.
    fn poll_resident(&mut self, conn: &rusqlite::Connection, config: &Config) {
        while let Some(msg) = self.resident.as_ref().and_then(|r| r.rx.try_recv().ok()) {
            self.on_reply(conn, msg);
        }
        let Some(r) = &mut self.resident else {
            return;
        };
        let action = resident_action(
            r.busy.map(|(_, since)| since),
            r.last_used,
            Instant::now(),
            DERIVE_TIMEOUT,
            Duration::from_secs(u64::from(config.worker_idle_secs) + RESIDENT_IDLE_GRACE_SECS),
        );
        match r.child.try_wait() {
            Ok(Some(status)) => {
                if r.busy.is_some() || !status.success() {
                    tracing::warn!(%status, busy = ?r.busy.map(|b| b.0), "derive worker exited");
                } else {
                    tracing::info!("derive worker exited (idle)");
                }
                self.drop_resident(conn, false, false);
            }
            Ok(None) => match action {
                ResidentAction::Keep => {}
                ResidentAction::KillTimeout => {
                    tracing::warn!(busy = ?r.busy.map(|b| b.0), "derive worker timed out; killing");
                    self.drop_resident(conn, true, true);
                }
                ResidentAction::KillIdle => {
                    tracing::info!("derive worker idle past grace; killing");
                    let _ = r.child.kill();
                    let _ = r.child.wait();
                    self.resident = None;
                }
            },
            Err(e) => {
                tracing::error!("derive worker wait failed: {e}");
                self.drop_resident(conn, true, false);
            }
        }
    }

    fn spawn_ai_job(&mut self, conn: &rusqlite::Connection, job_id: i64) {
        let timeout = match chronicle_core::storage::ai_job_kind(conn, job_id) {
            Ok(Some(k)) if k == "reconcile_day" => NIGHT_PASS_TIMEOUT,
            _ => DERIVE_TIMEOUT,
        };
        match spawn_ai_job_worker(job_id) {
            Ok(child) => {
                tracing::info!(id = job_id, "ai-job worker spawned");
                self.ai_job = Some((child, Instant::now(), job_id, timeout));
            }
            Err(e) => tracing::error!(id = job_id, "failed to spawn ai-job worker: {e}"),
        }
    }

    /// Send a request to the resident worker, spawning it first if needed.
    /// The request queues in the pipe behind model load; `Ready` restarts the
    /// timeout clock so it covers inference only.
    fn dispatch(&mut self, conn: &rusqlite::Connection, job: Job) {
        use chronicle_core::storage;
        use std::io::Write;
        fn progress(
            kind: &str,
            batch_id: Option<i64>,
            start_ts: i64,
            end_ts: i64,
        ) -> storage::DeriveProgress {
            storage::DeriveProgress {
                kind: kind.into(),
                batch_id,
                start_ts,
                end_ts,
                started_ts: Timestamp::now().as_millisecond(),
                label: String::new(),
            }
        }
        let (request, progress) = match job {
            Job::Batch(batch_id) => {
                let (start_ts, end_ts) = storage::batch_row(conn, batch_id)
                    .ok()
                    .flatten()
                    .map_or((0, 0), |b| (b.start_ts, b.end_ts));
                (
                    deriveproto::Request::Derive { batch_id },
                    progress("batch", Some(batch_id), start_ts, end_ts),
                )
            }
            Job::Live { lo, hi } => (
                deriveproto::Request::Live { lo, hi },
                progress("live", None, lo, hi),
            ),
            Job::Consolidate { lo, hi } => (
                deriveproto::Request::Consolidate { lo, hi },
                progress("day", None, lo, hi),
            ),
        };
        if self.resident.is_none() {
            match spawn_resident() {
                Ok(r) => {
                    tracing::info!("derive worker spawned");
                    self.resident = Some(r);
                }
                Err(e) => {
                    tracing::error!("failed to spawn derive worker: {e}");
                    return;
                }
            }
        }
        let Some(r) = self.resident.as_mut() else {
            return;
        };
        let mut line = serde_json::to_string(&request).expect("request serializes");
        line.push('\n');
        match r
            .stdin
            .write_all(line.as_bytes())
            .and_then(|()| r.stdin.flush())
        {
            Ok(()) => {
                tracing::info!(?job, "derive requested");
                r.busy = Some((job, Instant::now()));
                if let Err(e) = storage::set_derive_progress(conn, Some(&progress)) {
                    tracing::warn!("derive progress write failed: {e}");
                }
                r.progress = Some(progress);
            }
            Err(e) => {
                tracing::error!(?job, "derive worker pipe broken: {e}");
                let _ = r.child.kill();
                let _ = r.child.wait();
                self.resident = None;
            }
        }
    }

    /// Kill whatever is running; the rows they were working on go back to
    /// retryable states. SIGKILL is safe: `store_derivation` commits in one
    /// transaction and `fail_batch` records the burned attempt immediately.
    fn shutdown(&mut self, conn: &rusqlite::Connection) {
        use chronicle_core::storage;
        if let Some((mut child, _, id, _)) = self.ai_job.take() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = storage::fail_ai_job(conn, id, "daemon shutdown");
        }
        if let Some(mut r) = self.resident.take() {
            let _ = r.child.kill();
            let _ = r.child.wait();
            let _ = fail_stuck(conn, r.busy);
            let _ = storage::set_derive_progress(conn, None);
        }
    }
}

/// A batch the worker never answered for keeps its `running` row; count
/// that as the failed attempt it was. A lost consolidation returns its day
/// so the scheduler stops retrying it until asked. Live passes have no row
/// to repair.
pub(crate) fn fail_stuck(
    conn: &rusqlite::Connection,
    busy: Option<(Job, Instant)>,
) -> Option<String> {
    use chronicle_core::storage;
    match busy {
        Some((Job::Batch(id), _)) => {
            if matches!(storage::batch_status(conn, id), Ok(Some(ref s)) if s == "running") {
                let _ = storage::fail_batch(conn, id);
            }
            None
        }
        Some((Job::Consolidate { lo, .. }, _)) => Some(
            chronicle_core::types::ms_to_ts(lo)
                .to_zoned(TimeZone::system())
                .date()
                .to_string(),
        ),
        _ => None,
    }
}

pub(crate) fn spawn_resident() -> std::io::Result<Resident> {
    use std::io::BufRead;
    // Worker logging goes to the log file; stderr would only carry llama's
    // own noise.
    let mut child = Command::new(own_exe()?)
        .arg("derive-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdin = child.stdin.take().expect("stdin piped");
    let stdout = child.stdout.take().expect("stdout piped");
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::Builder::new()
        .name("derive-reader".into())
        .spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(msg) = serde_json::from_str::<deriveproto::Reply>(&line)
                    && tx.send(msg).is_err()
                {
                    break;
                }
            }
        })?;
    Ok(Resident {
        child,
        stdin,
        rx,
        ready: false,
        busy: None,
        last_used: Instant::now(),
        progress: None,
    })
}

pub(crate) const PRUNE_EVERY_MS: i64 = 24 * 3_600_000;
pub(crate) const PRUNE_BATCH: usize = 1000;

/// Daily retention prune + stale-task autoclose, piggybacking on the idle
/// gate the caller already checked. The stamp is written even when nothing
/// was deleted (or the prune failed) so a busy DB isn't retried every tick.
pub(crate) fn prune_if_due(conn: &rusqlite::Connection, config: &Config) {
    use chronicle_core::storage;
    if config.retention_days == 0 && config.task_autoclose_days == 0 {
        return; // 0 = keep forever / never autoclose
    }
    let now = Timestamp::now().as_millisecond();
    let last = storage::get_meta(conn, "last_prune_ts")
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    if now - last < PRUNE_EVERY_MS {
        return;
    }
    if config.retention_days != 0 {
        let cutoff = now - i64::from(config.retention_days) * 86_400_000;
        match storage::prune(conn, cutoff, PRUNE_BATCH) {
            Ok(0) => {}
            Ok(rows) => tracing::info!(rows, "retention prune"),
            Err(e) => tracing::error!("retention prune failed: {e}"),
        }
    }
    if config.task_autoclose_days != 0 {
        match storage::autoclose_stale_tasks(conn, Timestamp::now(), config.task_autoclose_days) {
            Ok(0) => {}
            Ok(rows) => tracing::info!(rows, "stale derived tasks closed"),
            Err(e) => tracing::error!("task autoclose failed: {e}"),
        }
    }
    let _ = storage::set_meta(conn, "last_prune_ts", Some(&now.to_string()));
}

/// Key the WakaTime heartbeat routes authenticate with, generated once and
/// shown (with a copy button) in Settings › Connections. 32 url-safe
/// characters from `/dev/urandom`; the 64-character alphabet divides 256, so
/// the byte-to-character map is unbiased.
pub(crate) fn wakapi_api_key(conn: &rusqlite::Connection) -> anyhow::Result<String> {
    use chronicle_core::storage;
    use std::io::Read;
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    if let Some(key) = storage::get_meta(conn, "wakapi_api_key")?
        && !key.is_empty()
    {
        return Ok(key);
    }
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .context("reading /dev/urandom for the heartbeat api key")?;
    let key: String = bytes
        .iter()
        .map(|b| char::from(ALPHABET[usize::from(*b) % ALPHABET.len()]))
        .collect();
    storage::set_meta(conn, "wakapi_api_key", Some(&key))?;
    Ok(key)
}

pub(crate) fn spawn_ai_job_worker(job_id: i64) -> std::io::Result<Child> {
    Command::new(own_exe()?)
        .args(["ai-job", "--id", &job_id.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Derive when AFK ≥ `derive_idle_secs`, or the 1-min load is low enough
/// that inference won't be noticed.
/// Spans embedded per tick; new focus spans arrive a few a minute, and a
/// backlog after enabling the model drains a few hundred a minute.
const EMBED_PER_TICK: usize = 200;
/// Embed the focus spans that have no vector yet (m30 chunk 6), when an
/// embedding model is configured and loads.
fn embed_new_spans(
    conn: &mut rusqlite::Connection,
    config: &Config,
    data_dir: &Path,
    embedder: &mut Option<Option<chronicle_derive::embed::Embedder>>,
) {
    use chronicle_core::storage;
    let Some(path) =
        chronicle_derive::model::resolve_embed(config.embed_model.as_deref(), data_dir)
    else {
        return;
    };
    let Some(e) = load_embedder(&path, embedder) else {
        return;
    };
    let pending = match storage::spans_missing_embeddings(conn, EMBED_PER_TICK) {
        Ok(p) => p,
        Err(err) => {
            tracing::error!("embedding query failed: {err}");
            return;
        }
    };
    if pending.is_empty() {
        return;
    }
    let texts: Vec<String> = pending
        .iter()
        .map(|(_, app, title)| format!("{app}: {title}"))
        .collect();
    let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
    match e.embed(&refs) {
        Ok(vecs) => {
            let rows: Vec<(i64, Vec<f32>)> = pending.iter().map(|(id, ..)| *id).zip(vecs).collect();
            if let Err(err) = storage::store_span_embeddings(conn, &rows) {
                tracing::error!("storing embeddings failed: {err}");
            } else {
                tracing::debug!(n = rows.len(), "spans embedded");
            }
        }
        Err(err) => tracing::error!("embedding failed: {err}"),
    }
}

/// The tick's one embedding model, loaded on first use; a failed load is
/// remembered so the tick does not retry every 30 s.
fn load_embedder<'a>(
    path: &Path,
    cache: &'a mut Option<Option<chronicle_derive::embed::Embedder>>,
) -> Option<&'a chronicle_derive::embed::Embedder> {
    cache
        .get_or_insert_with(|| match chronicle_derive::embed::Embedder::load(path) {
            Ok(e) => Some(e),
            Err(err) => {
                tracing::error!("embedding model failed to load: {err}");
                None
            }
        })
        .as_ref()
}

/// Rows per tick for the example memory (m36 chunk 2): a rename or a
/// correction is one row, so the backlog is the first tick's only.
const EXAMPLES_PER_TICK: usize = 50;
/// Embed task labels that changed and corrections that are new (m36 chunk
/// 2), with the configured model or the downloaded preset.
fn embed_new_examples(
    conn: &mut rusqlite::Connection,
    config: &Config,
    data_dir: &Path,
    embedder: &mut Option<Option<chronicle_derive::embed::Embedder>>,
) {
    use chronicle_core::storage;
    let Some(path) =
        chronicle_derive::model::resolve_embed_or_default(config.embed_model.as_deref(), data_dir)
    else {
        return;
    };
    let (labels, corrections) = match (
        storage::tasks_missing_label_embeddings(conn, EXAMPLES_PER_TICK),
        storage::corrections_missing_embeddings(conn, EXAMPLES_PER_TICK),
    ) {
        (Ok(l), Ok(c)) => (l, c),
        (Err(err), _) | (_, Err(err)) => {
            tracing::error!("example embedding query failed: {err}");
            return;
        }
    };
    if labels.is_empty() && corrections.is_empty() {
        return;
    }
    let Some(e) = load_embedder(&path, embedder) else {
        return;
    };
    let t0 = Instant::now();
    if !labels.is_empty() {
        let texts: Vec<String> = labels
            .iter()
            .map(|(_, label, project)| match project {
                Some(p) => format!("{label} [{p}]"),
                None => label.clone(),
            })
            .collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        match e.embed(&refs) {
            Ok(vecs) => {
                let rows: Vec<(i64, String, Vec<f32>)> = labels
                    .iter()
                    .zip(vecs)
                    .map(|((id, label, _), v)| (*id, label.clone(), v))
                    .collect();
                if let Err(err) = storage::store_task_label_embeddings(
                    conn,
                    jiff::Timestamp::now().as_millisecond(),
                    &rows,
                ) {
                    tracing::error!("storing label embeddings failed: {err}");
                }
            }
            Err(err) => tracing::error!("label embedding failed: {err}"),
        }
    }
    if !corrections.is_empty() {
        let refs: Vec<&str> = corrections.iter().map(|(_, t)| t.as_str()).collect();
        match e.embed(&refs) {
            Ok(vecs) => {
                let rows: Vec<(i64, Vec<f32>)> =
                    corrections.iter().map(|(id, _)| *id).zip(vecs).collect();
                if let Err(err) = storage::store_correction_embeddings(conn, &rows) {
                    tracing::error!("storing correction embeddings failed: {err}");
                }
            }
            Err(err) => tracing::error!("correction embedding failed: {err}"),
        }
    }
    let n = labels.len() + corrections.len();
    tracing::debug!(
        labels = labels.len(),
        corrections = corrections.len(),
        ms_per_row = t0.elapsed().as_millis() as f64 / n.max(1) as f64,
        "examples embedded"
    );
}

/// The segmenter's batch tier (m30 chunk 3): every batch the model would
/// have derived is re-scored with the day's profiles instead, in-process,
/// no idle gate — it is a few milliseconds of SQL and arithmetic. A few
/// per tick so a backlog after a long pause drains without stalling the
/// loop; a failure leaves the batch eligible, so it logs and stops.
const RECONCILE_PER_TICK: usize = 3;
fn reconcile_due(conn: &mut rusqlite::Connection, config: &Config, distractions: &[regex::Regex]) {
    use chronicle_core::storage;
    for _ in 0..RECONCILE_PER_TICK {
        let batch_id = match storage::next_eligible_batch(conn) {
            Ok(Some(id)) => id,
            Ok(None) => return,
            Err(e) => {
                tracing::error!("eligible-batch query failed: {e}");
                return;
            }
        };
        match chronicle_core::segmenter::reconcile(
            conn,
            config,
            batch_id,
            Timestamp::now(),
            distractions,
        ) {
            Ok(n) => tracing::info!(batch_id, segments = n, "batch reconciled"),
            Err(e) => {
                tracing::error!(batch_id, "reconcile failed: {e}");
                return;
            }
        }
    }
}

/// Advisor questions a day (m36 chunk 3): the online cap; the nightly
/// pass has none.
const ADVISE_PER_DAY: usize = 60;
/// The online advisor only looks at today (m36 chunk 4): yesterday is the
/// night pass's, and the online pass never rewrites it.
fn advise_since_ms() -> i64 {
    crate::ai_job::day_start_ms()
}
/// Queue an `advise` job per unsure reconciled verdict with a runner-up
/// (m36 chunk 3), up to the day's cap; the row is marked pending so the
/// next tick does not queue it twice. Called only while `advise` has a
/// cloud route: the local model is never asked.
fn advise_due(conn: &rusqlite::Connection) {
    use chronicle_core::storage;
    let used = storage::get_meta(conn, &day_counter_key("advise"))
        .ok()
        .flatten()
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(0);
    if used >= ADVISE_PER_DAY {
        return;
    }
    let now = Timestamp::now();
    let due = match storage::unadvised_verdicts(conn, advise_since_ms(), ADVISE_PER_DAY - used) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!("unadvised verdict query failed: {e}");
            return;
        }
    };
    let mut queued = 0;
    for (verdict_id, _) in due {
        let payload = serde_json::json!({ "verdict_id": verdict_id }).to_string();
        if let Err(e) = storage::mark_verdict_advice(conn, verdict_id, "pending")
            .and_then(|_| storage::enqueue_ai_job(conn, now, "advise", 0, &payload))
        {
            tracing::error!(verdict_id, "advise enqueue failed: {e}");
            break;
        }
        queued += 1;
    }
    bump_day_counter_by(conn, "advise", queued);
    if queued > 0 {
        tracing::info!(queued, "advisor questions queued");
    }
}

pub(crate) fn derive_gates_open(config: &Config, idle_since: Option<i64>) -> bool {
    let idle_long_enough = idle_since.is_some_and(|since| {
        Timestamp::now().as_millisecond() - since >= i64::from(config.derive_idle_secs) * 1000
    });
    idle_long_enough || load_1min().is_some_and(|l| l < LOW_LOAD)
}

pub(crate) fn load_1min() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/loadavg").ok()?;
    s.split_whitespace().next()?.parse().ok()
}

/// Defer derivation below 30% on battery power. No battery = never defers.
pub(crate) fn on_low_battery() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_battery =
            std::fs::read_to_string(path.join("type")).is_ok_and(|t| t.trim() == "Battery");
        if !is_battery {
            continue;
        }
        let discharging =
            std::fs::read_to_string(path.join("status")).is_ok_and(|s| s.trim() == "Discharging");
        let low = std::fs::read_to_string(path.join("capacity"))
            .ok()
            .and_then(|c| c.trim().parse::<u32>().ok())
            .is_some_and(|c| c < BATTERY_DEFER_PCT);
        if discharging && low {
            return true;
        }
    }
    false
}

// 15 s tick + 10 s UI wake bounds timeline staleness at ~25 s worst case
// (was 60+30 ≈ 90 s); the tail rewrite is a handful of rows, cost negligible.
/// Link files and editor workspace lists are re-read this often (m37).
const SOURCES_EVERY: Duration = Duration::from_secs(60 * 60);
pub(crate) const SESSIONIZE_EVERY: Duration = Duration::from_secs(15);

/// What the repos' link files and the editors' recent lists say, refreshed
/// hourly (m37 chunks 2 and 3): `repo_links` per project path and
/// `editor_workspaces` from VS Code, JetBrains and Zed. Names, ids and
/// paths only.
fn refresh_sources(conn: &mut rusqlite::Connection, config: &Config, now: Timestamp) {
    let now_ms = now.as_millisecond();
    let matcher = chronicle_core::project::Matcher::from_config(config);
    for path in matcher.projects.iter().flat_map(|p| p.paths.iter()) {
        let links: Vec<chronicle_core::storage::RepoLinkRow> =
            chronicle_core::links::read_links(path)
                .into_iter()
                .map(|l| chronicle_core::storage::RepoLinkRow {
                    repo: path.display().to_string(),
                    kind: l.kind.to_owned(),
                    name: l.name,
                    url_pattern: l.url_pattern,
                })
                .collect();
        if let Err(e) = chronicle_core::storage::replace_repo_links(
            conn,
            &path.display().to_string(),
            &links,
            now_ms,
        ) {
            tracing::error!("repo links refresh failed: {e}");
        }
    }
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let rows: Vec<chronicle_core::storage::WorkspaceRow> =
        chronicle_capture::workspaces::read_all(&home)
            .into_iter()
            .map(|w| chronicle_core::storage::WorkspaceRow {
                editor: w.editor.to_owned(),
                path: w.path,
                remote: w.remote.unwrap_or_default(),
                last_ts: w.last_ts_ms,
            })
            .collect();
    if let Err(e) = chronicle_core::storage::upsert_workspaces(conn, &rows, now_ms) {
        tracing::error!("editor workspaces refresh failed: {e}");
    }
}
