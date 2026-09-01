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

/// Task-scoped grounding (m16): the task IS the retrieval — external context,
/// checkpoint, journal tail, falling back to raw span evidence for a task
/// with no workspace material yet. Same size cap as [`build_context`].
pub fn build_task_context(
    conn: &Connection,
    task_id: i64,
    tz: &TimeZone,
) -> Result<String, StorageError> {
    let (label, project, external_ref): (String, Option<String>, Option<String>) = conn.query_row(
        "SELECT label, project, external_ref FROM tasks WHERE id=?1",
        [task_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let mut out = String::new();
    let project = project.map(|p| format!(" [{p}]")).unwrap_or_default();
    let anchor = external_ref.map(|r| format!(" ({r})")).unwrap_or_default();
    let _ = writeln!(out, "# Task: {label}{project}{anchor}");

    if let Some((_, content)) = storage::task_context(conn, task_id)? {
        let _ = writeln!(out, "\n## External context\n{}", content.trim());
    }
    if let Some(cp) = storage::get_checkpoint(conn, task_id)? {
        let _ = writeln!(
            out,
            "\n## Checkpoint ({})\n{}\nNext: {}",
            ms_to_ts(cp.ts)
                .to_zoned(tz.clone())
                .strftime("%Y-%m-%d %H:%M"),
            cp.state,
            cp.next_steps
        );
    }
    let journal = storage::journal_tail(conn, task_id, 15)?;
    if !journal.is_empty() {
        let _ = writeln!(out, "\n## Journal");
        for e in &journal {
            let _ = writeln!(
                out,
                "- [{}] {}",
                ms_to_ts(e.start_ts)
                    .to_zoned(tz.clone())
                    .strftime("%m-%d %H:%M"),
                e.entry
            );
        }
    } else {
        // Freshly declared task: raw span evidence beats an empty prompt.
        let evidence = storage::task_evidence_text(conn, task_id)?;
        if !evidence.trim().is_empty() {
            let _ = writeln!(out, "\n## Screen evidence\n{}", evidence.trim());
        }
    }
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

#[cfg(test)]
mod tests {
    use jiff::tz::TimeZone;

    fn db() -> rusqlite::Connection {
        let mut conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::storage::test_migrate(&mut conn);
        conn.execute_batch(
            "INSERT INTO batches (id, start_ts, end_ts, status) VALUES (1, 0, 100, 'done');
             INSERT INTO tasks (id, label, project, status, source, created_ts, external_ref)
                 VALUES (5, 'sending plans', 'plans', 'open', 'user', 10, 'ABC-123');",
        )
        .unwrap();
        conn
    }

    // Workspace material renders in priority order; a task without any falls
    // back to raw span evidence.
    #[test]
    fn build_task_context_sections_and_fallback() {
        let conn = db();
        let ts = crate::types::ms_to_ts(1_000);
        crate::storage::upsert_task_context(&conn, 5, "mcp", ts, "ticket body").unwrap();
        crate::storage::insert_journal_entry(&conn, 5, 1, 10, 90, "wired the API", "[1]").unwrap();
        crate::storage::upsert_checkpoint(&conn, 5, ts, "API wired", "add tests").unwrap();

        let out = super::build_task_context(&conn, 5, &TimeZone::UTC).unwrap();
        assert!(
            out.starts_with("# Task: sending plans [plans] (ABC-123)"),
            "{out}"
        );
        for needle in [
            "## External context",
            "ticket body",
            "## Checkpoint",
            "Next: add tests",
            "## Journal",
            "wired the API",
        ] {
            assert!(out.contains(needle), "missing {needle:?} in {out}");
        }

        // Fresh task: no journal, so span evidence stands in.
        conn.execute_batch(
            "INSERT INTO tasks (id, label, status, source, created_ts)
                 VALUES (6, 'fresh', 'open', 'user', 10);
             INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence)
                 VALUES (6, 1, 10, 90, 0.9);
             INSERT INTO spans (batch_id, start_ts, end_ts, app, title, kind)
                 VALUES (1, 10, 90, 'code', 'editing foo.rs', 'focus');",
        )
        .unwrap();
        let out = super::build_task_context(&conn, 6, &TimeZone::UTC).unwrap();
        assert!(out.contains("## Screen evidence"), "{out}");
        assert!(out.contains("code editing foo.rs"), "{out}");
    }
}
