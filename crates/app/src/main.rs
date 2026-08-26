mod ui;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use chronicle_core::config::Config;
use chronicle_core::types::CaptureEvent;
use clap::{Parser, Subcommand};
use crossbeam_channel::Sender;
use jiff::{Timestamp, ToSpan, Zoned, civil, tz::TimeZone};
use regex::Regex;

#[derive(Parser)]
#[command(name = "chronicle", version, about = "Local-first activity tracker")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon (default).
    Run,
    /// Internal: egui window process, spawned by the daemon.
    #[command(hide = true)]
    Ui,
    /// Internal: ephemeral derivation worker.
    #[command(hide = true)]
    Derive {
        #[arg(long)]
        batch: i64,
    },
    /// Internal: warm chat inference worker.
    #[command(hide = true)]
    ChatWorker,
    /// Show or hide the UI of the running daemon.
    Toggle,
    /// Print stored data, optionally for one local civil day.
    Dump {
        #[arg(long, value_name = "YYYY-MM-DD")]
        day: Option<String>,
    },
    /// Manage local LLM models.
    Model {
        #[command(subcommand)]
        cmd: ModelCmd,
    },
    /// Internal: benchmark downloaded models on fixtures and/or real batches.
    #[command(hide = true)]
    Bench {
        /// Directory of fixture JSONL event streams.
        #[arg(long, default_value = "fixtures")]
        fixtures: PathBuf,
        /// Real batch ids from the live DB (repeatable).
        #[arg(long)]
        batch: Vec<i64>,
        /// Print each case's digest instead of running inference.
        #[arg(long)]
        digest: bool,
    },
}

#[derive(Subcommand)]
enum ModelCmd {
    /// Download a model preset (resumable, SHA-256 verified).
    Pull {
        /// Preset name; defaults to qwen3-4b.
        preset: Option<String>,
    },
    /// List presets and their download state.
    List,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let data_dir = chronicle_core::data_dir().context("could not resolve a data directory")?;
    match cli.cmd.unwrap_or(Cmd::Run) {
        Cmd::Run => run(&data_dir),
        Cmd::Dump { day } => dump(&data_dir, day.as_deref()),
        Cmd::Ui => ui::run(&data_dir),
        Cmd::Toggle => {
            if send_ctrl(&socket_path(&data_dir), "toggle") {
                Ok(())
            } else {
                bail!("chronicle daemon is not running")
            }
        }
        Cmd::Derive { batch } => derive_worker(&data_dir, batch),
        Cmd::Model { cmd } => model_cmd(&data_dir, cmd),
        Cmd::Bench {
            fixtures,
            batch,
            digest,
        } => bench(&data_dir, &fixtures, &batch, digest),
        Cmd::ChatWorker => bail!("chat lands in M7"),
    }
}

