use std::path::Path;
use std::time::Duration;

use crate::ReportFormat;
use crate::daemon::{DaemonStatus, socket_path};
use crate::rotate;
use anyhow::Context;
use chronicle_core::config::Config;
use jiff::{Timestamp, ToSpan, Zoned, civil, tz::TimeZone};

pub(crate) fn init_logging(
    data_dir: &Path,
) -> anyhow::Result<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt};
    let dir = data_dir.join("logs");
    let file = rotate::SizeRotatingWriter::open(&dir, "chronicle.log")
        .with_context(|| format!("failed to open log file in {}", dir.display()))?;
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

pub(crate) fn mcp_check(data_dir: &Path) -> anyhow::Result<()> {
    let _guard = init_logging(data_dir)?;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let path = config.mcp_path(data_dir);
    println!("mcp config: {}", path.display());
    let mcp = chronicle_mcp::McpConfig::load(&path)?;
    if mcp.servers.is_empty() {
        println!("no servers configured");
    }
    for server in &mcp.servers {
        if !server.enabled {
            println!("{}: disabled", server.name);
            continue;
        }
        match chronicle_mcp::probe_server(server) {
            Ok(p) => println!(
                "{}: ok \u{2014} {} {} \u{b7} {} tools \u{b7} {:.1}s",
                server.name,
                p.server_name,
                p.server_version,
                p.tools.len(),
                p.elapsed.as_secs_f32()
            ),
            Err(e) => println!("{}: FAILED \u{2014} {e:#}", server.name),
        }
    }
    match chronicle_mcp::gather_context(&path).1 {
        Some(ctx) => println!("\n## Workspace context\n{ctx}"),
        None => println!(
            "no context gathered (missing/empty config, or every call failed — see warnings above)"
        ),
    }
    Ok(())
}

pub(crate) enum Liveness {
    Stopped,
    Unresponsive,
    Running(DaemonStatus),
}

pub(crate) fn query_daemon(sock: &Path) -> Liveness {
    query_daemon_within(sock, Duration::from_secs(1))
}

/// `query_daemon` with a caller-chosen reply timeout (the UI thread polls
/// with a short one so an unresponsive daemon cannot stall a frame).
pub(crate) fn query_daemon_within(sock: &Path, timeout: Duration) -> Liveness {
    use std::io::{BufRead, Write};
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(sock) else {
        return Liveness::Stopped;
    };
    let _ = stream.set_read_timeout(Some(timeout));
    if stream.write_all(b"status\n").is_err() {
        return Liveness::Unresponsive;
    }
    let mut line = String::new();
    if std::io::BufReader::new(&stream)
        .read_line(&mut line)
        .is_err()
        || line.trim().is_empty()
    {
        return Liveness::Unresponsive;
    }
    match serde_json::from_str(line.trim()) {
        Ok(status) => Liveness::Running(status),
        Err(_) => Liveness::Unresponsive,
    }
}

pub(crate) fn fmt_secs(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
        _ => format!("{}d {}h", secs / 86_400, (secs % 86_400) / 3600),
    }
}

/// What `chronicle status` reads from the DB and the model dir (the daemon's
/// own view arrives as `Liveness`).
#[derive(Default)]
pub(crate) struct DbStatus {
    pub(crate) last_event_age_secs: Option<u64>,
    pub(crate) last_batch_end_ms: Option<i64>,
    /// Model file name plus preset name; None when no model is downloaded.
    pub(crate) model_file: Option<String>,
    pub(crate) server_error: Option<String>,
    pub(crate) last_prune_age_secs: Option<u64>,
    pub(crate) last_derive: Option<chronicle_core::storage::DeriveMetrics>,
    pub(crate) pending_batches: i64,
    /// Today's ledger gaps and captured-but-underived time (m32 chunk 0).
    pub(crate) not_captured_today_ms: i64,
    pub(crate) underived_today_ms: i64,
    /// The daemon's daily self-score rows, oldest first (m32 chunk 6).
    pub(crate) self_score: Vec<chronicle_core::storage::SelfScore>,
}

