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
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let data_dir = chronicle_core::data_dir().context("could not resolve a data directory")?;
    match cli.cmd.unwrap_or(Cmd::Run) {
        Cmd::Run => run(&data_dir),
        Cmd::Dump { day } => dump(&data_dir, day.as_deref()),
        Cmd::Ui => ui::run(&data_dir),
        Cmd::Toggle => {
            if send_toggle(&socket_path(&data_dir)) {
                Ok(())
            } else {
                bail!("chronicle daemon is not running")
            }
        }
        Cmd::Derive { .. } => bail!("derivation lands in M4"),
        Cmd::ChatWorker => bail!("chat lands in M7"),
    }
}

fn socket_path(data_dir: &Path) -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.to_path_buf())
        .join("chronicle.sock")
}

fn send_toggle(sock: &Path) -> bool {
    use std::io::Write;
    match std::os::unix::net::UnixStream::connect(sock) {
        Ok(mut stream) => stream.write_all(b"toggle\n").is_ok(),
        Err(_) => false,
    }
}

fn spawn_ctrl_listener(
    listener: std::os::unix::net::UnixListener,
    tx: Sender<()>,
) -> anyhow::Result<()> {
    use std::io::BufRead;
    std::thread::Builder::new()
        .name("ctrl".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let mut line = String::new();
                let ok = std::io::BufReader::new(stream).read_line(&mut line).is_ok();
                if ok && line.trim() == "toggle" && tx.send(()).is_err() {
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

fn spawn_ui_child() -> std::io::Result<Child> {
    Command::new(std::env::current_exe()?)
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
    if send_toggle(&sock) {
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
    let (tx, rx) = crossbeam_channel::unbounded();
    let (ctrl_tx, ctrl_rx) = crossbeam_channel::unbounded();
    spawn_ctrl_listener(listener, ctrl_tx)?;
    spawn_capture(&config, tx)?;
    tracing::info!(?data_dir, "chronicle daemon running");
    let mut ui_child: Option<Child> = None;
    let mut next_refresh = Instant::now() + SESSIONIZE_EVERY;
    loop {
        let timeout = next_refresh.saturating_duration_since(Instant::now());
        crossbeam_channel::select! {
            recv(rx) -> event => {
                let Ok(event) = event else { break };
                if filters.excluded(&event) {
                    continue;
                }
                if let Err(e) = chronicle_core::storage::insert_event(&conn, &event) {
                    tracing::error!("event insert failed: {e}");
                }
            }
            recv(ctrl_rx) -> cmd => {
                if cmd.is_ok() {
                    toggle_ui(&mut ui_child);
                }
            }
            default(timeout) => {
                let now = Timestamp::now();
                if let Err(e) = chronicle_core::sessionizer::refresh(&mut conn, &config, now) {
                    tracing::error!("sessionize refresh failed: {e}");
                }
                reap_ui(&mut ui_child);
                next_refresh = Instant::now() + SESSIONIZE_EVERY;
            }
        }
    }
    bail!("capture threads exited")
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
        let (app, title) = match event {
            CaptureEvent::Focus(e) | CaptureEvent::TitleChanged(e) => (&e.app, &e.title),
            CaptureEvent::Afk { .. } => return false,
        };
        self.apps.iter().any(|r| r.is_match(app)) || self.titles.iter().any(|r| r.is_match(title))
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
        "SELECT ts, kind, app, title, idle FROM events
         WHERE ts >= ?1 AND ts < ?2 ORDER BY ts, id",
    )?;
    let mut rows = stmt.query([lo, hi])?;
    while let Some(row) = rows.next()? {
        let ts: i64 = row.get(0)?;
        let (kind, app, title): (String, String, String) = (row.get(1)?, row.get(2)?, row.get(3)?);
        let idle: Option<i64> = row.get(4)?;
        let t = local(ts)?;
        match kind.as_str() {
            "afk" => println!(
                "{}  [afk] idle={}",
                t.strftime("%H:%M:%S"),
                idle.unwrap_or(0) == 1
            ),
            _ => println!("{}  [{kind}] {app}: {title}", t.strftime("%H:%M:%S")),
        }
    }

    let mut stmt = conn.prepare(
        "SELECT start_ts, end_ts, app, title, kind FROM spans
         WHERE start_ts >= ?1 AND start_ts < ?2 ORDER BY start_ts, id",
    )?;
    let mut rows = stmt.query([lo, hi])?;
    while let Some(row) = rows.next()? {
        let (start, end): (i64, i64) = (row.get(0)?, row.get(1)?);
        let (app, title, kind): (String, String, String) = (row.get(2)?, row.get(3)?, row.get(4)?);
        let start = local(start)?;
        let end = local(end)?;
        let label = if kind == "focus" {
            format!(" {app}: {title}")
        } else {
            String::new()
        };
        println!(
            "{} – {}  [{kind}]{label}",
            start.strftime("%H:%M:%S"),
            end.strftime("%H:%M:%S"),
        );
    }
    Ok(())
}

fn local(ms: i64) -> anyhow::Result<Zoned> {
    Ok(chronicle_core::types::ms_to_ts(ms).to_zoned(TimeZone::system()))
}