fn model_cmd(data_dir: &Path, cmd: ModelCmd) -> anyhow::Result<()> {
    use chronicle_derive::model;
    match cmd {
        ModelCmd::Pull { preset } => {
            let name = preset.as_deref().unwrap_or(model::default_preset().name);
            let Some(spec) = model::preset(name) else {
                bail!(
                    "unknown preset {name:?}; available: {}",
                    model::PRESETS
                        .iter()
                        .map(|p| p.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            };
            println!("pulling {} ({}/{})", spec.name, spec.repo, spec.file);
            let mut last_pct = u64::MAX;
            let path = model::pull(data_dir, spec, &mut |done, total| {
                let pct = done * 100 / total.max(1);
                if pct != last_pct {
                    last_pct = pct;
                    print!("\r{pct:3}% of {} MiB", total >> 20);
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                }
            })?;
            println!("\rverified {} ", path.display());
            Ok(())
        }
        ModelCmd::List => {
            let config = Config::load(&data_dir.join("config.toml"))?;
            for spec in model::PRESETS {
                let path = model::models_dir(data_dir).join(spec.file);
                let state = if path.exists() {
                    "downloaded"
                } else {
                    "not downloaded"
                };
                println!("{:12} {state}  {}", spec.name, path.display());
            }
            match model::resolve(config.model_path.as_deref(), data_dir) {
                Some(p) => println!("active: {}", p.display()),
                None => println!("active: none (run `chronicle model pull`)"),
            }
            Ok(())
        }
    }
}

/// M4 benchmark gate: run every downloaded preset over fixture streams and
/// real batches, print tasks + timing side by side. Judgment stays human.
fn bench(
    data_dir: &Path,
    fixtures: &Path,
    batch_ids: &[i64],
    digest_only: bool,
) -> anyhow::Result<()> {
    use chronicle_core::{digest, sessionizer, storage, types::Event};

    let config = Config::load(&data_dir.join("config.toml"))?;
    let mut cases: Vec<(String, String)> = Vec::new(); // (name, digest)

    if fixtures.is_dir() {
        let mut paths: Vec<_> = std::fs::read_dir(fixtures)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        paths.sort();
        for path in paths {
            let text = std::fs::read_to_string(&path)?;
            let events = text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(serde_json::from_str::<Event>)
                .collect::<Result<Vec<_>, _>>()
                .with_context(|| format!("parsing {}", path.display()))?;
            let Some(end) = events.last().map(|e| e.ts) else {
                continue;
            };
            let spans = sessionizer::sessionize(&events, end, &config);
            let name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            cases.push((
                format!("fixture:{name}"),
                digest::build_digest(&spans, &jiff::tz::TimeZone::UTC, &[], &[]),
            ));
        }
    }

    if !batch_ids.is_empty() {
        let conn = storage::open(&data_dir.join("chronicle.db"))?;
        let tz = TimeZone::system();
        for &id in batch_ids {
            let spans = storage::batch_spans(&conn, id)?;
            let Some(first) = spans.first() else {
                println!("batch {id}: no spans, skipping");
                continue;
            };
            let recent = storage::recent_labels_before(&conn, first.start.as_millisecond(), 3)?;
            let corrections = storage::similar_corrections(&conn, &spans, 4)?;
            cases.push((
                format!("batch:{id}"),
                digest::build_digest(&spans, &tz, &recent, &corrections),
            ));
        }
    }
    if cases.is_empty() {
        bail!("nothing to bench: no fixtures found and no --batch given");
    }
    if digest_only {
        for (case, digest_text) in &cases {
            println!(
                "\n=== {case} (digest ~{} tokens)\n{digest_text}",
                digest::approx_tokens(digest_text)
            );
        }
        return Ok(());
    }

    let models: Vec<_> = chronicle_derive::model::PRESETS
        .iter()
        .map(|p| {
            (
                p.name,
                chronicle_derive::model::models_dir(data_dir).join(p.file),
            )
        })
        .filter(|(_, path)| path.exists())
        .collect();
    if models.is_empty() {
        bail!("no models downloaded; run `chronicle model pull`");
    }

    for (case, digest_text) in &cases {
        println!(
            "\n=== {case} (digest ~{} tokens)",
            digest::approx_tokens(digest_text)
        );
        for (name, path) in &models {
            let t0 = Instant::now();
            match chronicle_derive::infer_tasks(path, digest_text) {
                Ok(tasks) => {
                    println!(
                        "--- {name}: {} tasks in {:.1}s",
                        tasks.len(),
                        t0.elapsed().as_secs_f64()
                    );
                    for t in tasks {
                        let project = t.project.as_deref().unwrap_or("-");
                        println!(
                            "  {:>4}–{:<4} {:.2}  {}  [{project}]",
                            t.start_offset_min, t.end_offset_min, t.confidence, t.label
                        );
                    }
                }
                Err(e) => println!(
                    "--- {name}: FAILED in {:.1}s: {e:#}",
                    t0.elapsed().as_secs_f64()
                ),
            }
        }
    }
    Ok(())
}

/// Ephemeral derivation worker: claim batch → digest → infer → write tasks →
/// exit. Any failure marks the batch failed (retry-once via attempts cap).
fn derive_worker(data_dir: &Path, batch_id: i64) -> anyhow::Result<()> {
    use chronicle_core::storage;
    let _guard = init_logging(data_dir)?;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let mut conn = storage::open(&data_dir.join("chronicle.db"))?;
    let Some(model_path) = chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir)
    else {
        bail!("no model available; run `chronicle model pull`")
    };
    let Some(batch) = storage::claim_batch(&conn, batch_id)? else {
        bail!("batch {batch_id} is not eligible for derivation")
    };
    let result = (|| -> anyhow::Result<usize> {
        let spans = storage::batch_spans(&conn, batch_id)?;
        let recent = storage::recent_labels_before(&conn, batch.start_ts, 3)?;
        let corrections = storage::similar_corrections(&conn, &spans, 4)?;
        let tz = TimeZone::system();
        let digest = chronicle_core::digest::build_digest(&spans, &tz, &recent, &corrections);
        let drafts = chronicle_derive::infer_tasks(&model_path, &digest)?;
        let tasks = clamp_tasks(drafts, &spans, batch.start_ts, batch.end_ts);
        let n = tasks.len();
        storage::store_tasks(&mut conn, batch_id, &tasks)?;
        Ok(n)
    })();
    match result {
        Ok(n) => {
            tracing::info!(batch_id, tasks = n, "derivation done");
            Ok(())
        }
        Err(e) => {
            tracing::error!(batch_id, "derivation failed: {e:#}");
            storage::fail_batch(&conn, batch_id)?;
            Err(e)
        }
    }
}

/// Offsets are minutes from batch start, untrusted model output: clamp into
/// the batch window, drop empty/inverted tasks, and split any task the model
/// stretched across a long AFK gap (small models ignore the prompt rule).
fn clamp_tasks(
    drafts: Vec<chronicle_derive::TaskDraft>,
    spans: &[chronicle_core::sessionizer::SpanDraft],
    start_ms: i64,
    end_ms: i64,
) -> Vec<chronicle_core::types::NewTask> {
    use chronicle_core::sessionizer::SpanKind;
    use chronicle_core::types::{NewTask, ms_to_ts};
    const AFK_SPLIT_MS: i64 = 5 * 60_000;
    const MIN_PIECE_MS: i64 = 60_000;
    let gaps: Vec<(i64, i64)> = spans
        .iter()
        .filter(|s| s.kind == SpanKind::Afk && s.duration_ms() >= AFK_SPLIT_MS)
        .map(|s| (s.start.as_millisecond(), s.end.as_millisecond()))
        .collect();
    let mut out = Vec::new();
    for d in drafts {
        let s = (start_ms + d.start_offset_min * 60_000).clamp(start_ms, end_ms);
        let e = (start_ms + d.end_offset_min * 60_000).clamp(start_ms, end_ms);
        let label = d.label.trim();
        if label.is_empty() || e <= s {
            continue;
        }
        let mut pieces = Vec::new();
        let mut cur = s;
        for &(gap_start, gap_end) in &gaps {
            if gap_end <= cur || gap_start >= e {
                continue;
            }
            if gap_start > cur {
                pieces.push((cur, gap_start));
            }
            cur = gap_end.max(cur);
        }
        if cur < e {
            pieces.push((cur, e));
        }
        for (piece_start, piece_end) in pieces {
            if piece_end - piece_start < MIN_PIECE_MS {
                continue;
            }
            out.push(NewTask {
                label: label.to_string(),
                project: d.project.clone().filter(|p| !p.trim().is_empty()),
                start_ts: ms_to_ts(piece_start),
                end_ts: ms_to_ts(piece_end),
                confidence: d.confidence.clamp(0.0, 1.0),
            });
        }
    }
    // Tasks must not overlap; when the model overlaps anyway, the earlier
    // start (higher confidence on ties) wins and the later task is trimmed.
    out.sort_by(|a, b| {
        a.start_ts
            .cmp(&b.start_ts)
            .then(b.confidence.total_cmp(&a.confidence))
    });
    let mut last_end = None;
    out.retain_mut(|t| {
        if let Some(le) = last_end
            && t.start_ts < le
        {
            if t.end_ts <= le {
                return false;
            }
            t.start_ts = le;
        }
        last_end = Some(t.end_ts);
        true
    });
    out
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

enum CtrlMsg {
    Toggle,
    DeriveNow,
}

fn spawn_ctrl_listener(
    listener: std::os::unix::net::UnixListener,
    tx: Sender<CtrlMsg>,
) -> anyhow::Result<()> {
    use std::io::BufRead;
    std::thread::Builder::new()
        .name("ctrl".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let mut line = String::new();
                if std::io::BufReader::new(stream)
                    .read_line(&mut line)
                    .is_err()
                {
                    continue;
                }
                let msg = match line.trim() {
                    "toggle" => CtrlMsg::Toggle,
                    "derive" => CtrlMsg::DeriveNow,
                    _ => continue,
                };
                if tx.send(msg).is_err() {
                    return;
                }
            }
        })?;
    Ok(())
}

