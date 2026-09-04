//! Question → grounding context for the chat worker, local DB only.
//! A recognized time reference selects an SQL range; anything else falls back
//! to FTS over task labels + span titles, then to today. A question asking
//! for a quantity also carries a SQL totals table for its range, so the
//! model quotes figures instead of adding rows up.

use std::collections::HashMap;
use std::fmt::Write;

use jiff::ToSpan;
use jiff::Zoned;
use jiff::tz::TimeZone;
use rusqlite::Connection;

use crate::sessionizer::SpanKind;
use crate::storage::{self, StorageError};
use crate::types::{ActivityKind, Task, ms_to_ts, ts_to_ms};
use crate::{digest, report, timeref};

const FTS_K: usize = 12;
/// Rows of the per-task totals table a quantity question gets.
const TABLE_ROWS: usize = 40;

/// How much context a question may carry: the local 4B gets
/// `digest::MAX_TOKENS`; a cloud backend (m31) gets a budget an order of
/// magnitude larger, and the row caps scale with it.
#[derive(Debug, Clone, Copy)]
pub struct Budget {
    pub max_chars: usize,
    pub table_rows: usize,
    pub fts_k: usize,
}

impl Budget {
    pub fn for_tokens(max_tokens: usize) -> Self {
        let scale = (max_tokens / digest::MAX_TOKENS).clamp(1, 8);
        Self {
            max_chars: digest::max_chars(max_tokens),
            table_rows: TABLE_ROWS * scale,
            fts_k: FTS_K * scale,
        }
    }

    /// The local model's budget.
    pub fn local() -> Self {
        Self::for_tokens(digest::MAX_TOKENS)
    }
}

/// What an answer was built from, for the panel's "Read 14 blocks ·
/// Thu 3 Sep 08:00–14:56" footer and the row list it expands to.
#[derive(Debug, Clone, Default)]
pub struct ChatContextInfo {
    /// Number of activity rows handed to the model: `rows.len()`.
    pub blocks: usize,
    /// Range the context covers, epoch ms. `None` when the question carried
    /// no time reference and search stood in for one.
    pub start_ms: Option<i64>,
    pub end_ms: Option<i64>,
    /// Those rows, in prompt order, as display text.
    pub rows: Vec<String>,
}

pub fn build_context(
    conn: &Connection,
    question: &str,
    now: &Zoned,
) -> Result<(String, ChatContextInfo), StorageError> {
    build_context_with(conn, question, now, Budget::local())
}

