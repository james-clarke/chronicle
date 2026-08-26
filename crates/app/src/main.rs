use std::path::Path;

use anyhow::{Context, bail};
use clap::{Parser, Subcommand};
use jiff::{ToSpan, Zoned, civil, tz::TimeZone};

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
    let config = chronicle_core::config::Config::load(&data_dir.join("config.toml"))?;
    let _conn = chronicle_core::storage::open(&data_dir.join("chronicle.db"))?;
    tracing::info!(?data_dir, port = config.port, "chronicle initialized");
    bail!("capture loop lands in M1; config + storage verified")
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