fn toggle_ui(slot: &mut Option<Child>) {
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
fn own_exe() -> std::io::Result<PathBuf> {
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

fn spawn_ui_child() -> std::io::Result<Child> {
    Command::new(own_exe()?)
        .arg("ui")
        .stdin(Stdio::piped())
        .spawn()
}

fn reap_ui(slot: &mut Option<Child>) {
    if let Some(child) = slot
        && matches!(child.try_wait(), Ok(Some(_)))
    {
        *slot = None;
    }
}

fn run(data_dir: &Path) -> anyhow::Result<()> {
    // Single instance: a second `chronicle` just toggles the running daemon.
    let sock = socket_path(data_dir);
    if send_ctrl(&sock, "toggle") {
        println!("chronicle daemon already running \u{2014} toggled UI");
        return Ok(());
    }
    let _ = std::fs::remove_file(&sock); // stale socket from an unclean exit
    let listener = std::os::unix::net::UnixListener::bind(&sock)
        .with_context(|| format!("failed to bind {}", sock.display()))?;

    let _guard = init_logging(data_dir)?;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let filters = Filters::new(&config)?;
    let mut conn = chronicle_core::storage::open(&data_dir.join("chronicle.db"))?;
    // Daemon downtime must not read as focus time: mark a gap as AFK-from-the-
    // last-event; the AFK poller's initial state announcement closes it.
    if let Some(last_ms) = chronicle_core::storage::latest_event_ts(&conn)?
        && Timestamp::now().as_millisecond() - last_ms > GAP_MARKER_SECS * 1000
    {
        let ts = chronicle_core::types::ms_to_ts(last_ms + 1);
        chronicle_core::storage::insert_event(&conn, &CaptureEvent::Afk { idle: true, ts })?;
    }
    // A `running` batch with no live worker (unclean daemon exit) burns its
    // attempt and falls back to `failed` so the retry cap still holds.
    chronicle_core::storage::reset_stale_running(&conn)?;
    let (tx, rx) = crossbeam_channel::unbounded();
    let (ctrl_tx, ctrl_rx) = crossbeam_channel::unbounded();
    spawn_ctrl_listener(listener, ctrl_tx)?;
    spawn_capture(&config, tx.clone())?;
    // Port taken (a real aw-server?) must not kill capture: log, warn in UI.
    let server_error = match chronicle_server::spawn(&config, tx) {
        Ok(()) => None,
        Err(e) => {
            tracing::error!("AW endpoint failed on 127.0.0.1:{}: {e}", config.port);
            Some(format!("AW endpoint failed on port {}: {e}", config.port))
        }
    };
    chronicle_core::storage::set_meta(&conn, "server_error", server_error.as_deref())?;
    tracing::info!(?data_dir, "chronicle daemon running");
    let mut ui_child: Option<Child> = None;
    let mut scheduler = Scheduler { worker: None };
    let mut idle_since: Option<i64> = None;
    let mut next_refresh = Instant::now() + SESSIONIZE_EVERY;
    loop {
        let timeout = next_refresh.saturating_duration_since(Instant::now());
        crossbeam_channel::select! {
            recv(rx) -> event => {
                let Ok(event) = event else { break };
                if filters.excluded(&event) {
                    continue;
                }
                if let CaptureEvent::Afk { idle, ts } = &event {
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
                    Ok(CtrlMsg::Toggle) => toggle_ui(&mut ui_child),
                    Ok(CtrlMsg::DeriveNow) => scheduler.tick(&conn, &config, data_dir, idle_since, true),
                    Err(_) => {}
                }
            }
            default(timeout) => {
                let now = Timestamp::now();
                if let Err(e) = chronicle_core::sessionizer::refresh(&mut conn, &config, now) {
                    tracing::error!("sessionize refresh failed: {e}");
                }
                scheduler.tick(&conn, &config, data_dir, idle_since, false);
                reap_ui(&mut ui_child);
                next_refresh = Instant::now() + SESSIONIZE_EVERY;
            }
        }
    }
    bail!("capture threads exited")
}

const DERIVE_TIMEOUT: Duration = Duration::from_secs(300);
/// "Low 1-min load" gate for deriving while the user is active.
const LOW_LOAD: f64 = 1.0;
const BATTERY_DEFER_PCT: u32 = 30;

struct Scheduler {
    /// At most one derive worker at a time: (child, started, batch id).
    worker: Option<(Child, Instant, i64)>,
}

impl Scheduler {
    fn tick(
        &mut self,
        conn: &rusqlite::Connection,
        config: &Config,
        data_dir: &Path,
        idle_since: Option<i64>,
        force: bool,
    ) {
        use chronicle_core::storage;
        if let Some((child, started, batch_id)) = &mut self.worker {
            let batch_id = *batch_id;
            match child.try_wait() {
                Ok(Some(status)) => {
                    // A worker that died before reporting leaves the batch
                    // `running`; count that as the failed attempt it was.
                    let stuck = matches!(
                        storage::batch_status(conn, batch_id),
                        Ok(Some(ref s)) if s == "running"
                    );
                    if stuck {
                        let _ = storage::fail_batch(conn, batch_id);
                    }
                    if !status.success() {
                        tracing::warn!(batch_id, %status, "derive worker failed");
                    }
                    self.worker = None;
                }
                Ok(None) => {
                    if started.elapsed() >= DERIVE_TIMEOUT {
                        tracing::warn!(batch_id, "derive worker timed out; killing");
                        let _ = child.kill();
                        let _ = child.wait();
                        let _ = storage::fail_batch(conn, batch_id);
                        self.worker = None;
                    }
                    return; // one worker at a time
                }
                Err(e) => {
                    tracing::error!(batch_id, "derive worker wait failed: {e}");
                    self.worker = None;
                }
            }
        }
        if !force && !derive_gates_open(config, idle_since) {
            return;
        }
        if on_low_battery() {
            tracing::debug!("derivation deferred: battery low");
            return;
        }
        if chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir).is_none() {
            tracing::debug!("derivation skipped: no model downloaded");
            return;
        }
        let batch_id = match storage::next_eligible_batch(conn) {
            Ok(Some(id)) => id,
            Ok(None) => return,
            Err(e) => {
                tracing::error!("eligible-batch query failed: {e}");
                return;
            }
        };
        match spawn_derive_worker(batch_id) {
            Ok(child) => {
                tracing::info!(batch_id, "derive worker spawned");
                self.worker = Some((child, Instant::now(), batch_id));
            }
            Err(e) => tracing::error!(batch_id, "failed to spawn derive worker: {e}"),
        }
    }
}

fn spawn_derive_worker(batch_id: i64) -> std::io::Result<Child> {
    // Worker logging goes to the log file; keep the daemon terminal clean.
    Command::new(own_exe()?)
        .args(["derive", "--batch", &batch_id.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Derive when AFK ≥ `derive_idle_secs`, or the 1-min load is low enough
/// that inference won't be noticed.
fn derive_gates_open(config: &Config, idle_since: Option<i64>) -> bool {
    let idle_long_enough = idle_since.is_some_and(|since| {
        Timestamp::now().as_millisecond() - since >= i64::from(config.derive_idle_secs) * 1000
    });
    idle_long_enough || load_1min().is_some_and(|l| l < LOW_LOAD)
}

fn load_1min() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/loadavg").ok()?;
    s.split_whitespace().next()?.parse().ok()
}

/// Defer derivation below 30% on battery power. No battery = never defers.
fn on_low_battery() -> bool {
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

const SESSIONIZE_EVERY: Duration = Duration::from_secs(60);

#[cfg(target_os = "linux")]
fn spawn_capture(config: &Config, tx: Sender<CaptureEvent>) -> anyhow::Result<()> {
    use chronicle_capture::FocusProvider;
    use chronicle_capture::x11::{X11AfkProvider, X11FocusProvider};

    let focus = X11FocusProvider::new().map_err(|e| anyhow::anyhow!("X11 focus provider: {e}"))?;
    let ftx = tx.clone();
    std::thread::Builder::new()
        .name("focus".into())
        .spawn(move || {
            if let Err(e) = focus.run(ftx) {
                tracing::error!("focus provider exited: {e}");
            }
        })?;

    let afk = X11AfkProvider::new().map_err(|e| anyhow::anyhow!("X11 afk provider: {e}"))?;
    let threshold_ms = u64::from(config.afk_close_secs) * 1000;
    std::thread::Builder::new()
        .name("afk".into())
        .spawn(move || afk_loop(afk, tx, threshold_ms))?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn spawn_capture(_config: &Config, _tx: Sender<CaptureEvent>) -> anyhow::Result<()> {
    bail!("capture on this platform lands in M9/M10")
}

const AFK_POLL: Duration = Duration::from_secs(30);
const GAP_MARKER_SECS: i64 = 60;

fn afk_loop(afk: impl chronicle_capture::AfkProvider, tx: Sender<CaptureEvent>, threshold_ms: u64) {
    // Announce the starting state so a dangling AFK span (gap marker, or a
    // restart while idle) gets closed.
    let mut was_idle = match afk.idle_ms() {
        Ok(ms) => {
            let idle = ms >= threshold_ms;
            if tx.send(afk_event(idle, ms)).is_err() {
                return;
            }
            idle
        }
        Err(e) => {
            tracing::warn!("afk poll failed: {e}");
            false
        }
    };
    loop {
        std::thread::sleep(AFK_POLL);
        let ms = match afk.idle_ms() {
            Ok(ms) => ms,
            Err(e) => {
                tracing::warn!("afk poll failed: {e}");
                continue;
            }
        };
        let idle = ms >= threshold_ms;
        if idle == was_idle {
            continue;
        }
        was_idle = idle;
        if tx.send(afk_event(idle, ms)).is_err() {
            return;
        }
    }
}

/// Idle transitions are backdated to when input actually stopped.
fn afk_event(idle: bool, idle_ms: u64) -> CaptureEvent {
    let now = Timestamp::now();
    let ts = if idle {
        now.checked_sub((idle_ms as i64).milliseconds())
            .unwrap_or(now)
    } else {
        now
    };
    CaptureEvent::Afk { idle, ts }
}

struct Filters {
    apps: Vec<Regex>,
    titles: Vec<Regex>,
}

impl Filters {
    fn new(config: &Config) -> anyhow::Result<Self> {
        let compile = |patterns: &[String]| -> anyhow::Result<Vec<Regex>> {
            patterns
                .iter()
                .map(|p| Regex::new(p).with_context(|| format!("bad exclusion regex {p:?}")))
                .collect()
        };
        Ok(Self {
            apps: compile(&config.excluded_apps)?,
            titles: compile(&config.excluded_titles)?,
        })
    }

    /// Excluded events are dropped before storage — never written at all.
    fn excluded(&self, event: &CaptureEvent) -> bool {
        let (app, title, url) = match event {
            CaptureEvent::Focus(e) | CaptureEvent::TitleChanged(e) => (&e.app, &e.title, None),
            CaptureEvent::Url(e) => (&e.app, &e.title, Some(&e.url)),
            CaptureEvent::Afk { .. } => return false,
        };
        self.apps.iter().any(|r| r.is_match(app))
            || self
                .titles
                .iter()
                .any(|r| r.is_match(title) || url.is_some_and(|u| r.is_match(u)))
    }
}

fn init_logging(data_dir: &Path) -> anyhow::Result<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt};
    let file = tracing_appender::rolling::daily(data_dir.join("logs"), "chronicle.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file);
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(fmt::layer().with_ansi(false).with_writer(file_writer))
        .init();
    Ok(guard)
}

fn dump(data_dir: &Path, day: Option<&str>) -> anyhow::Result<()> {
    let conn = chronicle_core::storage::open(&data_dir.join("chronicle.db"))?;

    let range = match day {
        Some(day) => {
            let date: civil::Date = day.parse().with_context(|| format!("bad --day {day:?}"))?;
            let start = date.to_zoned(TimeZone::system())?;
            let end = start.checked_add(1.day())?;
            println!(
                "chronicle dump — {date} ({tz})",
                tz = start.time_zone().iana_name().unwrap_or("local")
            );
            Some((
                start.timestamp().as_millisecond(),
                end.timestamp().as_millisecond(),
            ))
        }
        None => {
            println!("chronicle dump — all data");
            None
        }
    };
    let (lo, hi) = range.unwrap_or((i64::MIN, i64::MAX));

    let count =
        |sql: &str| -> anyhow::Result<i64> { Ok(conn.query_row(sql, [lo, hi], |r| r.get(0))?) };
    let events = count("SELECT COUNT(*) FROM events WHERE ts >= ?1 AND ts < ?2")?;
    let spans = count("SELECT COUNT(*) FROM spans WHERE start_ts >= ?1 AND start_ts < ?2")?;
    let batches = count("SELECT COUNT(*) FROM batches WHERE start_ts >= ?1 AND start_ts < ?2")?;
    let tasks = count("SELECT COUNT(*) FROM tasks WHERE start_ts >= ?1 AND start_ts < ?2")?;
    println!("events: {events}  spans: {spans}  batches: {batches}  tasks: {tasks}");

    let mut stmt = conn.prepare(
        "SELECT ts, kind, app, title, idle, url FROM events
         WHERE ts >= ?1 AND ts < ?2 ORDER BY ts, id",
    )?;
    let mut rows = stmt.query([lo, hi])?;
    while let Some(row) = rows.next()? {
        let ts: i64 = row.get(0)?;
        let (kind, app, title): (String, String, String) = (row.get(1)?, row.get(2)?, row.get(3)?);
        let idle: Option<i64> = row.get(4)?;
        let url: Option<String> = row.get(5)?;
        let t = local(ts)?;
        match kind.as_str() {
            "afk" => println!(
                "{}  [afk] idle={}",
                t.strftime("%H:%M:%S"),
                idle.unwrap_or(0) == 1
            ),
            "url" => println!(
                "{}  [url] {app}: {title} <{}>",
                t.strftime("%H:%M:%S"),
                url.as_deref().unwrap_or("")
            ),
            _ => println!("{}  [{kind}] {app}: {title}", t.strftime("%H:%M:%S")),
        }
    }

    let mut stmt = conn.prepare(
        "SELECT start_ts, end_ts, app, title, kind, url FROM spans
         WHERE start_ts >= ?1 AND start_ts < ?2 ORDER BY start_ts, id",
    )?;
    let mut rows = stmt.query([lo, hi])?;
    while let Some(row) = rows.next()? {
        let (start, end): (i64, i64) = (row.get(0)?, row.get(1)?);
        let (app, title, kind): (String, String, String) = (row.get(2)?, row.get(3)?, row.get(4)?);
        let url: Option<String> = row.get(5)?;
        let start = local(start)?;
        let end = local(end)?;
        let label = match (kind.as_str(), url) {
            ("focus", Some(url)) => {
                format!(
                    " {app}: {title} <{}>",
                    chronicle_core::sessionizer::domain(&url)
                )
            }
            ("focus", None) => format!(" {app}: {title}"),
            _ => String::new(),
        };
        println!(
            "{} – {}  [{kind}]{label}",
            start.strftime("%H:%M:%S"),
            end.strftime("%H:%M:%S"),
        );
    }

    let mut stmt = conn.prepare(
        "SELECT start_ts, end_ts, label, project, confidence FROM tasks
         WHERE start_ts >= ?1 AND start_ts < ?2 ORDER BY start_ts, id",
    )?;
    let mut rows = stmt.query([lo, hi])?;
    while let Some(row) = rows.next()? {
        let (start, end): (i64, i64) = (row.get(0)?, row.get(1)?);
        let (label, project): (String, Option<String>) = (row.get(2)?, row.get(3)?);
        let confidence: f64 = row.get(4)?;
        let project = project.map(|p| format!(" [{p}]")).unwrap_or_default();
        println!(
            "{} – {}  [task] {label}{project} ({confidence:.2})",
            local(start)?.strftime("%H:%M:%S"),
            local(end)?.strftime("%H:%M:%S"),
        );
    }
    Ok(())
}

fn local(ms: i64) -> anyhow::Result<Zoned> {
    Ok(chronicle_core::types::ms_to_ts(ms).to_zoned(TimeZone::system()))
}
