//! Question → grounding context for the chat worker, local DB only.
//! A recognized time reference selects an SQL range; anything else falls back
//! to FTS over task labels + span titles, then to today.

use std::collections::HashMap;
use std::fmt::Write;

use jiff::Zoned;
use jiff::tz::TimeZone;
use rusqlite::Connection;

use crate::sessionizer::SpanKind;
use crate::storage::{self, StorageError};
use crate::types::{Task, ms_to_ts};
use crate::{digest, timeref};

/// Hard cap ≈ digest::MAX_TOKENS with the chars/4 heuristic.
const MAX_CHARS: usize = digest::MAX_TOKENS * 4;
const FTS_K: usize = 12;

pub fn build_context(
    conn: &Connection,
    question: &str,
    now: &Zoned,
) -> Result<String, StorageError> {
    let tz = now.time_zone();
    let mut out = match timeref::parse(question, now) {
        Some((lo, hi)) => range_context(conn, lo, hi, tz)?,
        None => {
            let fts = fts_context(conn, question, tz)?;
            if fts.is_empty() {
                // Nothing matched: today's activity beats an empty prompt.
                let (lo, hi) = today_range(now);
                format!(
                    "(no data matched the question; showing today)\n{}",
                    range_context(conn, lo, hi, tz)?
                )
            } else {
                fts
            }
        }
    };
    let mut cut = MAX_CHARS.min(out.len());
    while !out.is_char_boundary(cut) {
        cut -= 1;
    }
    out.truncate(cut);
    Ok(out)
}

fn today_range(now: &Zoned) -> (i64, i64) {
    let lo = now
        .start_of_day()
        .map(|z| z.timestamp().as_millisecond())
        .unwrap_or_else(|_| now.timestamp().as_millisecond() - 24 * 3_600_000);
    (lo, now.timestamp().as_millisecond())
}

fn range_context(
    conn: &Connection,
    lo: i64,
    hi: i64,
    tz: &TimeZone,
) -> Result<String, StorageError> {
    let tasks = storage::tasks_in_range(conn, lo, hi)?;
    let spans = storage::spans_in_range(conn, lo, hi)?;

    let start = ms_to_ts(lo).to_zoned(tz.clone());
    let end = ms_to_ts(hi).to_zoned(tz.clone());
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Activity {}\u{2013}{} ({})",
        start.strftime("%Y-%m-%d %H:%M"),
        end.strftime("%Y-%m-%d %H:%M"),
        tz.iana_name().unwrap_or("local"),
    );

    // Ahead of ## Tasks so totals survive the MAX_CHARS truncation, and
    // over the full vec, not the take(60) display cap below.
    let totals = crate::report::project_totals(&tasks, lo, hi);
    if !totals.is_empty() {
        let _ = writeln!(out, "\n## Totals by project");
        for p in &totals {
            let _ = writeln!(out, "- {}: {}", p.project, fmt_dur(p.total_ms));
        }
    }

    let _ = writeln!(out, "\n## Tasks (derived, may lag recent activity)");
    if tasks.is_empty() {
        let _ = writeln!(out, "(none derived for this range)");
    }
    let multi_day = start.date() != end.date();
    for t in tasks.iter().take(60) {
        let _ = writeln!(out, "- {}", task_line(t, tz, multi_day));
    }

    // Aggregates from raw spans back the tasks up (and stand in for them
    // when derivation hasn't caught up yet).
    let mut active_ms = 0i64;
    let mut afk_ms = 0i64;
    let mut app_ms: HashMap<&str, i64> = HashMap::new();
    let mut title_ms: HashMap<(&str, &str), i64> = HashMap::new();
    for s in &spans {
        let dur = s.duration_ms();
        match s.kind {
            SpanKind::Focus => {
                active_ms += dur;
                *app_ms.entry(s.app.as_str()).or_default() += dur;
                *title_ms
                    .entry((s.app.as_str(), s.title.as_str()))
                    .or_default() += dur;
            }
            SpanKind::ContextSwitching => active_ms += dur,
            SpanKind::Afk => afk_ms += dur,
        }
    }
    let _ = writeln!(
        out,
        "\nactive {} \u{b7} afk {}",
        fmt_dur(active_ms),
        fmt_dur(afk_ms)
    );

    let _ = writeln!(out, "\n## Apps by time");
    for (app, ms) in top(app_ms, 10) {
        let _ = writeln!(out, "- {app}: {}", fmt_dur(ms));
    }

    let _ = writeln!(out, "\n## Windows by time");
    for ((app, title), ms) in top(title_ms, 15) {
        let _ = writeln!(out, "- {}: {app}: {}", fmt_dur(ms), clip(title, 80));
    }
    Ok(out)
}

fn fts_context(conn: &Connection, question: &str, tz: &TimeZone) -> Result<String, StorageError> {
    let query = storage::fts_query_from_text(question);
    let tasks = storage::search_tasks(conn, &query, FTS_K)?;
    let spans = storage::search_spans(conn, &query, FTS_K)?;
    if tasks.is_empty() && spans.is_empty() {
        return Ok(String::new());
    }
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Stored activity matching the question ({})",
        tz.iana_name().unwrap_or("local"),
    );
    if !tasks.is_empty() {
        let _ = writeln!(out, "\n## Matching tasks");
        for t in &tasks {
            let _ = writeln!(out, "- {}", task_line(t, tz, true));
        }
    }
    if !spans.is_empty() {
        let _ = writeln!(out, "\n## Matching windows");
        for s in &spans {
            let start = s.start.to_zoned(tz.clone());
            let end = s.end.to_zoned(tz.clone());
            let _ = writeln!(
                out,
                "- {} {}\u{2013}{} {}: {}",
                start.strftime("%Y-%m-%d"),
                start.strftime("%H:%M"),
                end.strftime("%H:%M"),
                s.app,
                clip(&s.title, 80),
            );
        }
    }
    Ok(out)
}

fn task_line(t: &Task, tz: &TimeZone, with_date: bool) -> String {
    let start = t.start_ts.to_zoned(tz.clone());
    let end = t.end_ts.to_zoned(tz.clone());
    let date = if with_date {
        format!("{} ", start.strftime("%Y-%m-%d"))
    } else {
        String::new()
    };
    let project = t
        .project
        .as_deref()
        .map(|p| format!(" [{p}]"))
        .unwrap_or_default();
    format!(
        "{date}{}\u{2013}{} {}{project}",
        start.strftime("%H:%M"),
        end.strftime("%H:%M"),
        t.label,
    )
}

fn top<K>(map: HashMap<K, i64>, n: usize) -> Vec<(K, i64)>
where
    K: Ord,
{
    let mut v: Vec<(K, i64)> = map.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    v.truncate(n);
    v
}

fn clip(s: &str, max_chars: usize) -> String {
    let mut it = s.chars();
    let head: String = it.by_ref().take(max_chars).collect();
    if it.next().is_some() {
        head + "\u{2026}"
    } else {
        head
    }
}

fn fmt_dur(ms: i64) -> String {
    let s = ms / 1000;
    let (h, m) = (s / 3600, (s % 3600) / 60);
    if h > 0 {
        format!("{h}h{m:02}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{}s", s % 60)
    }
}
