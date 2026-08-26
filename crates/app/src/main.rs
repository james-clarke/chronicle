use std::path::Path;
use std::time::Duration;

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
        Cmd::Ui | Cmd::Toggle => bail!("UI lands in M3"),
        Cmd::Derive { .. } => bail!("derivation lands in M4"),
        Cmd::ChatWorker => bail!("chat lands in M7"),
    }
}

fn run(data_dir: &Path) -> anyhow::Result<()> {
    let _guard = init_logging(data_dir)?;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let filters = Filters::new(&config)?;
    let conn = chronicle_core::storage::open(&data_dir.join("chronicle.db"))?;
    let (tx, rx) = crossbeam_channel::unbounded();
    spawn_capture(&config, tx)?;
    tracing::info!(?data_dir, "chronicle daemon running");
    for event in rx {
        if filters.excluded(&event) {
            continue;
        }
        if let Err(e) = chronicle_core::storage::insert_event(&conn, &event) {
            tracing::error!("event insert failed: {e}");
        }
    }
    bail!("capture threads exited")
}

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

fn afk_loop(afk: impl chronicle_capture::AfkProvider, tx: Sender<CaptureEvent>, threshold_ms: u64) {
    let mut was_idle = false;
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
        // Backdate the idle transition to when input actually stopped.
        let ts = if idle {
            Timestamp::now()
                .checked_sub((ms as i64).milliseconds())
                .unwrap_or_else(|_| Timestamp::now())
        } else {
            Timestamp::now()
        };
        if tx.send(CaptureEvent::Afk { idle, ts }).is_err() {
            return;
        }
    }
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
         WHERE ts >= ?1 AND ts < ?2 ORDER BY ts",
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
         WHERE start_ts >= ?1 AND start_ts < ?2 ORDER BY start_ts",
    )?;
    let mut rows = stmt.query([lo, hi])?;
    while let Some(row) = rows.next()? {
        let (start, end): (i64, i64) = (row.get(0)?, row.get(1)?);
        let (app, title, kind): (String, String, String) = (row.get(2)?, row.get(3)?, row.get(4)?);
        let start = local(start)?;
        let end = local(end)?;
        println!(
            "{} – {}  [{kind}] {app}: {title}",
            start.strftime("%H:%M:%S"),
            end.strftime("%H:%M:%S"),
        );
    }
    Ok(())
}

fn local(ms: i64) -> anyhow::Result<Zoned> {
    Ok(chronicle_core::types::ms_to_ts(ms).to_zoned(TimeZone::system()))
}