pub(crate) fn format_status(liveness: &Liveness, db: &DbStatus) -> String {
    let DbStatus {
        last_event_age_secs,
        last_batch_end_ms,
        model_file,
        server_error,
        last_prune_age_secs,
        last_derive,
        pending_batches,
        not_captured_today_ms,
        underived_today_ms,
        self_score,
    } = db;
    let mut out = String::new();
    match liveness {
        Liveness::Running(s) => {
            out.push_str(&format!(
                "chronicle: healthy — daemon running (uptime {})\n",
                fmt_secs(s.uptime_secs)
            ));
            let worker = match (&s.worker, s.worker_secs) {
                (Some(w), Some(secs)) => format!("{w} ({})", fmt_secs(secs)),
                (Some(w), None) => w.clone(),
                (None, _) if s.derive_active => "running".into(),
                (None, _) if s.model_resident => "idle (model resident)".into(),
                (None, _) => "idle".into(),
            };
            out.push_str(&format!("  derive worker: {worker}\n"));
            out.push_str(&format!(
                "  ui: {}\n",
                if s.ui_open { "open" } else { "closed" }
            ));
            if let Some(idle) = s.idle_secs {
                out.push_str(&format!("  user idle: {}\n", fmt_secs(idle)));
            }
        }
        Liveness::Stopped => out.push_str("chronicle: stopped — daemon not running\n"),
        Liveness::Unresponsive => {
            out.push_str("chronicle: running but not responding — socket up, no status reply\n");
        }
    }
    match last_event_age_secs {
        Some(age) => out.push_str(&format!("  last event: {} ago\n", fmt_secs(*age))),
        None => out.push_str("  last event: none recorded yet\n"),
    }
    if let Some(end_ms) = last_batch_end_ms
        && let Ok(t) = local(*end_ms)
    {
        out.push_str(&format!(
            "  last derived batch ended: {}\n",
            t.strftime("%Y-%m-%d %H:%M")
        ));
    }
    match last_derive {
        Some(d) => {
            let when = local(d.derived_ts)
                .map(|t| t.strftime("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
            out.push_str(&format!(
                "  last derive: batch {} at {when}, {:.1}s, {} prompt + {} gen tokens\n",
                d.batch_id,
                d.derive_ms as f64 / 1000.0,
                d.prompt_tokens,
                d.gen_tokens
            ));
        }
        None => out.push_str("  last derive: none instrumented yet\n"),
    }
    out.push_str(&format!("  pending batches: {pending_batches}\n"));
    if *not_captured_today_ms > 0 {
        out.push_str(&format!(
            "  not captured today: {}\n",
            fmt_secs((*not_captured_today_ms / 1000) as u64)
        ));
    }
    if *underived_today_ms > 0 {
        out.push_str(&format!(
            "  captured, not yet derived: {}\n",
            fmt_secs((*underived_today_ms / 1000) as u64)
        ));
    }
    if !self_score.is_empty() {
        let s = chronicle_core::self_score::Summary::of(self_score);
        let when = local(s.computed_ts)
            .map(|t| t.strftime("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_default();
        out.push_str(&format!(
            "  self-score, {} days to {} (computed {when}):\n",
            s.days, s.last_day
        ));
        for line in s.lines() {
            out.push_str(&format!("    {line}\n"));
        }
    }
    out.push_str(&format!(
        "  model: {}\n",
        model_file.as_deref().unwrap_or("not downloaded")
    ));
    if let Some(err) = server_error {
        out.push_str(&format!("  warning: {err}\n"));
    }
    if let Some(age) = last_prune_age_secs {
        out.push_str(&format!("  last prune: {} ago\n", fmt_secs(*age)));
    }
    out
}

pub(crate) fn status(data_dir: &Path, json: bool) -> anyhow::Result<()> {
    let liveness = query_daemon(&socket_path(data_dir));
    let conn = chronicle_core::storage::open(&data_dir.join("chronicle.db"))?;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let now_ms = Timestamp::now().as_millisecond();
    let age = |ms: i64| ((now_ms - ms) / 1000).max(0) as u64;
    let today_ms = Zoned::now()
        .date()
        .to_zoned(TimeZone::system())?
        .timestamp()
        .as_millisecond();
    let (ledger_start, runs) = chronicle_core::storage::runs_for_report(&conn, today_ms, now_ms)?;
    let not_captured_today_ms =
        chronicle_core::report::capture_gaps(&runs, ledger_start, today_ms, now_ms)
            .iter()
            .map(|(a, b)| b - a)
            .sum();
    let model_file =
        chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir).map(|p| {
            let file = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            match chronicle_derive::model::PRESETS
                .iter()
                .find(|m| m.file == file)
            {
                Some(m) => format!("{file} ({})", m.name),
                None => file,
            }
        });
    let db = DbStatus {
        last_event_age_secs: chronicle_core::storage::latest_event_ts(&conn)?.map(age),
        last_batch_end_ms: chronicle_core::storage::latest_batch_end(&conn)?,
        model_file,
        server_error: chronicle_core::storage::get_meta(&conn, "server_error")?,
        last_prune_age_secs: chronicle_core::storage::get_meta(&conn, "last_prune_ts")?
            .and_then(|v| v.parse::<i64>().ok())
            .map(age),
        last_derive: chronicle_core::storage::last_derive(&conn)?,
        pending_batches: chronicle_core::storage::pending_batch_count(&conn)?,
        not_captured_today_ms,
        underived_today_ms: chronicle_core::storage::underived_ms(&conn, today_ms, now_ms)?,
        self_score: chronicle_core::storage::self_scores(&conn, chronicle_core::self_score::DAYS)?,
    };

    if json {
        let (liveness_str, daemon) = match &liveness {
            Liveness::Running(s) => ("healthy", Some(s)),
            Liveness::Stopped => ("stopped", None),
            Liveness::Unresponsive => ("unresponsive", None),
        };
        let doc = serde_json::json!({
            "liveness": liveness_str,
            "daemon": daemon,
            "last_event_age_secs": db.last_event_age_secs,
            "last_batch_end_ms": db.last_batch_end_ms,
            "model_present": db.model_file.is_some(),
            "model_file": db.model_file,
            "server_error": db.server_error,
            "last_prune_age_secs": db.last_prune_age_secs,
            "last_derive": db.last_derive,
            "pending_batches": db.pending_batches,
            "not_captured_today_ms": db.not_captured_today_ms,
            "underived_today_ms": db.underived_today_ms,
            "self_score": db.self_score,
        });
        println!("{}", serde_json::to_string_pretty(&doc)?);
    } else {
        print!("{}", format_status(&liveness, &db));
    }
    if !matches!(liveness, Liveness::Running(_)) {
        // Scriptable failure code without anyhow's "Error:" noise on top of
        // the report we just printed.
        std::process::exit(1);
    }
    Ok(())
}

pub(crate) fn report(
    data_dir: &Path,
    day: Option<&str>,
    week: Option<&str>,
    format: ReportFormat,
) -> anyhow::Result<()> {
    let conn = chronicle_core::storage::open(&data_dir.join("chronicle.db"))?;
    let tz = TimeZone::system();
    let parse = |s: &str| -> anyhow::Result<civil::Date> {
        s.parse().with_context(|| format!("bad date {s:?}"))
    };
    let days: Vec<civil::Date> = match (day, week) {
        (Some(d), None) => vec![parse(d)?],
        (None, given) => {
            let anchor = match given {
                Some(w) => parse(w)?,
                None => Zoned::now().date(),
            };
            let monday =
                chronicle_core::timeref::week_start(anchor).context("week start out of range")?;
            (0..7)
                .map(|i| Ok(monday.checked_add(i.days())?))
                .collect::<anyhow::Result<_>>()?
        }
        (Some(_), Some(_)) => unreachable!("clap conflicts_with"),
    };
    let lo = days[0].to_zoned(tz.clone())?.timestamp().as_millisecond();
    let hi = (days[days.len() - 1]
        .to_zoned(tz.clone())?
        .checked_add(1.day())?)
    .timestamp()
    .as_millisecond();
    let tasks = chronicle_core::storage::tasks_in_range(&conn, lo, hi)?;
    let mut r = chronicle_core::report::build(&tasks, days, &tz)?;
    let now = Timestamp::now().as_millisecond();
    let (ledger_start, runs) = chronicle_core::storage::runs_for_report(&conn, lo, hi)?;
    r.gaps = chronicle_core::report::capture_gaps(&runs, ledger_start, lo, hi.min(now));
    r.underived_ms = chronicle_core::storage::underived_ms(&conn, lo, hi)?;
    match format {
        ReportFormat::Csv => print!("{}", chronicle_core::report::to_csv(&r)),
        ReportFormat::Md => print!("{}", chronicle_core::report::to_md(&r)),
    }
    Ok(())
}

pub(crate) fn standup_cmd(data_dir: &Path, day: Option<&str>) -> anyhow::Result<()> {
    let conn = chronicle_core::storage::open(&data_dir.join("chronicle.db"))?;
    let date: civil::Date = match day {
        Some(d) => d.parse().with_context(|| format!("bad --day {d:?}"))?,
        None => Zoned::now().date().checked_sub(1.day())?,
    };
    let key = date.to_string();
    match chronicle_core::storage::get_standup_draft(&conn, &key)? {
        Some((_, content)) => println!("{content}"),
        None => println!(
            "no standup draft yet for {key} — it drafts in the daemon's next idle window once the day has journal entries"
        ),
    }
    Ok(())
}

pub(crate) fn dump(data_dir: &Path, day: Option<&str>) -> anyhow::Result<()> {
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
    let intervals = count("SELECT COUNT(*) FROM intervals WHERE start_ts >= ?1 AND start_ts < ?2")?;
    let tasks = count(
        "SELECT COUNT(DISTINCT task_id) FROM intervals WHERE start_ts >= ?1 AND start_ts < ?2",
    )?;
    println!(
        "events: {events}  spans: {spans}  batches: {batches}  tasks: {tasks}  intervals: {intervals}"
    );

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
            "lock" => println!(
                "{}  [lock] locked={}",
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
        "SELECT i.start_ts, i.end_ts, t.id, t.label, t.project, i.confidence, t.source, i.share
         FROM intervals i JOIN tasks t ON t.id = i.task_id
         WHERE i.start_ts >= ?1 AND i.start_ts < ?2 ORDER BY i.start_ts, i.id",
    )?;
    let mut rows = stmt.query([lo, hi])?;
    while let Some(row) = rows.next()? {
        let (start, end): (i64, i64) = (row.get(0)?, row.get(1)?);
        let task_id: i64 = row.get(2)?;
        let (label, project): (String, Option<String>) = (row.get(3)?, row.get(4)?);
        let confidence: f64 = row.get(5)?;
        let source: String = row.get(6)?;
        let share: f64 = row.get(7)?;
        let project = project.map(|p| format!(" [{p}]")).unwrap_or_default();
        let declared = if source == "user" { " (declared)" } else { "" };
        let share = if share < 1.0 {
            format!(" {:.0}%", share * 100.0)
        } else {
            String::new()
        };
        println!(
            "{} – {}  [task #{task_id}] {label}{project}{declared}{share} ({confidence:.2})",
            local(start)?.strftime("%H:%M:%S"),
            local(end)?.strftime("%H:%M:%S"),
        );
    }
    Ok(())
}

pub(crate) fn local(ms: i64) -> anyhow::Result<Zoned> {
    Ok(chronicle_core::types::ms_to_ts(ms).to_zoned(TimeZone::system()))
}
