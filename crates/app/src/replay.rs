//! Load a fixture event stream into a data dir the way the daemon would have
//! captured it (m43 chunk 0), so the site's screenshots run against synthetic
//! days. Nothing to do with `chronicle_core::replay`, which scores
//! corrections.

use std::path::Path;

use anyhow::{Context, bail};
use chronicle_core::config::Config;
use chronicle_core::types::{CaptureEvent, Event, FocusEvent, UrlEvent};
use chronicle_core::{sessionizer, storage};
use jiff::{civil, tz::TimeZone};

/// `<fixture.jsonl>=<YYYY-MM-DD>`: the stream, and the local day it lands on.
fn parse_case(spec: &str) -> anyhow::Result<(&str, civil::Date)> {
    let Some((path, day)) = spec.rsplit_once('=') else {
        bail!("bad --case {spec:?}: expected <fixture.jsonl>=<YYYY-MM-DD>")
    };
    let date = day
        .parse()
        .with_context(|| format!("bad day in --case {spec:?}"))?;
    Ok((path, date))
}

/// A fixture line is an `events` row; capture writes those rows through
/// `insert_event`, so replay goes back through the same door.
fn capture_event(e: &Event) -> anyhow::Result<CaptureEvent> {
    let focus = || FocusEvent {
        ts: e.ts,
        app: e.app.clone(),
        title: e.title.clone(),
        pid: None,
    };
    Ok(match e.kind.as_str() {
        "focus" => CaptureEvent::Focus(focus()),
        "title" => CaptureEvent::TitleChanged(focus()),
        "url" => CaptureEvent::Url(UrlEvent {
            ts: e.ts,
            app: e.app.clone(),
            title: e.title.clone(),
            url: e.url.clone().unwrap_or_default(),
        }),
        "afk" => CaptureEvent::Afk {
            idle: e.idle.unwrap_or(false),
            ts: e.ts,
        },
        "lock" => CaptureEvent::Lock {
            locked: e.idle.unwrap_or(false),
            ts: e.ts,
        },
        other => bail!("fixture event kind {other:?} is not a captured kind"),
    })
}

pub(crate) fn replay(data_dir: &Path, cases: &[String], reconcile: bool) -> anyhow::Result<()> {
    let config = Config::load(&data_dir.join("config.toml"))?;
    let mut conn = storage::open(&data_dir.join("chronicle.db"))?;
    let tz = TimeZone::system();
    let distractions = chronicle_core::evidence::compile_patterns(&config.distraction_patterns);
    // Naming a new task is an `ai_jobs` row the daemon runs in a child
    // process; without a model the rows stay queued and the tasks keep the
    // segmenter's placeholder labels.
    let can_name = chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir);
    for case in cases {
        let (path, day) = parse_case(case)?;
        let text = std::fs::read_to_string(path).with_context(|| format!("reading {path}"))?;
        let mut events = text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(serde_json::from_str::<Event>)
            .collect::<Result<Vec<_>, _>>()
            .with_context(|| format!("parsing {path}"))?;
        let Some(first) = events.first() else {
            bail!("{path} has no events")
        };
        // Calendar arithmetic, not a fixed offset: every event keeps its local
        // wall-clock time whatever DST does between the two days.
        let shift = first.ts.to_zoned(tz.clone()).date().until(day)?;
        for e in &mut events {
            e.ts = e.ts.to_zoned(tz.clone()).checked_add(shift)?.timestamp();
        }
        let before: i64 =
            conn.query_row("SELECT COALESCE(MAX(id), 0) FROM batches", [], |r| r.get(0))?;
        for e in &events {
            storage::insert_event(&conn, &capture_event(e)?)?;
        }
        let end = events.last().expect("non-empty above").ts;
        // The daemon's own tick: sessionize the tail, close the batches it
        // fills, anchor and file the spans.
        sessionizer::refresh(&mut conn, &config, end)?;
        eprintln!("{path} → {day}: {n} events", n = events.len());
        let ids: Vec<i64> = {
            let mut stmt =
                conn.prepare("SELECT id FROM batches WHERE id > ?1 ORDER BY start_ts")?;
            let rows = stmt.query_map([before], |r| r.get::<_, i64>(0))?;
            rows.collect::<Result<_, _>>()?
        };
        for id in &ids {
            println!("{id}");
        }
        if reconcile {
            // The daemon's segmenter tick (`reconcile_due`, daemon.rs:1803):
            // place each closed batch, then the follow-ups that run beside it
            // in the same tick (daemon.rs:598-617).
            for &id in &ids {
                chronicle_core::segmenter::reconcile(&mut conn, &config, id, end, &distractions)?;
            }
            chronicle_core::proposals::refresh(&mut conn, end, &distractions)?;
            chronicle_core::segmenter::daily(&mut conn, &config, end)?;
            chronicle_core::self_score::refresh(&conn, end, &tz)?;
            if can_name.is_some() {
                while let Some(job) =
                    storage::next_eligible_ai_job_in(&conn, 0, &["name_task"], true)?
                {
                    crate::daemon::spawn_ai_job_worker(job)?.wait()?;
                }
            }
            summarize(&conn, day)?;
        }
    }
    Ok(())
}

/// What the case left in the database, for the rendering script's log.
fn summarize(conn: &rusqlite::Connection, day: civil::Date) -> anyhow::Result<()> {
    let lo = day
        .to_zoned(TimeZone::system())?
        .timestamp()
        .as_millisecond();
    let hi = day
        .tomorrow()?
        .to_zoned(TimeZone::system())?
        .timestamp()
        .as_millisecond();
    let spans: i64 = conn.query_row(
        "SELECT COUNT(*) FROM spans WHERE start_ts >= ?1 AND start_ts < ?2",
        [lo, hi],
        |r| r.get(0),
    )?;
    let tasks: i64 = conn.query_row(
        "SELECT COUNT(DISTINCT task_id) FROM intervals WHERE start_ts >= ?1 AND start_ts < ?2",
        [lo, hi],
        |r| r.get(0),
    )?;
    let mut stmt = conn.prepare(
        "SELECT source, COUNT(*) FROM intervals WHERE start_ts >= ?1 AND start_ts < ?2
         GROUP BY source ORDER BY source",
    )?;
    let by_source = stmt
        .query_map([lo, hi], |r| {
            Ok(format!(
                "{}={}",
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    eprintln!(
        "{day}: {spans} spans, intervals {}, {tasks} tasks",
        if by_source.is_empty() {
            "none".to_owned()
        } else {
            by_source.join(" ")
        }
    );
    Ok(())
}