/// [`build_context`] under an explicit budget.
pub fn build_context_with(
    conn: &Connection,
    question: &str,
    now: &Zoned,
    budget: Budget,
) -> Result<(String, ChatContextInfo), StorageError> {
    let tz = now.time_zone();
    let (mut out, mut info) = match resolve_range(conn, question, now)? {
        Some((lo, hi)) => range_context(conn, question, lo, hi, tz, budget)?,
        // A quantity question has to arrive with the totals table — the
        // prompt tells the model to refuse the total without one — and FTS
        // context carries none. This week stands in for the missing phrase.
        None if is_quantity_question(question) => {
            let (lo, hi) = this_week_range(now);
            range_context(conn, question, lo, hi, tz, budget)?
        }
        None => {
            let (fts, fts_info) = fts_context(conn, question, tz, budget)?;
            if fts.is_empty() {
                // Nothing matched: today's activity beats an empty prompt.
                let (lo, hi) = today_range(now);
                let (ctx, info) = range_context(conn, question, lo, hi, tz, budget)?;
                (
                    format!("(no data matched the question; showing today)\n{ctx}"),
                    info,
                )
            } else {
                (fts, fts_info)
            }
        }
    };
    digest::truncate_chars(&mut out, budget.max_chars);
    // Truncation drops rows off the tail of the prompt; the footer must
    // count only the ones the model actually received.
    info.rows.retain(|r| out.contains(r.as_str()));
    info.blocks = info.rows.len();
    Ok((out, info))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallSide {
    Before,
    After,
}

/// "before the call", "after the 09:40 call": the nearest side word ahead of
/// the noun wins.
fn call_side(question: &str) -> Option<CallSide> {
    let lower = question.to_lowercase();
    let words: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .collect();
    let at = words.iter().position(|w| *w == "call" || *w == "calls")?;
    words[..at].iter().rev().find_map(|w| match *w {
        "before" => Some(CallSide::Before),
        "after" => Some(CallSide::After),
        _ => None,
    })
}

/// The range a question asks about: a call-relative phrase first (it needs
/// the DB), otherwise the deterministic phrase set in [`timeref`]. The call
/// is the day's first mic-in-use block; with none recorded the day's own
/// range stands.
fn resolve_range(
    conn: &Connection,
    question: &str,
    now: &Zoned,
) -> Result<Option<(i64, i64)>, StorageError> {
    let parsed = timeref::parse(question, now);
    if let Some(side) = call_side(question) {
        let (day_lo, day_hi) = parsed.unwrap_or_else(|| today_range(now));
        // A call splits one day. Over a wider span the first call in it
        // would cut the whole span down to one side of that one call, so
        // the phrase is ignored instead.
        if within_one_day(day_lo, day_hi, now.time_zone()) {
            let call = storage::activity_in_range(conn, day_lo, day_hi)?
                .into_iter()
                .find(|e| e.kind == ActivityKind::Call);
            if let Some(call) = call {
                let start = ts_to_ms(call.ts);
                // A call still running ends now, not at its own start —
                // otherwise "after the call" covers the call itself.
                let end = call
                    .end_ts
                    .map_or_else(|| now.timestamp().as_millisecond(), ts_to_ms);
                let (lo, hi) = match side {
                    CallSide::Before => (day_lo, start.clamp(day_lo, day_hi)),
                    CallSide::After => (end.clamp(day_lo, day_hi), day_hi),
                };
                // A call at the window's edge collapses the range to
                // nothing; the phrase's own range beats an empty one.
                if hi - lo >= 60_000 {
                    return Ok(Some((lo, hi)));
                }
            }
        }
    }
    Ok(parsed)
}

/// Whether `[lo, hi)` falls inside a single civil day locally — `hi` is
/// exclusive, so a whole-day range ending at the next midnight counts.
fn within_one_day(lo: i64, hi: i64, tz: &TimeZone) -> bool {
    hi > lo
        && ms_to_ts(lo).to_zoned(tz.clone()).date() == ms_to_ts(hi - 1).to_zoned(tz.clone()).date()
}

/// Questions whose answer is a number. Those get the totals table and are
/// told to read it off rather than add rows up.
fn is_quantity_question(question: &str) -> bool {
    let q = question.to_lowercase();
    [
        "how long",
        "how much",
        "how many hour",
        "how many min",
        "total",
        "per project",
        "spent on",
        "time on",
    ]
    .iter()
    .any(|p| q.contains(p))
}

/// Task-scoped grounding (m16): the task IS the retrieval — external context,
/// checkpoint, journal tail, falling back to raw span evidence for a task
/// with no workspace material yet. Same size cap as [`build_context`].
pub fn build_task_context(
    conn: &Connection,
    task_id: i64,
    tz: &TimeZone,
) -> Result<String, StorageError> {
    build_task_context_with(conn, task_id, tz, Budget::local())
}

/// [`build_task_context`] under an explicit budget.
pub fn build_task_context_with(
    conn: &Connection,
    task_id: i64,
    tz: &TimeZone,
    budget: Budget,
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
    digest::truncate_chars(&mut out, budget.max_chars);
    Ok(out)
}

fn today_range(now: &Zoned) -> (i64, i64) {
    let lo = now
        .start_of_day()
        .map(|z| z.timestamp().as_millisecond())
        .unwrap_or_else(|_| now.timestamp().as_millisecond() - 24 * 3_600_000);
    (lo, now.timestamp().as_millisecond())
}

/// Monday 00:00 local through now — the default range for a quantity
/// question that named no time at all.
fn this_week_range(now: &Zoned) -> (i64, i64) {
    let back = i64::from(now.date().weekday().to_monday_zero_offset());
    let lo = now
        .date()
        .checked_sub(back.days())
        .ok()
        .and_then(|d| d.to_zoned(now.time_zone().clone()).ok())
        .map(|z| z.timestamp().as_millisecond())
        .unwrap_or_else(|| today_range(now).0);
    (lo, now.timestamp().as_millisecond())
}

fn range_context(
    conn: &Connection,
    question: &str,
    lo: i64,
    hi: i64,
    tz: &TimeZone,
    budget: Budget,
) -> Result<(String, ChatContextInfo), StorageError> {
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

    // Ahead of ## Tasks so totals survive the budget truncation, and
    // over the full vec, not the take(60) display cap below.
    let totals = report::project_totals(&tasks, lo, hi);
    if !totals.is_empty() {
        let _ = writeln!(out, "\n## Totals by project");
        for p in &totals {
            let _ = writeln!(out, "- {}: {}", p.project, fmt_dur(p.total_ms));
        }
    }

    if is_quantity_question(question) {
        let per_task = report::task_totals(&tasks, lo, hi);
        if !per_task.is_empty() {
            let _ = write!(
                out,
                "\n## Time per task (computed from the database; quote these figures verbatim)\n{}",
                report::totals_table(&per_task, budget.table_rows)
            );
        }
    }

    let mut rows = Vec::new();
    let _ = writeln!(out, "\n## Tasks (derived, may lag recent activity)");
    if tasks.is_empty() {
        let _ = writeln!(out, "(none derived for this range)");
    }
    let multi_day = start.date() != end.date();
    for t in tasks.iter().take(60) {
        let line = task_line(t, tz, multi_day);
        let _ = writeln!(out, "- {line}");
        rows.push(line);
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
        let line = format!("{}: {app}: {}", fmt_dur(ms), digest::clip(title, 80));
        let _ = writeln!(out, "- {line}");
        rows.push(line);
    }
    Ok((
        out,
        ChatContextInfo {
            blocks: rows.len(),
            start_ms: Some(lo),
            end_ms: Some(hi),
            rows,
        },
    ))
}

fn fts_context(
    conn: &Connection,
    question: &str,
    tz: &TimeZone,
    budget: Budget,
) -> Result<(String, ChatContextInfo), StorageError> {
    let query = storage::fts_query_from_text(question);
    let tasks = storage::search_tasks(conn, &query, budget.fts_k)?;
    let spans = storage::search_spans(conn, &query, budget.fts_k)?;
    if tasks.is_empty() && spans.is_empty() {
        return Ok((String::new(), ChatContextInfo::default()));
    }
    let mut rows = Vec::new();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Stored activity matching the question ({})",
        tz.iana_name().unwrap_or("local"),
    );
    if !tasks.is_empty() {
        let _ = writeln!(out, "\n## Matching tasks");
        for t in &tasks {
            let line = task_line(t, tz, true);
            let _ = writeln!(out, "- {line}");
            rows.push(line);
        }
    }
    if !spans.is_empty() {
        let _ = writeln!(out, "\n## Matching windows");
        for s in &spans {
            let start = s.start.to_zoned(tz.clone());
            let end = s.end.to_zoned(tz.clone());
            let line = format!(
                "{} {}\u{2013}{} {}: {}",
                start.strftime("%Y-%m-%d"),
                start.strftime("%H:%M"),
                end.strftime("%H:%M"),
                s.app,
                digest::clip(&s.title, 80),
            );
            let _ = writeln!(out, "- {line}");
            rows.push(line);
        }
    }
    Ok((
        out,
        ChatContextInfo {
            blocks: rows.len(),
            rows,
            ..ChatContextInfo::default()
        },
    ))
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
    use jiff::civil;
    use jiff::tz::TimeZone;

    /// Wed 2026-08-26 14:30 UTC.
    fn now() -> jiff::Zoned {
        civil::date(2026, 8, 26)
            .at(14, 30, 0, 0)
            .to_zoned(TimeZone::UTC)
            .unwrap()
    }

    fn at(date: civil::Date, hour: i8) -> i64 {
        date.at(hour, 0, 0, 0)
            .to_zoned(TimeZone::UTC)
            .unwrap()
            .timestamp()
            .as_millisecond()
    }

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

    #[test]
    fn phrases_resolve_to_fixed_ranges() {
        let conn = db();
        let (mon, tue, wed) = (
            civil::date(2026, 8, 24),
            civil::date(2026, 8, 25),
            civil::date(2026, 8, 26),
        );
        let thu = civil::date(2026, 8, 27);
        for (question, want) in [
            (
                "what did I work on this morning?",
                (at(wed, 0), at(wed, 12)),
            ),
            (
                "what happened yesterday afternoon",
                (at(tue, 12), at(tue, 18)),
            ),
            ("how long on chronicle this week?", (at(mon, 0), at(thu, 0))),
            (
                "what shipped last week",
                (at(civil::date(2026, 8, 17), 0), at(mon, 0)),
            ),
            // Most recent past Tuesday, today included for today's weekday.
            ("what did I do on tuesday", (at(tue, 0), at(wed, 0))),
            ("what did I do on wednesday", (at(wed, 0), at(thu, 0))),
        ] {
            assert_eq!(
                super::resolve_range(&conn, question, &now()).unwrap(),
                Some(want),
                "{question}"
            );
        }
    }

    fn insert_call(conn: &rusqlite::Connection, ts: i64, end_ts: Option<i64>) {
        conn.execute(
            "INSERT INTO activity_events (ts, end_ts, repo, branch, kind, ext_id)
                 VALUES (?1, ?2, '', '', 'call', 'call:' || ?1)",
            rusqlite::params![ts, end_ts],
        )
        .unwrap();
    }

    // The call = the day's first mic-in-use block; a day without one keeps
    // the range the rest of the phrase parsed to.
    #[test]
    fn call_relative_ranges_split_the_day() {
        let conn = db();
        let (tue, wed) = (civil::date(2026, 8, 25), civil::date(2026, 8, 26));
        conn.execute(
            "INSERT INTO activity_events (ts, end_ts, repo, branch, kind, ext_id)
                 VALUES (?1, ?2, '', '', 'call', 'call:1'), (?3, ?4, '', '', 'call', 'call:2')",
            [at(wed, 9), at(wed, 10), at(wed, 13), at(wed, 14)],
        )
        .unwrap();

        let now_ms = now().timestamp().as_millisecond();
        for (question, want) in [
            (
                "what was I doing before the call?",
                (at(wed, 0), at(wed, 9)),
            ),
            (
                "what was I doing before the 09:40 call?",
                (at(wed, 0), at(wed, 9)),
            ),
            ("and after the call?", (at(wed, 10), now_ms)),
            (
                "what did I do after the call this morning",
                (at(wed, 10), at(wed, 12)),
            ),
            ("before the call yesterday", (at(tue, 0), at(wed, 0))),
        ] {
            assert_eq!(
                super::resolve_range(&conn, question, &now()).unwrap(),
                Some(want),
                "{question}"
            );
        }

        // A call still running ends now, not at its own start, so "after
        // the call" doesn't swallow the call itself.
        let conn = db();
        insert_call(&conn, at(wed, 9), None);
        assert_eq!(
            super::resolve_range(&conn, "what did I do after the call today", &now()).unwrap(),
            Some((now_ms, at(civil::date(2026, 8, 27), 0)))
        );
    }

    // A call at the edge of the window would collapse the range to nothing;
    // the range the phrase itself parsed to stands instead.
    #[test]
    fn calls_at_the_window_edge_keep_the_parsed_range() {
        let (wed, thu) = (civil::date(2026, 8, 26), civil::date(2026, 8, 27));

        // Starts at midnight: nothing is "before the call".
        let conn = db();
        insert_call(&conn, at(wed, 0), Some(at(wed, 1)));
        assert_eq!(
            super::resolve_range(&conn, "what did I do before the call today", &now()).unwrap(),
            Some((at(wed, 0), at(thu, 0)))
        );

        // Runs past the window: nothing is "after the call" inside it.
        let conn = db();
        insert_call(&conn, at(wed, 11), Some(at(wed, 18)));
        assert_eq!(
            super::resolve_range(&conn, "what did I do after the call this morning", &now())
                .unwrap(),
            Some((at(wed, 0), at(wed, 12)))
        );

        // Same for a call still running when the window closes.
        let conn = db();
        insert_call(&conn, at(wed, 8), None);
        assert_eq!(
            super::resolve_range(&conn, "what did I do after the call this morning", &now())
                .unwrap(),
            Some((at(wed, 0), at(wed, 12)))
        );
    }

    // A call splits one day, not a span of them: over a multi-day range the
    // phrase is ignored rather than truncating the span at its first call.
    #[test]
    fn call_phrase_is_ignored_over_a_multi_day_range() {
        let conn = db();
        let (mon, wed, thu) = (
            civil::date(2026, 8, 24),
            civil::date(2026, 8, 26),
            civil::date(2026, 8, 27),
        );
        insert_call(&conn, at(wed, 9), Some(at(wed, 10)));
        for (question, want) in [
            (
                "how long before the call this week",
                (at(mon, 0), at(thu, 0)),
            ),
            (
                "what did I do after the call last week",
                (at(civil::date(2026, 8, 17), 0), at(mon, 0)),
            ),
        ] {
            assert_eq!(
                super::resolve_range(&conn, question, &now()).unwrap(),
                Some(want),
                "{question}"
            );
        }
    }

    // A quantity question carries the SQL totals table; the answer's figure
    // is in the prompt verbatim, so the model never adds rows up.
    #[test]
    fn quantity_question_gets_the_totals_table() {
        let conn = db();
        let wed = civil::date(2026, 8, 26);
        conn.execute(
            "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence)
                 VALUES (5, 1, ?1, ?2, 0.9)",
            [at(wed, 9), at(wed, 9) + 90 * 60_000],
        )
        .unwrap();

        let (ctx, info) =
            super::build_context(&conn, "how long did I spend on plans this morning?", &now())
                .unwrap();
        assert!(ctx.contains("## Time per task"), "{ctx}");
        assert!(ctx.contains("| sending plans | plans | 1h30m |"), "{ctx}");
        assert!(ctx.contains("| all tasks | | 1h30m |"), "{ctx}");
        assert_eq!(
            (info.start_ms, info.end_ms),
            (Some(at(wed, 0)), Some(at(wed, 12)))
        );
        assert_eq!(info.blocks, info.rows.len());
        assert!(
            info.rows.iter().any(|r| r.contains("sending plans")),
            "{:?}",
            info.rows
        );

        let (ctx, _) =
            super::build_context(&conn, "what did I work on this morning?", &now()).unwrap();
        assert!(!ctx.contains("## Time per task"), "{ctx}");
    }

    // No time phrase at all: the quantity question still has to arrive with
    // the table (the prompt refuses the total without one), so it defaults
    // to this week instead of falling to search.
    #[test]
    fn quantity_question_without_a_time_phrase_covers_this_week() {
        let conn = db();
        let mon = civil::date(2026, 8, 24);
        conn.execute(
            "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence)
                 VALUES (5, 1, ?1, ?2, 0.9)",
            [at(mon, 9), at(mon, 11)],
        )
        .unwrap();

        let (ctx, info) =
            super::build_context(&conn, "how long did I spend on ABC-123?", &now()).unwrap();
        assert!(ctx.contains("## Time per task"), "{ctx}");
        assert!(ctx.contains("| sending plans | plans | 2h00m |"), "{ctx}");
        assert_eq!(
            (info.start_ms, info.end_ms),
            (Some(at(mon, 0)), Some(now().timestamp().as_millisecond()))
        );

        // A non-quantity question with no phrase still goes through search.
        let (ctx, info) =
            super::build_context(&conn, "what was that plans thing?", &now()).unwrap();
        assert!(
            ctx.contains("# Stored activity matching the question"),
            "{ctx}"
        );
        assert_eq!((info.start_ms, info.end_ms), (None, None));
    }
}
