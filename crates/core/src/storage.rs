use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use rusqlite::{Connection, params};
use rusqlite_migration::{M, Migrations};

use crate::sessionizer::{BatchDraft, SpanDraft, SpanKind};
use crate::types::{
    ActivityEvent, ActivityKind, CaptureEvent, Correction, Dedupe, Event, FocusEvent, NewInterval,
    OpenTask, Task, TaskSlot, ms_to_ts, ts_to_ms,
};

#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("failed to create data dir: {0}")]
    Io(#[from] std::io::Error),
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("migration error: {0}")]
    Migration(#[from] rusqlite_migration::Error),
}

static MIGRATIONS: LazyLock<Migrations<'static>> = LazyLock::new(|| {
    Migrations::new(vec![
        M::up(include_str!("../migrations/001_schema.sql")),
        M::up(include_str!("../migrations/002_corrections_fts.sql")),
        M::up(include_str!("../migrations/003_spans_url_meta.sql")),
        M::up(include_str!("../migrations/004_task_identity.sql")),
        M::up(include_str!("../migrations/005_chat_conversations.sql")),
        M::up(include_str!("../migrations/006_task_descriptions.sql")),
        M::up(include_str!("../migrations/007_vcs_events.sql")),
        M::up(include_str!("../migrations/008_task_workspace.sql")),
        M::up(include_str!("../migrations/009_standup_drafts.sql")),
        M::up(include_str!("../migrations/010_activity_events.sql")),
        M::up(include_str!("../migrations/011_interval_source.sql")),
        M::up(include_str!("../migrations/012_interval_tail.sql")),
        M::up(include_str!("../migrations/013_proposals.sql")),
        M::up(include_str!("../migrations/014_derive_metrics.sql")),
    ])
});

/// Migrate an in-memory connection for sibling modules' unit tests.
#[cfg(test)]
pub(crate) fn test_migrate(conn: &mut Connection) {
    MIGRATIONS.to_latest(conn).unwrap();
}

pub fn open(path: &Path) -> Result<Connection, StorageError> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut conn = Connection::open(path)?;
    // auto_vacuum must be set before the first table exists; it's a no-op on
    // an already-populated db (flipping it later requires a full VACUUM).
    conn.pragma_update(None, "auto_vacuum", "INCREMENTAL")?;
    conn.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(()))?;
    conn.busy_timeout(Duration::from_secs(5))?;
    // Foreign keys OFF during migrations (bundled sqlite defaults them ON):
    // 004 rebuilds `tasks` under live children, which enforcement would
    // reject mid-transaction.
    conn.pragma_update(None, "foreign_keys", "OFF")?;
    MIGRATIONS.to_latest(&mut conn)?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    Ok(conn)
}

pub fn insert_event(conn: &Connection, event: &CaptureEvent) -> Result<(), StorageError> {
    match event {
        CaptureEvent::Focus(e) => insert_focus(conn, "focus", e),
        CaptureEvent::TitleChanged(e) => insert_focus(conn, "title", e),
        CaptureEvent::Url(e) => {
            conn.execute(
                "INSERT INTO events (ts, kind, app, title, url) VALUES (?1, 'url', ?2, ?3, ?4)",
                params![ts_to_ms(e.ts), e.app, e.title, e.url],
            )?;
            Ok(())
        }
        CaptureEvent::Afk { idle, ts } => {
            conn.execute(
                "INSERT INTO events (ts, kind, app, idle) VALUES (?1, 'afk', '', ?2)",
                params![ts_to_ms(*ts), *idle as i64],
            )?;
            Ok(())
        }
        CaptureEvent::Activity(e) => insert_activity_event(conn, e),
    }
}

/// Stores one observation per the kind's [`Dedupe`] rule: a checkout
/// matching the repo's latest stored checkout is dropped (the poller
/// re-announces its state on daemon start, and replays must not stack
/// duplicate branch markers); span kinds upsert on `(kind, ext_id)`; PR
/// markers are one row per `(kind, ext_id, ts)`.
pub fn insert_activity_event(conn: &Connection, e: &ActivityEvent) -> Result<(), StorageError> {
    use rusqlite::OptionalExtension;
    match e.kind.dedupe() {
        Dedupe::LatestCheckout => {
            let last: Option<String> = conn
                .query_row(
                    "SELECT branch FROM activity_events WHERE repo=?1 AND kind='checkout'
                     ORDER BY ts DESC, id DESC LIMIT 1",
                    params![e.repo],
                    |r| r.get(0),
                )
                .optional()?;
            if last.as_deref() == Some(e.branch.as_str()) {
                return Ok(());
            }
        }
        Dedupe::Upsert => {
            if let Some(ext) = &e.ext_id {
                let n = conn.execute(
                    "UPDATE activity_events SET ts=?3, end_ts=?4,
                            summary=COALESCE(NULLIF(?5, ''), summary)
                     WHERE kind=?1 AND ext_id=?2",
                    params![
                        e.kind.as_str(),
                        ext,
                        ts_to_ms(e.ts),
                        e.end_ts.map(ts_to_ms),
                        e.summary
                    ],
                )?;
                if n > 0 {
                    return Ok(());
                }
            }
        }
        Dedupe::None | Dedupe::Ignore => {}
    }
    conn.execute(
        "INSERT OR IGNORE INTO activity_events
             (ts, end_ts, repo, branch, kind, ext_id, summary)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            ts_to_ms(e.ts),
            e.end_ts.map(ts_to_ms),
            e.repo,
            e.branch,
            e.kind.as_str(),
            e.ext_id,
            e.summary
        ],
    )?;
    Ok(())
}

fn activity_from_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ActivityEvent> {
    activity_from_row_at(r, 0)
}

fn activity_from_row_at(r: &rusqlite::Row<'_>, at: usize) -> rusqlite::Result<ActivityEvent> {
    let kind: String = r.get(at + 4)?;
    Ok(ActivityEvent {
        ts: ms_to_ts(r.get(at)?),
        end_ts: r.get::<_, Option<i64>>(at + 1)?.map(ms_to_ts),
        repo: r.get(at + 2)?,
        branch: r.get(at + 3)?,
        kind: ActivityKind::parse(&kind).unwrap_or(ActivityKind::Checkout),
        ext_id: r.get(at + 5)?,
        summary: r.get(at + 6)?,
    })
}

const ACTIVITY_COLS: &str = "ts, end_ts, repo, branch, kind, ext_id, summary";
const ACTIVITY_COLS_V: &str = "v.ts, v.end_ts, v.repo, v.branch, v.kind, v.ext_id, v.summary";
const VCS_KINDS: &str = "kind IN ('checkout','commit')";
const VCS_KINDS_A: &str = "a.kind IN ('checkout','commit')";

/// Every kind inside `[lo, hi)` by start ts — the digest's activity section.
pub fn activity_in_range(
    conn: &Connection,
    lo: i64,
    hi: i64,
) -> Result<Vec<ActivityEvent>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ACTIVITY_COLS} FROM activity_events
         WHERE ts >= ?1 AND ts < ?2 ORDER BY ts, id"
    ))?;
    let rows = stmt.query_map([lo, hi], activity_from_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Git kinds only inside `[lo, hi)` — anchoring's in-window evidence.
pub fn vcs_in_range(
    conn: &Connection,
    lo: i64,
    hi: i64,
) -> Result<Vec<ActivityEvent>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ACTIVITY_COLS} FROM activity_events
         WHERE {VCS_KINDS} AND ts >= ?1 AND ts < ?2 ORDER BY ts, id"
    ))?;
    let rows = stmt.query_map([lo, hi], activity_from_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Latest checkout per repo strictly before `lo` — the branch state a window
/// opens under, for repos with no checkout inside it.
pub fn branch_state_before(conn: &Connection, lo: i64) -> Result<Vec<ActivityEvent>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ACTIVITY_COLS} FROM activity_events
         WHERE kind='checkout' AND ts < ?1
           AND id IN (SELECT MAX(id) FROM activity_events
                      WHERE kind='checkout' AND ts < ?1 GROUP BY repo)"
    ))?;
    let rows = stmt.query_map([lo], activity_from_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Newest git event per repo — the settings panel's per-repo "last seen"
/// line. Ordered by repo name.
pub fn latest_vcs_event_per_repo(conn: &Connection) -> Result<Vec<ActivityEvent>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ACTIVITY_COLS} FROM activity_events
         WHERE id IN (SELECT MAX(id) FROM activity_events WHERE {VCS_KINDS} GROUP BY repo)
         ORDER BY repo"
    ))?;
    let rows = stmt.query_map([], activity_from_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Newest event per kind by end (span kinds) or start — the settings
/// panel's per-collector "last seen" line.
pub fn latest_activity_per_kind(conn: &Connection) -> Result<Vec<ActivityEvent>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ACTIVITY_COLS_V} FROM activity_events v
         WHERE NOT EXISTS (SELECT 1 FROM activity_events w WHERE w.kind = v.kind
                           AND (COALESCE(w.end_ts, w.ts), w.id) > (COALESCE(v.end_ts, v.ts), v.id))
         ORDER BY v.kind"
    ))?;
    let rows = stmt.query_map([], activity_from_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Commits whose ts falls inside any of the task's intervals, oldest first.
pub fn commits_for_task(
    conn: &Connection,
    task_id: i64,
) -> Result<Vec<ActivityEvent>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT DISTINCT {ACTIVITY_COLS_V} FROM activity_events v
         JOIN intervals i ON i.task_id=?1 AND v.ts >= i.start_ts AND v.ts < i.end_ts
         WHERE v.kind='commit' ORDER BY v.ts"
    ))?;
    let rows = stmt.query_map([task_id], activity_from_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Non-checkout events overlapping any task interval that overlaps
/// `[lo, hi)`, keyed by task, oldest first per task (one range query for the
/// whole timeline day). A span kind overlaps every interval it covers; a
/// point kind the one it lands in.
pub fn activity_in_range_by_task(
    conn: &Connection,
    lo: i64,
    hi: i64,
) -> Result<Vec<(i64, ActivityEvent)>, StorageError> {
    activity_by_task(conn, lo, hi, None, "v.kind != 'checkout'")
}

/// Non-checkout events inside `[lo, hi)` overlapping no interval at all —
/// the timeline's "unplaced" strip (a call taken between tasks, a PR
/// reviewed during a gap), oldest first.
pub fn activity_unplaced_in_range(
    conn: &Connection,
    lo: i64,
    hi: i64,
) -> Result<Vec<ActivityEvent>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {ACTIVITY_COLS_V} FROM activity_events v
         WHERE v.kind != 'checkout' AND v.ts < ?2 AND COALESCE(v.end_ts, v.ts) >= ?1
           AND NOT EXISTS (SELECT 1 FROM intervals i
                           WHERE v.ts < i.end_ts AND COALESCE(v.end_ts, v.ts) >= i.start_ts)
         ORDER BY v.ts, v.id"
    ))?;
    let rows = stmt.query_map([lo, hi], activity_from_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Every kind overlapping one task's intervals inside `[lo, hi)` — the
/// journal's activity lines.
pub fn activity_for_task_in_range(
    conn: &Connection,
    task_id: i64,
    lo: i64,
    hi: i64,
) -> Result<Vec<ActivityEvent>, StorageError> {
    Ok(activity_by_task(conn, lo, hi, Some(task_id), "1")?
        .into_iter()
        .map(|(_, e)| e)
        .collect())
}

/// Repo-aware interval join. A task's repo signal is every repo whose name
/// matches its `project` (case-insensitive) or that has vcs activity on a
/// branch carrying its `external_ref`. With a signal, only rows from those
/// repos attach (a Claude session in `chronicle` never lands under a
/// `mailer` task that merely overlapped it); repo-less rows (calls) and
/// tasks with no signal fall back to time overlap alone.
fn activity_by_task(
    conn: &Connection,
    lo: i64,
    hi: i64,
    task_id: Option<i64>,
    kinds: &str,
) -> Result<Vec<(i64, ActivityEvent)>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "WITH task_repos AS (
           SELECT DISTINCT t.id AS task_id, a.repo FROM tasks t
           JOIN activity_events a ON a.repo != ''
            AND (LOWER(a.repo) = LOWER(t.project)
                 OR (t.external_ref IS NOT NULL AND {VCS_KINDS_A}
                     AND instr(a.branch, t.external_ref) > 0))
           WHERE t.id IN (SELECT task_id FROM intervals
                          WHERE start_ts < ?2 AND end_ts >= ?1)
         )
         SELECT DISTINCT i.task_id, {ACTIVITY_COLS_V} FROM activity_events v
         JOIN intervals i ON v.ts < i.end_ts AND COALESCE(v.end_ts, v.ts) >= i.start_ts
         WHERE {kinds} AND v.ts < ?2 AND COALESCE(v.end_ts, v.ts) >= ?1
           AND (?3 IS NULL OR i.task_id = ?3)
           AND (v.repo = ''
                OR NOT EXISTS (SELECT 1 FROM task_repos r WHERE r.task_id = i.task_id)
                OR EXISTS (SELECT 1 FROM task_repos r
                           WHERE r.task_id = i.task_id AND r.repo = v.repo))
         ORDER BY i.task_id, v.ts"
    ))?;
    let rows = stmt.query_map(params![lo, hi, task_id], |r| {
        Ok((r.get::<_, i64>(0)?, activity_from_row_at(r, 1)?))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Anchoring never overwrites: first ref wins, user edits win over both.
/// True when the ref was newly set (callers chain a context fetch on it).
pub fn set_task_external_ref(
    conn: &Connection,
    task_id: i64,
    external_ref: &str,
) -> Result<bool, StorageError> {
    let n = conn.execute(
        "UPDATE tasks SET external_ref=?2 WHERE id=?1 AND external_ref IS NULL",
        params![task_id, external_ref],
    )?;
    Ok(n > 0)
}

fn insert_focus(conn: &Connection, kind: &str, e: &FocusEvent) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO events (ts, kind, app, title, pid) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![ts_to_ms(e.ts), kind, e.app, e.title, e.pid],
    )?;
    Ok(())
}

pub fn latest_event_ts(conn: &Connection) -> Result<Option<i64>, StorageError> {
    Ok(conn.query_row("SELECT MAX(ts) FROM events", [], |r| r.get(0))?)
}

pub fn latest_batch_end(conn: &Connection) -> Result<Option<i64>, StorageError> {
    Ok(conn.query_row("SELECT MAX(end_ts) FROM batches", [], |r| r.get(0))?)
}

pub fn pending_batch_count(conn: &Connection) -> Result<i64, StorageError> {
    Ok(conn.query_row(
        &format!("SELECT COUNT(*) FROM batches WHERE {ELIGIBLE}"),
        [],
        |r| r.get(0),
    )?)
}

/// Per-derive instrumentation (m27 chunk 2), written by the worker after
/// `store_derivation`; `chronicle status` and the inspector read it back.
#[derive(Debug, Clone, serde::Serialize)]
pub struct DeriveMetrics {
    pub batch_id: i64,
    pub derived_ts: i64,
    pub derive_ms: i64,
    pub prompt_tokens: i64,
    pub gen_tokens: i64,
}

pub fn record_derive_metrics(conn: &Connection, m: &DeriveMetrics) -> Result<(), StorageError> {
    conn.execute(
        "UPDATE batches SET derived_ts=?2, derive_ms=?3, prompt_tokens=?4, gen_tokens=?5 WHERE id=?1",
        params![m.batch_id, m.derived_ts, m.derive_ms, m.prompt_tokens, m.gen_tokens],
    )?;
    Ok(())
}

pub fn batch_row(conn: &Connection, id: i64) -> Result<Option<BatchRow>, StorageError> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(
            "SELECT id, start_ts, end_ts, status, attempts FROM batches WHERE id=?1",
            [id],
            |r| {
                Ok(BatchRow {
                    id: r.get(0)?,
                    start_ts: r.get(1)?,
                    end_ts: r.get(2)?,
                    status: r.get(3)?,
                    attempts: r.get(4)?,
                })
            },
        )
        .optional()?)
}

/// Owned rows for the corrections replay eval (`chronicle bench --replay`):
/// corrections since `since_ms`, plus every task, interval, and done batch
/// they could refer to.
pub struct ReplayRows {
    pub corrections: Vec<crate::replay::CorrectionRow>,
    pub intervals: Vec<crate::replay::IntervalRow>,
    pub tasks: Vec<crate::replay::TaskRow>,
    pub batches: Vec<crate::replay::BatchRow>,
}

pub fn replay_rows(conn: &Connection, since_ms: i64) -> Result<ReplayRows, StorageError> {
    use crate::replay;
    let corrections = conn
        .prepare(
            "SELECT id, ts, kind, task_id, old_label, new_label, old_project, new_project, interval_id
             FROM corrections WHERE ts >= ?1 ORDER BY ts, id",
        )?
        .query_map([since_ms], |r| {
            Ok(replay::CorrectionRow {
                id: r.get(0)?,
                ts: r.get(1)?,
                kind: r.get(2)?,
                task_id: r.get(3)?,
                old_label: r.get(4)?,
                new_label: r.get(5)?,
                old_project: r.get(6)?,
                new_project: r.get(7)?,
                interval_id: r.get(8)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    let intervals = conn
        .prepare(
            "SELECT id, task_id, batch_id, start_ts, end_ts FROM intervals ORDER BY start_ts, id",
        )?
        .query_map([], |r| {
            Ok(replay::IntervalRow {
                id: r.get(0)?,
                task_id: r.get(1)?,
                batch_id: r.get(2)?,
                start_ts: r.get(3)?,
                end_ts: r.get(4)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    let tasks = conn
        .prepare("SELECT id, label, project, source, status, created_ts, closed_ts FROM tasks")?
        .query_map([], |r| {
            Ok(replay::TaskRow {
                id: r.get(0)?,
                label: r.get(1)?,
                project: r.get(2)?,
                declared: r.get::<_, String>(3)? == "user",
                closed: r.get::<_, String>(4)? == "closed",
                created_ts: r.get(5)?,
                closed_ts: r.get(6)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    let batches = conn
        .prepare("SELECT id, start_ts, end_ts FROM batches WHERE status='done' ORDER BY start_ts")?
        .query_map([], |r| {
            Ok(replay::BatchRow {
                id: r.get(0)?,
                start_ts: r.get(1)?,
                end_ts: r.get(2)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(ReplayRows {
        corrections,
        intervals,
        tasks,
        batches,
    })
}

/// The most recently instrumented derive, if any batch has finished since 014.
pub fn last_derive(conn: &Connection) -> Result<Option<DeriveMetrics>, StorageError> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(
            "SELECT id, derived_ts, derive_ms, prompt_tokens, gen_tokens FROM batches
             WHERE derived_ts IS NOT NULL ORDER BY derived_ts DESC LIMIT 1",
            [],
            |r| {
                Ok(DeriveMetrics {
                    batch_id: r.get(0)?,
                    derived_ts: r.get(1)?,
                    derive_ms: r.get(2)?,
                    prompt_tokens: r.get(3)?,
                    gen_tokens: r.get(4)?,
                })
            },
        )
        .optional()?)
}

pub fn load_events_from(conn: &Connection, from_ms: i64) -> Result<Vec<Event>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT ts, kind, app, title, url, idle FROM events WHERE ts >= ?1 ORDER BY ts, id",
    )?;
    let rows = stmt.query_map([from_ms], |r| {
        Ok(Event {
            ts: ms_to_ts(r.get(0)?),
            kind: r.get(1)?,
            app: r.get(2)?,
            title: r.get(3)?,
            url: r.get(4)?,
            idle: r.get::<_, Option<i64>>(5)?.map(|v| v != 0),
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Replace the unbatched tail (spans with NULL batch_id at/after `t0`) with a
/// fresh sessionizer result. Closed batches are never touched.
pub fn replace_tail(
    conn: &mut Connection,
    t0: i64,
    spans: &[SpanDraft],
    batches: &[BatchDraft],
) -> Result<(), StorageError> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM spans WHERE batch_id IS NULL AND start_ts >= ?1",
        [t0],
    )?;
    let mut batch_ids: Vec<Option<i64>> = vec![None; spans.len()];
    for batch in batches {
        tx.execute(
            "INSERT INTO batches (start_ts, end_ts, status) VALUES (?1, ?2, 'pending')",
            params![ts_to_ms(batch.start), ts_to_ms(batch.end)],
        )?;
        let id = tx.last_insert_rowid();
        for slot in &mut batch_ids[batch.spans.clone()] {
            *slot = Some(id);
        }
    }
    for (span, batch_id) in spans.iter().zip(batch_ids) {
        tx.execute(
            "INSERT INTO spans (start_ts, end_ts, app, title, kind, url, batch_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                ts_to_ms(span.start),
                ts_to_ms(span.end),
                span.app,
                span.title,
                span.kind.as_str(),
                span.url,
                batch_id
            ],
        )?;
    }
    attach_tail_intervals(&tx)?;
    tx.commit()?;
    Ok(())
}

/// Intervals placed over the live tail (NULL batch) take the batch their
/// start now falls in, so derive replaces pre-pass rows batch by batch and
/// journals see user rows. Called whenever batches are created.
pub fn attach_tail_intervals(conn: &Connection) -> Result<usize, StorageError> {
    Ok(conn.execute(
        "UPDATE intervals SET batch_id = (SELECT id FROM batches b
             WHERE b.start_ts <= intervals.start_ts AND b.end_ts > intervals.start_ts)
         WHERE batch_id IS NULL AND EXISTS (SELECT 1 FROM batches b
             WHERE b.start_ts <= intervals.start_ts AND b.end_ts > intervals.start_ts)",
        [],
    )?)
}

#[derive(Debug, Clone)]
pub struct BatchRow {
    pub id: i64,
    pub start_ts: i64,
    pub end_ts: i64,
    pub status: String,
    pub attempts: i64,
}

/// Crash recovery: a `running` batch with no live worker (daemon restart)
/// counts as a failed attempt.
pub fn reset_stale_running(conn: &Connection) -> Result<usize, StorageError> {
    let n = conn.execute(
        "UPDATE batches SET status='failed' WHERE status='running'",
        [],
    )?;
    let m = conn.execute(
        "UPDATE ai_jobs SET status='failed', error='interrupted' WHERE status='running'",
        [],
    )?;
    Ok(n + m)
}

const ELIGIBLE: &str = "status IN ('pending','failed') AND attempts < 2";

/// Oldest batch still worth deriving: pending, or failed with a retry left.
pub fn next_eligible_batch(conn: &Connection) -> Result<Option<i64>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id FROM batches WHERE {ELIGIBLE} ORDER BY start_ts LIMIT 1"
    ))?;
    let mut rows = stmt.query([])?;
    Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
}

/// Worker-side claim: flips the batch to `running` and burns an attempt.
/// Returns None if the batch is not (or no longer) eligible.
pub fn claim_batch(conn: &Connection, id: i64) -> Result<Option<BatchRow>, StorageError> {
    let n = conn.execute(
        &format!(
            "UPDATE batches SET status='running', attempts=attempts+1 WHERE id=?1 AND {ELIGIBLE}"
        ),
        [id],
    )?;
    if n == 0 {
        return Ok(None);
    }
    let row = conn.query_row(
        "SELECT id, start_ts, end_ts, status, attempts FROM batches WHERE id=?1",
        [id],
        |r| {
            Ok(BatchRow {
                id: r.get(0)?,
                start_ts: r.get(1)?,
                end_ts: r.get(2)?,
                status: r.get(3)?,
                attempts: r.get(4)?,
            })
        },
    )?;
    Ok(Some(row))
}

pub fn batch_status(conn: &Connection, id: i64) -> Result<Option<String>, StorageError> {
    let mut stmt = conn.prepare("SELECT status FROM batches WHERE id=?1")?;
    let mut rows = stmt.query([id])?;
    Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
}

pub fn fail_batch(conn: &Connection, id: i64) -> Result<(), StorageError> {
    conn.execute("UPDATE batches SET status='failed' WHERE id=?1", [id])?;
    Ok(())
}

/// A claimed AI job: what the worker needs to run it.
pub struct AiJobRow {
    pub id: i64,
    pub kind: String,
    pub payload: String,
}

/// Priority at or above which an AI job means a user is actively waiting;
/// the scheduler skips its idle gate for these.
pub const AI_JOB_INTERACTIVE: i64 = 5;

/// Queue a local-LLM job (same lifecycle as batches: pending → running →
/// done/failed, one retry). Returns the job id for status polling.
pub fn enqueue_ai_job(
    conn: &Connection,
    ts: jiff::Timestamp,
    kind: &str,
    priority: i64,
    payload: &str,
) -> Result<i64, StorageError> {
    conn.execute(
        "INSERT INTO ai_jobs (kind, priority, created_ts, payload) VALUES (?1, ?2, ?3, ?4)",
        params![kind, priority, ts_to_ms(ts), payload],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Next AI job worth running with priority >= `min_priority`: highest
/// priority first, then oldest. Same eligibility rules as batches.
pub fn next_eligible_ai_job(
    conn: &Connection,
    min_priority: i64,
) -> Result<Option<i64>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id FROM ai_jobs WHERE {ELIGIBLE} AND priority >= ?1
         ORDER BY priority DESC, created_ts LIMIT 1"
    ))?;
    let mut rows = stmt.query([min_priority])?;
    Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
}

/// Worker-side claim: flips the job to `running` and burns an attempt.
pub fn claim_ai_job(conn: &Connection, id: i64) -> Result<Option<AiJobRow>, StorageError> {
    let n = conn.execute(
        &format!(
            "UPDATE ai_jobs SET status='running', attempts=attempts+1, started_ts=?2
             WHERE id=?1 AND {ELIGIBLE}"
        ),
        params![id, now_ms()],
    )?;
    if n == 0 {
        return Ok(None);
    }
    let row = conn.query_row(
        "SELECT id, kind, payload FROM ai_jobs WHERE id=?1",
        [id],
        |r| {
            Ok(AiJobRow {
                id: r.get(0)?,
                kind: r.get(1)?,
                payload: r.get(2)?,
            })
        },
    )?;
    Ok(Some(row))
}

fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

pub fn complete_ai_job(conn: &Connection, id: i64, result: &str) -> Result<(), StorageError> {
    conn.execute(
        "UPDATE ai_jobs SET status='done', result=?2, error=NULL, finished_ts=?3 WHERE id=?1",
        params![id, result, now_ms()],
    )?;
    Ok(())
}

pub fn fail_ai_job(conn: &Connection, id: i64, error: &str) -> Result<(), StorageError> {
    conn.execute(
        "UPDATE ai_jobs SET status='failed', error=?2, finished_ts=?3 WHERE id=?1",
        params![id, error, now_ms()],
    )?;
    Ok(())
}

/// The job's premise no longer holds (its batch was re-derived under other
/// tasks): terminal, never retried, not a failure.
pub fn skip_ai_job(conn: &Connection, id: i64, reason: &str) -> Result<(), StorageError> {
    conn.execute(
        "UPDATE ai_jobs SET status='skipped', error=?2, finished_ts=?3 WHERE id=?1",
        params![id, reason, now_ms()],
    )?;
    Ok(())
}

/// Newest job of `kind`, any status: (status, created, error) — the
/// settings panel's "last context fetch" line.
pub fn latest_ai_job(
    conn: &Connection,
    kind: &str,
) -> Result<Option<(String, jiff::Timestamp, Option<String>)>, StorageError> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(
            "SELECT status, created_ts, error FROM ai_jobs WHERE kind=?1
             ORDER BY id DESC LIMIT 1",
            [kind],
            |r| Ok((r.get(0)?, ms_to_ts(r.get(1)?), r.get(2)?)),
        )
        .optional()?)
}

/// (status, result) for a job, for UI polling.
pub fn ai_job_status(
    conn: &Connection,
    id: i64,
) -> Result<Option<(String, Option<String>)>, StorageError> {
    // Failed jobs carry their reason in `error`; surface it through the same
    // slot so pollers can show it.
    let mut stmt =
        conn.prepare("SELECT status, COALESCE(result, error) FROM ai_jobs WHERE id=?1")?;
    let mut rows = stmt.query([id])?;
    Ok(rows
        .next()?
        .map(|r| Ok::<_, rusqlite::Error>((r.get(0)?, r.get(1)?)))
        .transpose()?)
}

/// Canonical single-task payload (description and fetch_context jobs);
/// stored and matched verbatim so the UI can ask "is one queued for this
/// task" by equality.
pub fn task_description_payload(task_id: i64) -> String {
    format!("{{\"task_id\":{task_id}}}")
}

/// True while a description job for this task is queued or running.
pub fn pending_description_job(conn: &Connection, task_id: i64) -> Result<bool, StorageError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ai_jobs
         WHERE kind='task_description' AND status IN ('pending','running') AND payload=?1",
        [task_description_payload(task_id)],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// True while a context-fetch job for this task is queued or running.
pub fn pending_fetch_context_job(conn: &Connection, task_id: i64) -> Result<bool, StorageError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ai_jobs
         WHERE kind='fetch_context' AND status IN ('pending','running') AND payload=?1",
        [task_description_payload(task_id)],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

pub fn batch_spans(conn: &Connection, id: i64) -> Result<Vec<SpanDraft>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT start_ts, end_ts, app, title, kind, url FROM spans
         WHERE batch_id = ?1 ORDER BY start_ts, id",
    )?;
    let rows = stmt.query_map([id], |r| {
        Ok(SpanDraft {
            start: ms_to_ts(r.get(0)?),
            end: ms_to_ts(r.get(1)?),
            app: r.get(2)?,
            title: r.get(3)?,
            kind: SpanKind::parse(&r.get::<_, String>(4)?).unwrap_or(SpanKind::Focus),
            url: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// `None` deletes the key. Daemon status flags the UI surfaces (e.g.
/// `server_error` when the AW endpoint port is taken).
pub fn set_meta(conn: &Connection, key: &str, value: Option<&str>) -> Result<(), StorageError> {
    match value {
        Some(v) => {
            conn.execute(
                "INSERT INTO meta (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                params![key, v],
            )?;
        }
        None => {
            conn.execute("DELETE FROM meta WHERE key=?1", [key])?;
        }
    }
    Ok(())
}

pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>, StorageError> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?)
}

/// Retention: delete rows fully older than `cutoff_ms`, at most `batch` rows
/// per statement (one implicit transaction each) so a large backlog never
/// holds a long write lock. FTS indexes stay in sync via the AFTER DELETE
/// triggers. Corrections are user teaching data and are never pruned; an
/// interval or task referenced by one is kept as its context (and the FK
/// requires it), user-declared tasks are never pruned, and a batch survives
/// while spans or intervals still reference it.
pub fn prune(conn: &Connection, cutoff_ms: i64, batch: usize) -> Result<u64, StorageError> {
    const STMTS: [&str; 10] = [
        "DELETE FROM events WHERE id IN (SELECT id FROM events WHERE ts < ?1 LIMIT ?2)",
        "DELETE FROM activity_events WHERE id IN (SELECT id FROM activity_events WHERE ts < ?1 LIMIT ?2)",
        "DELETE FROM spans WHERE id IN (SELECT id FROM spans WHERE end_ts < ?1 LIMIT ?2)",
        "DELETE FROM intervals WHERE id IN (SELECT id FROM intervals WHERE end_ts < ?1 \
         AND id NOT IN (SELECT interval_id FROM corrections WHERE interval_id IS NOT NULL) \
         LIMIT ?2)",
        "DELETE FROM tasks WHERE id IN (SELECT id FROM tasks WHERE created_ts < ?1 \
         AND source='derived' \
         AND id NOT IN (SELECT task_id FROM intervals) \
         AND id NOT IN (SELECT task_id FROM corrections) LIMIT ?2)",
        "DELETE FROM batches WHERE id IN (SELECT id FROM batches WHERE end_ts < ?1 \
         AND id NOT IN (SELECT batch_id FROM spans WHERE batch_id IS NOT NULL) \
         AND id NOT IN (SELECT batch_id FROM intervals WHERE batch_id IS NOT NULL) LIMIT ?2)",
        "DELETE FROM chat_messages WHERE id IN (SELECT id FROM chat_messages WHERE ts < ?1 LIMIT ?2)",
        "DELETE FROM conversations WHERE id IN (SELECT id FROM conversations WHERE created_ts < ?1 \
         AND id NOT IN (SELECT conversation_id FROM chat_messages WHERE conversation_id IS NOT NULL) \
         LIMIT ?2)",
        "DELETE FROM ai_jobs WHERE id IN (SELECT id FROM ai_jobs WHERE created_ts < ?1 \
         AND status IN ('done','failed','skipped') LIMIT ?2)",
        "DELETE FROM narratives WHERE rowid IN (SELECT rowid FROM narratives WHERE created_ts < ?1 \
         LIMIT ?2)",
    ];
    let mut total = 0u64;
    for sql in STMTS {
        loop {
            let n = conn.execute(sql, params![cutoff_ms, batch as i64])?;
            total += n as u64;
            if n < batch {
                break;
            }
        }
    }
    if total > 0 {
        // Hand freed pages back a few at a time (auto_vacuum=INCREMENTAL),
        // then checkpoint so the WAL stops carrying the deleted pages. A
        // concurrent reader (UI / chat worker) makes the checkpoint partial,
        // not an error.
        conn.execute_batch("PRAGMA incremental_vacuum(256)")?;
        conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    }
    Ok(total)
}

/// Open tasks offered to the model for linking: user-declared first (stable
/// declaration order), then derived-open by most recent interval, `cap` total.
/// Every project name ever put on a task, in first-seen order (by the id of
/// the first task carrying it). Stable across days — new projects only
/// append — so a UI can hand out identity colours by position.
pub fn project_order(conn: &Connection) -> Result<Vec<String>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT project FROM tasks
         WHERE project IS NOT NULL AND TRIM(project) <> ''
         GROUP BY project ORDER BY MIN(id)",
    )?;
    let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn open_tasks(conn: &Connection, cap: usize) -> Result<Vec<OpenTask>, StorageError> {
    let mut out = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT id, label, project FROM tasks
         WHERE status='open' AND source='user' ORDER BY created_ts, id",
    )?;
    let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    for row in rows {
        let (id, label, project) = row?;
        out.push(OpenTask {
            id,
            label,
            project,
            declared: true,
        });
    }
    let mut stmt = conn.prepare(
        "SELECT t.id, t.label, t.project FROM tasks t
         JOIN intervals i ON i.task_id = t.id
         WHERE t.status='open' AND t.source='derived'
         GROUP BY t.id ORDER BY MAX(i.end_ts) DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([cap as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    for row in rows {
        if out.len() >= cap {
            break;
        }
        let (id, label, project) = row?;
        out.push(OpenTask {
            id,
            label,
            project,
            declared: false,
        });
    }
    Ok(out)
}

/// Last-n closed tasks, newest close first (the UI's "recently closed"
/// reopen list — covers accidental closes and autoclosed work resuming).
pub fn recently_closed(conn: &Connection, n: usize) -> Result<Vec<OpenTask>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT id, label, project, source='user' FROM tasks
         WHERE status='closed' ORDER BY closed_ts DESC, id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([n as i64], |r| {
        Ok(OpenTask {
            id: r.get(0)?,
            label: r.get(1)?,
            project: r.get(2)?,
            declared: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn reopen_task(conn: &Connection, task_id: i64) -> Result<(), StorageError> {
    conn.execute(
        "UPDATE tasks SET status='open', closed_ts=NULL WHERE id=?1",
        [task_id],
    )?;
    Ok(())
}

/// Declare a task the user is working on. Returns its id.
pub fn insert_user_task(
    conn: &Connection,
    ts: jiff::Timestamp,
    label: &str,
    project: Option<&str>,
) -> Result<i64, StorageError> {
    conn.execute(
        "INSERT INTO tasks (label, project, status, source, created_ts)
         VALUES (?1, ?2, 'open', 'user', ?3)",
        params![label, project, ts_to_ms(ts)],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn close_task(
    conn: &Connection,
    ts: jiff::Timestamp,
    task_id: i64,
) -> Result<(), StorageError> {
    conn.execute(
        "UPDATE tasks SET status='closed', closed_ts=?1 WHERE id=?2",
        params![ts_to_ms(ts), task_id],
    )?;
    Ok(())
}

/// `None` clears the description.
pub fn set_task_description(
    conn: &Connection,
    task_id: i64,
    description: Option<&str>,
) -> Result<(), StorageError> {
    conn.execute(
        "UPDATE tasks SET description=?1 WHERE id=?2",
        params![description, task_id],
    )?;
    Ok(())
}

/// Closed tasks with no description yet, newest close first, for the
/// backfill command (a partial run covers the most recent history; rerunning
/// naturally continues since described tasks drop out).
pub fn closed_tasks_missing_description(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<OpenTask>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT id, label, project, source='user' FROM tasks
         WHERE status='closed' AND (description IS NULL OR description='')
         ORDER BY closed_ts DESC, id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |r| {
        Ok(OpenTask {
            id: r.get(0)?,
            label: r.get(1)?,
            project: r.get(2)?,
            declared: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Cache one generated range narrative, replacing any prior text for the range.
pub fn upsert_narrative(
    conn: &Connection,
    lo: i64,
    hi: i64,
    data_hash: i64,
    ts: jiff::Timestamp,
    text: &str,
) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO narratives (range_lo, range_hi, data_hash, text, created_ts)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(range_lo, range_hi) DO UPDATE
           SET data_hash=excluded.data_hash, text=excluded.text, created_ts=excluded.created_ts",
        params![lo, hi, data_hash, text, ts_to_ms(ts)],
    )?;
    Ok(())
}

/// (data_hash, text) of the cached narrative for a range, if any.
pub fn get_narrative(
    conn: &Connection,
    lo: i64,
    hi: i64,
) -> Result<Option<(i64, String)>, StorageError> {
    let mut stmt =
        conn.prepare("SELECT data_hash, text FROM narratives WHERE range_lo=?1 AND range_hi=?2")?;
    let mut rows = stmt.query([lo, hi])?;
    Ok(rows
        .next()?
        .map(|r| Ok::<_, rusqlite::Error>((r.get(0)?, r.get(1)?)))
        .transpose()?)
}

/// One journal row: derived prose for one batch's slice of a task.
pub struct JournalEntry {
    pub id: i64,
    pub batch_id: i64,
    pub start_ts: i64,
    pub end_ts: i64,
    pub entry: String,
}

/// Latest "where I am / what's next" for a task.
pub struct Checkpoint {
    pub ts: i64,
    pub state: String,
    pub next_steps: String,
}

/// Replace the task's MCP-fetched context bundle (one per task).
pub fn upsert_task_context(
    conn: &Connection,
    task_id: i64,
    source: &str,
    ts: jiff::Timestamp,
    content: &str,
) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO task_context (task_id, source, fetched_ts, content)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(task_id) DO UPDATE
           SET source=excluded.source, fetched_ts=excluded.fetched_ts,
               content=excluded.content",
        params![task_id, source, ts_to_ms(ts), content],
    )?;
    Ok(())
}

/// (fetched_ts, content) of the task's context bundle, if fetched.
pub fn task_context(
    conn: &Connection,
    task_id: i64,
) -> Result<Option<(i64, String)>, StorageError> {
    let mut stmt = conn.prepare("SELECT fetched_ts, content FROM task_context WHERE task_id=?1")?;
    let mut rows = stmt.query([task_id])?;
    Ok(rows
        .next()?
        .map(|r| Ok::<_, rusqlite::Error>((r.get(0)?, r.get(1)?)))
        .transpose()?)
}

/// Append the batch's journal entry for a task; a re-derived batch replaces
/// its earlier entry (mirrors `store_derivation`'s replace semantics).
pub fn insert_journal_entry(
    conn: &Connection,
    task_id: i64,
    batch_id: i64,
    start_ts: i64,
    end_ts: i64,
    entry: &str,
    evidence: &str,
) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO journal_entries (task_id, batch_id, start_ts, end_ts, entry, evidence)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(task_id, batch_id) DO UPDATE
           SET start_ts=excluded.start_ts, end_ts=excluded.end_ts,
               entry=excluded.entry, evidence=excluded.evidence",
        params![task_id, batch_id, start_ts, end_ts, entry, evidence],
    )?;
    Ok(())
}

/// Journal evidence: span lines, interval ids, and the covered `(lo, hi)`.
pub type BatchEvidence = (String, Vec<i64>, i64, i64);

/// The task's batch-scoped journal evidence: focus-span lines overlapping the
/// task's intervals in this batch, plus the interval ids and the covered
/// window. None when the batch no longer holds intervals for the task.
pub fn task_batch_evidence(
    conn: &Connection,
    task_id: i64,
    batch_id: i64,
) -> Result<Option<BatchEvidence>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT id, start_ts, end_ts FROM intervals
         WHERE task_id=?1 AND batch_id=?2 ORDER BY start_ts",
    )?;
    let ivs: Vec<(i64, i64, i64)> = stmt
        .query_map(params![task_id, batch_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<Result<_, _>>()?;
    let (Some(lo), Some(hi)) = (
        ivs.iter().map(|iv| iv.1).min(),
        ivs.iter().map(|iv| iv.2).max(),
    ) else {
        return Ok(None);
    };
    let mut ctx = String::new();
    let mut stmt = tx_spans_for_intervals(conn)?;
    let mut rows = stmt.query(params![task_id, batch_id])?;
    while let Some(row) = rows.next()? {
        let (app, title): (String, String) = (row.get(0)?, row.get(1)?);
        if ctx.len() + app.len() + title.len() + 2 > 2000 {
            break;
        }
        ctx.push_str(&app);
        ctx.push(' ');
        ctx.push_str(&title);
        ctx.push('\n');
    }
    Ok(Some((ctx, ivs.iter().map(|iv| iv.0).collect(), lo, hi)))
}

fn tx_spans_for_intervals(conn: &Connection) -> Result<rusqlite::Statement<'_>, StorageError> {
    Ok(conn.prepare(
        "SELECT DISTINCT s.app, s.title FROM spans s
         JOIN intervals i ON i.task_id=?1 AND i.batch_id=?2
           AND s.start_ts < i.end_ts AND s.end_ts > i.start_ts
         WHERE s.kind='focus' ORDER BY s.app, s.title",
    )?)
}

/// Last `n` journal entries for a task, chronological (oldest of the tail
/// first) so prompts and UI read forward.
pub fn journal_tail(
    conn: &Connection,
    task_id: i64,
    n: usize,
) -> Result<Vec<JournalEntry>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT id, batch_id, start_ts, end_ts, entry FROM (
             SELECT id, batch_id, start_ts, end_ts, entry FROM journal_entries
             WHERE task_id=?1 ORDER BY start_ts DESC, id DESC LIMIT ?2
         ) ORDER BY start_ts, id",
    )?;
    let rows = stmt.query_map(params![task_id, n as i64], |r| {
        Ok(JournalEntry {
            id: r.get(0)?,
            batch_id: r.get(1)?,
            start_ts: r.get(2)?,
            end_ts: r.get(3)?,
            entry: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// True while a journal job for this batch's slice of the task is queued or
/// running (dedupe on batch retry).
pub fn pending_journal_job(
    conn: &Connection,
    task_id: i64,
    batch_id: i64,
) -> Result<bool, StorageError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ai_jobs
         WHERE kind='journal' AND status IN ('pending','running') AND payload=?1",
        [journal_payload(task_id, batch_id)],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Canonical payload for a journal job; stored and matched verbatim.
pub fn journal_payload(task_id: i64, batch_id: i64) -> String {
    format!("{{\"task_id\":{task_id},\"batch_id\":{batch_id}}}")
}

/// Rewrite one journal entry's text, logging the edit as a correction
/// (kind='journal'). The empty ctx keeps workspace edits out of the
/// rename-similarity FTS, which matches on span context.
pub fn update_journal_entry(
    conn: &mut Connection,
    ts: jiff::Timestamp,
    entry_id: i64,
    text: &str,
) -> Result<(), StorageError> {
    let tx = conn.transaction()?;
    let (task_id, old): (i64, String) = tx.query_row(
        "SELECT task_id, entry FROM journal_entries WHERE id=?1",
        [entry_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if old != text {
        tx.execute(
            "UPDATE journal_entries SET entry=?2 WHERE id=?1",
            params![entry_id, text],
        )?;
        tx.execute(
            "INSERT INTO corrections (ts, task_id, old_label, new_label, ctx, kind)
             VALUES (?1, ?2, ?3, ?4, '', 'journal')",
            params![ts_to_ms(ts), task_id, old, text],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Rewrite the task's checkpoint text, logging the edit as a correction
/// (kind='checkpoint'; next_steps ride the project columns). The checkpoint's
/// own ts stays put so an edit doesn't resurface the resume card.
pub fn update_checkpoint(
    conn: &mut Connection,
    ts: jiff::Timestamp,
    task_id: i64,
    state: &str,
    next_steps: &str,
) -> Result<(), StorageError> {
    let tx = conn.transaction()?;
    let (old_state, old_next): (String, String) = tx.query_row(
        "SELECT state, next_steps FROM checkpoints WHERE task_id=?1",
        [task_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    if old_state != state || old_next != next_steps {
        tx.execute(
            "UPDATE checkpoints SET state=?2, next_steps=?3 WHERE task_id=?1",
            params![task_id, state, next_steps],
        )?;
        tx.execute(
            "INSERT INTO corrections
                 (ts, task_id, old_label, new_label, old_project, new_project, ctx, kind)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, '', 'checkpoint')",
            params![
                ts_to_ms(ts),
                task_id,
                old_state,
                state,
                old_next,
                next_steps
            ],
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn upsert_checkpoint(
    conn: &Connection,
    task_id: i64,
    ts: jiff::Timestamp,
    state: &str,
    next_steps: &str,
) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO checkpoints (task_id, ts, state, next_steps)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(task_id) DO UPDATE
           SET ts=excluded.ts, state=excluded.state, next_steps=excluded.next_steps",
        params![task_id, ts_to_ms(ts), state, next_steps],
    )?;
    Ok(())
}

pub fn get_checkpoint(conn: &Connection, task_id: i64) -> Result<Option<Checkpoint>, StorageError> {
    let mut stmt =
        conn.prepare("SELECT ts, state, next_steps FROM checkpoints WHERE task_id=?1")?;
    let mut rows = stmt.query([task_id])?;
    Ok(rows
        .next()?
        .map(|r| {
            Ok::<_, rusqlite::Error>(Checkpoint {
                ts: r.get(0)?,
                state: r.get(1)?,
                next_steps: r.get(2)?,
            })
        })
        .transpose()?)
}

/// Tasks with interval activity since their checkpoint (or never
/// checkpointed), bounded to activity in the 24 h before `since_ms` so a
/// stale daemon restart doesn't fan out over old open tasks.
pub fn tasks_needing_checkpoint(
    conn: &Connection,
    since_ms: i64,
) -> Result<Vec<i64>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT i.task_id FROM intervals i
         LEFT JOIN checkpoints c ON c.task_id = i.task_id
         WHERE i.end_ts > COALESCE(c.ts, 0)
           AND i.start_ts < ?1 AND i.end_ts > ?1 - 86400000",
    )?;
    let rows = stmt.query_map([since_ms], |r| r.get(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Resume-card row: `(task_id, label, external_ref, checkpoint)`.
pub type ResumeCheckpoint = (i64, String, Option<String>, Checkpoint);

/// Newest checkpoint written after `since_ms`, with its task's identity —
/// the Home resume card. A task in `prefer` (the day's intent) wins when
/// several checkpoints qualify.
pub fn latest_checkpoint_since(
    conn: &Connection,
    since_ms: i64,
    prefer: &[i64],
) -> Result<Option<ResumeCheckpoint>, StorageError> {
    // 0 is never a task id, so an empty `prefer` is a list that matches
    // nothing and the order falls back to newest-first.
    let mut ids = vec!["0".to_owned()];
    ids.extend(prefer.iter().map(i64::to_string));
    let mut stmt = conn.prepare(&format!(
        "SELECT c.task_id, t.label, t.external_ref, c.ts, c.state, c.next_steps
         FROM checkpoints c JOIN tasks t ON t.id = c.task_id
         WHERE c.ts > ?1
         ORDER BY c.task_id IN ({}) DESC, c.ts DESC LIMIT 1",
        ids.join(",")
    ))?;
    let mut rows = stmt.query([since_ms])?;
    Ok(rows
        .next()?
        .map(|r| {
            Ok::<_, rusqlite::Error>((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                Checkpoint {
                    ts: r.get(3)?,
                    state: r.get(4)?,
                    next_steps: r.get(5)?,
                },
            ))
        })
        .transpose()?)
}

/// One task's slice of a standup digest: the day's journal entries plus the
/// checkpoint, when one was written during or after the day.
pub struct StandupDigestRow {
    pub task_id: i64,
    pub label: String,
    pub project: Option<String>,
    pub external_ref: Option<String>,
    pub entries: Vec<JournalEntry>,
    pub checkpoint: Option<Checkpoint>,
}

/// Journal entries overlapping `[lo, hi)` grouped by task, chronological
/// within each task — the raw material for a standup draft.
pub fn standup_digest(
    conn: &Connection,
    lo: i64,
    hi: i64,
) -> Result<Vec<StandupDigestRow>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT j.task_id, t.label, t.project, t.external_ref,
                j.id, j.batch_id, j.start_ts, j.end_ts, j.entry
         FROM journal_entries j JOIN tasks t ON t.id = j.task_id
         WHERE j.start_ts < ?2 AND j.end_ts > ?1
         ORDER BY j.task_id, j.start_ts, j.id",
    )?;
    let mut rows = stmt.query([lo, hi])?;
    let mut out: Vec<StandupDigestRow> = Vec::new();
    while let Some(r) = rows.next()? {
        let task_id: i64 = r.get(0)?;
        let entry = JournalEntry {
            id: r.get(4)?,
            batch_id: r.get(5)?,
            start_ts: r.get(6)?,
            end_ts: r.get(7)?,
            entry: r.get(8)?,
        };
        match out.last_mut() {
            Some(row) if row.task_id == task_id => row.entries.push(entry),
            _ => out.push(StandupDigestRow {
                task_id,
                label: r.get(1)?,
                project: r.get(2)?,
                external_ref: r.get(3)?,
                entries: vec![entry],
                checkpoint: None,
            }),
        }
    }
    for row in &mut out {
        row.checkpoint = get_checkpoint(conn, row.task_id)?.filter(|c| c.ts >= lo);
    }
    Ok(out)
}

/// Cache the day's standup draft, replacing any prior text for the day.
pub fn upsert_standup_draft(
    conn: &Connection,
    day: &str,
    ts: jiff::Timestamp,
    content: &str,
) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO standup_drafts (day, ts, content) VALUES (?1, ?2, ?3)
         ON CONFLICT(day) DO UPDATE SET ts=excluded.ts, content=excluded.content",
        params![day, ts_to_ms(ts), content],
    )?;
    Ok(())
}

/// (ts, content) of the cached standup draft for a day, if drafted.
pub fn get_standup_draft(
    conn: &Connection,
    day: &str,
) -> Result<Option<(i64, String)>, StorageError> {
    let mut stmt = conn.prepare("SELECT ts, content FROM standup_drafts WHERE day=?1")?;
    let mut rows = stmt.query([day])?;
    Ok(rows
        .next()?
        .map(|r| Ok::<_, rusqlite::Error>((r.get(0)?, r.get(1)?)))
        .transpose()?)
}

/// True while a standup job for this day is queued or running (dedupe on
/// every daemon tick).
pub fn pending_standup_job(conn: &Connection, day: &str) -> Result<bool, StorageError> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM ai_jobs
         WHERE kind='standup' AND status IN ('pending','running') AND payload=?1",
        [standup_payload(day)],
        |r| r.get(0),
    )?;
    Ok(n > 0)
}

/// Canonical payload for a standup job; stored and matched verbatim.
pub fn standup_payload(day: &str) -> String {
    format!("{{\"day\":{day:?}}}")
}

/// The task's chat thread, creating it on first use (at most one per task;
/// the partial unique index makes re-entry return the same conversation).
pub fn conversation_for_task(
    conn: &Connection,
    task_id: i64,
    now: jiff::Timestamp,
) -> Result<i64, StorageError> {
    conn.execute(
        "INSERT INTO conversations (created_ts, task_id) VALUES (?1, ?2)
         ON CONFLICT(task_id) WHERE task_id IS NOT NULL DO NOTHING",
        params![ts_to_ms(now), task_id],
    )?;
    Ok(conn.query_row(
        "SELECT id FROM conversations WHERE task_id=?1",
        [task_id],
        |r| r.get(0),
    )?)
}

/// The task a conversation is scoped to, if any.
pub fn conversation_task(
    conn: &Connection,
    conversation_id: i64,
) -> Result<Option<i64>, StorageError> {
    Ok(conn.query_row(
        "SELECT task_id FROM conversations WHERE id=?1",
        [conversation_id],
        |r| r.get(0),
    )?)
}

/// Newest conversation not scoped to a task (clearing a task scope returns
/// the user to their general thread).
pub fn latest_general_conversation(conn: &Connection) -> Result<Option<i64>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT id FROM conversations WHERE task_id IS NULL
         ORDER BY created_ts DESC, id DESC LIMIT 1",
    )?;
    let mut rows = stmt.query([])?;
    Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
}

/// Close open derived tasks whose last interval ended over `days` days ago
/// (candidate-list hygiene; declared tasks only close by hand). Closed is
/// not deleted — history stays, retention prune owns deletion.
pub fn autoclose_stale_tasks(
    conn: &Connection,
    now: jiff::Timestamp,
    days: u32,
) -> Result<usize, StorageError> {
    let now_ms = ts_to_ms(now);
    let cutoff = now_ms - i64::from(days) * 86_400_000;
    Ok(conn.execute(
        "UPDATE tasks SET status='closed', closed_ts=?1
         WHERE status='open' AND source='derived'
           AND id NOT IN (SELECT task_id FROM intervals WHERE end_ts > ?2)",
        params![now_ms, cutoff],
    )?)
}

/// Guard shared by re-derive and reassign: a derived task that lost its last
/// interval and has no corrections has no reason to exist.
const DELETE_ORPHAN_TASKS: &str = "DELETE FROM tasks WHERE source='derived' \
     AND id NOT IN (SELECT task_id FROM intervals) \
     AND id NOT IN (SELECT task_id FROM corrections)";

/// Replace the batch's derived intervals and mark it done. `New` slots become
/// task identity rows (created_ts = their earliest interval); derived tasks
/// orphaned by the replace are removed. Replacing keeps a retried batch
/// idempotent. Returns the stored intervals as `(task_id, start_ms, end_ms)`
/// for the caller's anchoring pass.
pub fn store_derivation(
    conn: &mut Connection,
    batch_id: i64,
    slots: &[TaskSlot],
    intervals: &[NewInterval],
) -> Result<Vec<(i64, i64, i64)>, StorageError> {
    let tx = conn.transaction()?;
    let (batch_lo, batch_hi): (i64, i64) = tx.query_row(
        "SELECT start_ts, end_ts FROM batches WHERE id=?1",
        [batch_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    // The model's answer replaces the batch's earlier derived rows and the
    // pre-pass's provisional ones (attached, or still over the tail with a
    // start inside the batch); what the user placed by hand stays and the
    // model's intervals are clipped around it.
    tx.execute(
        "DELETE FROM intervals WHERE source <> 'user'
           AND (batch_id=?1 OR (batch_id IS NULL AND start_ts >= ?2 AND start_ts < ?3))",
        params![batch_id, batch_lo, batch_hi],
    )?;
    let user_rows: Vec<(i64, i64)> = tx
        .prepare("SELECT start_ts, end_ts FROM intervals WHERE source='user' AND start_ts < ?2 AND end_ts > ?1 ORDER BY start_ts")?
        .query_map([batch_lo, batch_hi], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut ids: Vec<Option<i64>> = Vec::with_capacity(slots.len());
    for (i, slot) in slots.iter().enumerate() {
        match slot {
            TaskSlot::Existing(id) => ids.push(Some(*id)),
            TaskSlot::New { label, project } => {
                let created = intervals
                    .iter()
                    .filter(|iv| iv.slot == i)
                    .map(|iv| ts_to_ms(iv.start_ts))
                    .min();
                // A slot no interval survived (clamp dropped them) creates nothing.
                let Some(created) = created else {
                    ids.push(None);
                    continue;
                };
                tx.execute(
                    "INSERT INTO tasks (label, project, status, source, created_ts)
                     VALUES (?1, ?2, 'open', 'derived', ?3)",
                    params![label, project, created],
                )?;
                ids.push(Some(tx.last_insert_rowid()));
            }
        }
    }
    let mut stored = Vec::with_capacity(intervals.len());
    for iv in intervals {
        let Some(Some(task_id)) = ids.get(iv.slot) else {
            continue;
        };
        for (s, e) in subtract_ranges(ts_to_ms(iv.start_ts), ts_to_ms(iv.end_ts), &user_rows) {
            tx.execute(
                "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![task_id, batch_id, s, e, iv.confidence],
            )?;
            stored.push((*task_id, s, e));
        }
    }
    tx.execute(DELETE_ORPHAN_TASKS, [])?;
    tx.execute("UPDATE batches SET status='done' WHERE id=?1", [batch_id])?;
    tx.commit()?;
    Ok(stored)
}

/// `[lo, hi)` minus the sorted `blockers`, as the non-empty pieces left.
fn subtract_ranges(lo: i64, hi: i64, blockers: &[(i64, i64)]) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    let mut cur = lo;
    for &(s, e) in blockers {
        if e <= cur || s >= hi {
            continue;
        }
        if s > cur {
            out.push((cur, s));
        }
        cur = cur.max(e);
    }
    if cur < hi {
        out.push((cur, hi));
    }
    out
}

/// Focus spans overlapping any of the task's intervals, as "app title" lines.
/// Used as the FTS-searchable snapshot on corrections and as the evidence fed
/// to the task-description prompt.
pub fn task_evidence_text(conn: &Connection, task_id: i64) -> Result<String, StorageError> {
    task_span_ctx(conn, task_id)
}

/// Focus spans overlapping any of the task's intervals, as "app title" lines
/// (FTS-searchable snapshot of what the corrected work looked like).
fn task_span_ctx(tx: &Connection, task_id: i64) -> Result<String, StorageError> {
    let mut ctx = String::new();
    let mut stmt = tx.prepare(
        "SELECT DISTINCT s.app, s.title FROM spans s
         JOIN intervals i ON i.task_id=?1 AND s.start_ts < i.end_ts AND s.end_ts > i.start_ts
         WHERE s.kind='focus' ORDER BY s.app, s.title",
    )?;
    let mut rows = stmt.query([task_id])?;
    while let Some(row) = rows.next()? {
        let (app, title): (String, String) = (row.get(0)?, row.get(1)?);
        if ctx.len() + app.len() + title.len() + 2 > 2000 {
            break;
        }
        ctx.push_str(&app);
        ctx.push(' ');
        ctx.push_str(&title);
        ctx.push('\n');
    }
    Ok(ctx)
}

/// Record a user edit of a task's label/project ('rename') and apply it to
/// the identity row. The correction stores an FTS-searchable snapshot of the
/// span context the task's intervals covered, so future batches with similar
/// activity can retrieve it.
pub fn insert_correction(
    conn: &mut Connection,
    ts: jiff::Timestamp,
    task_id: i64,
    new_label: &str,
    new_project: Option<&str>,
) -> Result<(), StorageError> {
    let tx = conn.transaction()?;
    let (old_label, old_project): (String, Option<String>) = tx.query_row(
        "SELECT label, project FROM tasks WHERE id=?1",
        [task_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let ctx = task_span_ctx(&tx, task_id)?;
    tx.execute(
        "INSERT INTO corrections (ts, task_id, old_label, new_label, old_project, new_project, ctx, kind)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'rename')",
        params![
            ts_to_ms(ts),
            task_id,
            old_label,
            new_label,
            old_project,
            new_project,
            ctx
        ],
    )?;
    tx.execute(
        "UPDATE tasks SET label=?1, project=?2 WHERE id=?3",
        params![new_label, new_project, task_id],
    )?;
    tx.commit()?;
    Ok(())
}

/// Move intervals to another task ('reassign' correction) in one transaction,
/// so a mid-move failure can't leave a session split across two tasks. Each
/// old task's label → new task's label pair plus the interval's span context
/// become teaching data; orphaned derived source tasks are removed.
pub fn reassign_intervals(
    conn: &mut Connection,
    ts: jiff::Timestamp,
    interval_ids: &[i64],
    to_task: i64,
) -> Result<(), StorageError> {
    let tx = conn.transaction()?;
    for &interval_id in interval_ids {
        let (from_task, start_ts, end_ts): (i64, i64, i64) = tx.query_row(
            "SELECT task_id, start_ts, end_ts FROM intervals WHERE id=?1",
            [interval_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if from_task == to_task {
            continue;
        }
        let ident = |id: i64| -> Result<(String, Option<String>), rusqlite::Error> {
            tx.query_row("SELECT label, project FROM tasks WHERE id=?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
        };
        let (old_label, old_project) = ident(from_task)?;
        let (new_label, new_project) = ident(to_task)?;
        let ctx = span_ctx(&tx, start_ts, end_ts)?;
        tx.execute(
            "INSERT INTO corrections (ts, task_id, old_label, new_label, old_project, new_project, ctx, kind, interval_id)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'reassign', ?8)",
            params![
                ts_to_ms(ts),
                to_task,
                old_label,
                new_label,
                old_project,
                new_project,
                ctx,
                interval_id
            ],
        )?;
        tx.execute(
            "UPDATE intervals SET task_id=?1, source='user' WHERE id=?2",
            params![to_task, interval_id],
        )?;
    }
    tx.execute(DELETE_ORPHAN_TASKS, [])?;
    tx.commit()?;
    Ok(())
}

/// Focus app/title lines overlapping `[start_ts, end_ts)`: the FTS-searchable
/// context a correction stores (capped so one row stays small).
fn span_ctx(conn: &Connection, start_ts: i64, end_ts: i64) -> Result<String, StorageError> {
    let mut ctx = String::new();
    let mut stmt = conn.prepare(
        "SELECT DISTINCT app, title FROM spans
         WHERE kind='focus' AND start_ts < ?1 AND end_ts > ?2 ORDER BY app, title",
    )?;
    let mut rows = stmt.query([end_ts, start_ts])?;
    while let Some(row) = rows.next()? {
        let (app, title): (String, String) = (row.get(0)?, row.get(1)?);
        if ctx.len() + app.len() + title.len() + 2 > 2000 {
            break;
        }
        ctx.push_str(&app);
        ctx.push(' ');
        ctx.push_str(&title);
        ctx.push('\n');
    }
    Ok(ctx)
}

/// A stretch of focus time no interval covers: consecutive unassigned focus
/// spans with gaps under the caller's threshold, plus its app/title mix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnassignedRun {
    pub start_ts: i64,
    pub end_ts: i64,
    /// Focus ms inside the run (gaps excluded).
    pub ms: i64,
    /// `(app, title, ms)`, largest first.
    pub lines: Vec<(String, String, i64)>,
}

/// Unassigned focus inside `[lo, hi)` folded into runs (gap < `gap_ms`
/// joins), oldest first — the triage view's cards. Contiguous unassigned
/// work is one decision far more often than one app/title cluster is.
pub fn unassigned_runs(
    conn: &Connection,
    lo: i64,
    hi: i64,
    gap_ms: i64,
) -> Result<Vec<UnassignedRun>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT start_ts, MIN(end_ts, ?2), app, title FROM spans s
         WHERE kind='focus' AND start_ts >= ?1 AND start_ts < ?2
           AND NOT EXISTS (SELECT 1 FROM intervals i
                           WHERE i.start_ts < s.end_ts AND i.end_ts > s.start_ts)
         ORDER BY start_ts, id",
    )?;
    let mut runs: Vec<UnassignedRun> = Vec::new();
    let mut rows = stmt.query([lo, hi])?;
    while let Some(row) = rows.next()? {
        let (start, end, app, title): (i64, i64, String, String) =
            (row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?);
        let ms = (end - start).max(0);
        match runs.last_mut() {
            Some(run) if start - run.end_ts < gap_ms => {
                run.end_ts = run.end_ts.max(end);
                run.ms += ms;
                match run.lines.iter_mut().find(|l| l.0 == app && l.1 == title) {
                    Some(l) => l.2 += ms,
                    None => run.lines.push((app, title, ms)),
                }
            }
            _ => runs.push(UnassignedRun {
                start_ts: start,
                end_ts: end,
                ms,
                lines: vec![(app, title, ms)],
            }),
        }
    }
    for run in &mut runs {
        run.lines
            .sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    }
    Ok(runs)
}

/// Claim the unassigned focus inside `[start_ts, end_ts)` for a task
/// ('assign' correction): one confidence-1.0 interval per batch the spans
/// fall in (intervals are batch-scoped), so the time shows under the task on
/// every surface and the run's app/title mix teaches future derivations.
/// Spans not yet batched (the live tail) get a batch-less interval that
/// attaches when their batch closes. Returns the ms claimed.
pub fn assign_unassigned(
    conn: &mut Connection,
    ts: jiff::Timestamp,
    start_ts: i64,
    end_ts: i64,
    to_task: i64,
) -> Result<i64, StorageError> {
    let tx = conn.transaction()?;
    // batch_id → bounds of its unassigned spans. A span's own batch_id wins;
    // an unbatched span takes the batch its start falls in.
    let mut per_batch: Vec<(Option<i64>, i64, i64)> = Vec::new();
    {
        let mut stmt = tx.prepare(
            "SELECT COALESCE(s.batch_id,
                 (SELECT id FROM batches WHERE start_ts <= s.start_ts AND end_ts > s.start_ts)),
                 s.start_ts, s.end_ts FROM spans s
             WHERE s.kind='focus' AND s.start_ts >= ?1 AND s.start_ts < ?2
               AND NOT EXISTS (SELECT 1 FROM intervals i
                               WHERE i.start_ts < s.end_ts AND i.end_ts > s.start_ts)
             ORDER BY s.start_ts",
        )?;
        let mut rows = stmt.query([start_ts, end_ts])?;
        while let Some(row) = rows.next()? {
            let (batch, s, e): (Option<i64>, i64, i64) = (row.get(0)?, row.get(1)?, row.get(2)?);
            let e = e.min(end_ts);
            match per_batch.iter_mut().find(|p| p.0 == batch) {
                Some(p) => {
                    p.1 = p.1.min(s);
                    p.2 = p.2.max(e);
                }
                None => per_batch.push((batch, s, e)),
            }
        }
    }
    let mut claimed = 0;
    let mut first_interval = None;
    for (batch, s, e) in &per_batch {
        if e <= s {
            continue;
        }
        tx.execute(
            "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence, source)
             VALUES (?1, ?2, ?3, ?4, 1.0, 'user')",
            params![to_task, batch, s, e],
        )?;
        first_interval.get_or_insert(tx.last_insert_rowid());
        claimed += e - s;
    }
    if claimed > 0 {
        let (label, project): (String, Option<String>) = tx.query_row(
            "SELECT label, project FROM tasks WHERE id=?1",
            [to_task],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let ctx = span_ctx(&tx, start_ts, end_ts)?;
        tx.execute(
            "INSERT INTO corrections (ts, task_id, old_label, new_label, old_project, new_project, ctx, kind, interval_id)
             VALUES (?1, ?2, '(unassigned)', ?3, NULL, ?4, ?5, 'assign', ?6)",
            params![ts_to_ms(ts), to_task, label, project, ctx, first_interval],
        )?;
    }
    tx.commit()?;
    Ok(claimed)
}

/// Pull `[start_ts, end_ts)` out of an interval ('eject' correction): the
/// interval shrinks to what lies outside the block (the surviving left piece
/// keeps the id; a right remainder becomes a new row of the same task,
/// batch, confidence and source), the block's app/title mix is stored as a
/// negative for that task, and a derived task left with no time is removed.
/// Returns the ms ejected (0 when the block misses the interval).
pub fn split_interval(
    conn: &mut Connection,
    ts: jiff::Timestamp,
    interval_id: i64,
    start_ts: i64,
    end_ts: i64,
) -> Result<i64, StorageError> {
    let tx = conn.transaction()?;
    let (task_id, batch_id, lo, hi, confidence, source): (i64, Option<i64>, i64, i64, f64, String) =
        tx.query_row(
            "SELECT task_id, batch_id, start_ts, end_ts, confidence, source
             FROM intervals WHERE id=?1",
            [interval_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )?;
    let s = start_ts.max(lo);
    let e = end_ts.min(hi);
    if e <= s {
        return Ok(0);
    }
    let survivor = if s > lo {
        tx.execute(
            "UPDATE intervals SET end_ts=?1 WHERE id=?2",
            params![s, interval_id],
        )?;
        if e < hi {
            tx.execute(
                "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence, source)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![task_id, batch_id, e, hi, confidence, source],
            )?;
        }
        Some(interval_id)
    } else if e < hi {
        tx.execute(
            "UPDATE intervals SET start_ts=?1 WHERE id=?2",
            params![e, interval_id],
        )?;
        Some(interval_id)
    } else {
        tx.execute(
            "UPDATE corrections SET interval_id=NULL WHERE interval_id=?1",
            [interval_id],
        )?;
        tx.execute("DELETE FROM intervals WHERE id=?1", [interval_id])?;
        None
    };
    let (label, project): (String, Option<String>) = tx.query_row(
        "SELECT label, project FROM tasks WHERE id=?1",
        [task_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let ctx = span_ctx(&tx, s, e)?;
    tx.execute(
        "INSERT INTO corrections (ts, task_id, old_label, new_label, old_project, new_project, ctx, kind, interval_id)
         VALUES (?1, ?2, ?3, '(unassigned)', ?4, NULL, ?5, 'eject', ?6)",
        params![ts_to_ms(ts), task_id, label, project, ctx, survivor],
    )?;
    tx.execute(DELETE_ORPHAN_TASKS, [])?;
    tx.commit()?;
    Ok(e - s)
}

/// One pre-pass placement: a run's `[start_ts, end_ts)` under a task, with
/// the one-line rule that matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    pub task_id: i64,
    pub start_ts: i64,
    pub end_ts: i64,
    pub reason: String,
}

/// A stored interval row as the m27 backfill rewrites them. `pinned` rows are
/// referenced by a correction (`corrections.interval_id`) and stay as they
/// are; the rewrite treats their range as a boundary.
#[derive(Debug, Clone)]
pub struct StoredInterval {
    pub task_id: i64,
    pub start_ts: i64,
    pub end_ts: i64,
    pub confidence: f64,
    pub pinned: bool,
}

const PINNED: &str = "id IN (SELECT interval_id FROM corrections WHERE interval_id IS NOT NULL)";

/// `derived` rows of every batch starting at or after `since_ms`, oldest
/// first: `(batch_id, batch start, batch end, rows by start)`.
#[allow(clippy::type_complexity)]
pub fn derived_rows_by_batch(
    conn: &Connection,
    since_ms: i64,
) -> Result<Vec<(i64, i64, i64, Vec<StoredInterval>)>, StorageError> {
    let mut out = Vec::new();
    let mut batches = conn.prepare(
        "SELECT id, start_ts, end_ts FROM batches WHERE start_ts >= ?1 ORDER BY start_ts",
    )?;
    let mut rows = conn.prepare(&format!(
        "SELECT task_id, start_ts, end_ts, confidence, {PINNED} FROM intervals
          WHERE batch_id=?1 AND source='derived' ORDER BY start_ts"
    ))?;
    for b in batches.query_map([since_ms], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))? {
        let (id, lo, hi): (i64, i64, i64) = b?;
        let ivs: Vec<StoredInterval> = rows
            .query_map([id], |r| {
                Ok(StoredInterval {
                    task_id: r.get(0)?,
                    start_ts: r.get(1)?,
                    end_ts: r.get(2)?,
                    confidence: r.get(3)?,
                    pinned: r.get(4)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        if !ivs.is_empty() {
            out.push((id, lo, hi, ivs));
        }
    }
    Ok(out)
}

/// `(start, end)` of the user's own interval rows overlapping `[lo, hi)`.
pub fn user_ranges_in(
    conn: &Connection,
    lo: i64,
    hi: i64,
) -> Result<Vec<(i64, i64)>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT start_ts, end_ts FROM intervals
          WHERE source='user' AND start_ts < ?2 AND end_ts > ?1 ORDER BY start_ts",
    )?;
    let out = stmt
        .query_map([lo, hi], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    Ok(out)
}

/// Replace a batch's unpinned `derived` rows in one transaction (m27
/// backfill); `rows` must carry no pinned entry.
pub fn replace_derived_rows(
    conn: &mut Connection,
    batch_id: i64,
    rows: &[StoredInterval],
) -> Result<(), StorageError> {
    let tx = conn.transaction()?;
    tx.execute(
        &format!("DELETE FROM intervals WHERE batch_id=?1 AND source='derived' AND NOT {PINNED}"),
        [batch_id],
    )?;
    for r in rows {
        tx.execute(
            "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![r.task_id, batch_id, r.start_ts, r.end_ts, r.confidence],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Drop the pre-pass's provisional intervals starting inside `[lo, hi)`
/// (the pre-pass re-reads the window every tick).
pub fn clear_prepass(conn: &Connection, lo: i64, hi: i64) -> Result<usize, StorageError> {
    Ok(conn.execute(
        "DELETE FROM intervals WHERE source='prepass' AND start_ts >= ?1 AND start_ts < ?2",
        [lo, hi],
    )?)
}

/// One provisional interval: confidence 0.5, source 'prepass', attached to
/// the batch its start falls in (NULL over the tail).
pub fn insert_prepass(conn: &Connection, p: &Placement) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence, source, reason)
         VALUES (?1, (SELECT id FROM batches WHERE start_ts <= ?2 AND end_ts > ?2),
                 ?2, ?3, 0.5, 'prepass', ?4)",
        params![p.task_id, p.start_ts, p.end_ts, p.reason],
    )?;
    Ok(())
}

/// One live-tier interval (m27 chunk 4): `source='live'`, over the tail
/// (`batch_id` NULL), replacing pre-pass rows in its window. The batch
/// derive later replaces it like a pre-pass row; `keep` promotes it the same
/// way. A `New` slot creates a derived task (orphan-pruned if the batch
/// disagrees).
pub fn insert_live_interval(
    conn: &mut Connection,
    slot: &TaskSlot,
    lo: i64,
    hi: i64,
    confidence: f64,
) -> Result<i64, StorageError> {
    let tx = conn.transaction()?;
    let task_id = match slot {
        TaskSlot::Existing(id) => *id,
        TaskSlot::New { label, project } => {
            tx.execute(
                "INSERT INTO tasks (label, project, status, source, created_ts)
                 VALUES (?1, ?2, 'open', 'derived', ?3)",
                params![label, project, lo],
            )?;
            tx.last_insert_rowid()
        }
    };
    clear_prepass(&tx, lo, hi)?;
    tx.execute(
        "DELETE FROM intervals WHERE source='live' AND start_ts >= ?1 AND start_ts < ?2",
        [lo, hi],
    )?;
    tx.execute(
        "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence, source, reason)
         VALUES (?1, NULL, ?2, ?3, ?4, 'live', 'live')",
        params![task_id, lo, hi, confidence],
    )?;
    tx.execute(DELETE_ORPHAN_TASKS, [])?;
    tx.commit()?;
    Ok(task_id)
}

/// Where the current stretch starts for the live tier: the latest interval
/// end (any source) or the end of the latest AFK gap ≥ 5 min inside
/// `[floor, now)`, else `floor`.
pub fn live_window_start(conn: &Connection, floor: i64, now: i64) -> Result<i64, StorageError> {
    let iv: Option<i64> = conn.query_row(
        "SELECT MAX(MIN(end_ts, ?2)) FROM intervals WHERE end_ts > ?1 AND start_ts < ?2",
        [floor, now],
        |r| r.get(0),
    )?;
    let afk: Option<i64> = conn.query_row(
        "SELECT MAX(MIN(end_ts, ?2)) FROM spans
         WHERE kind='afk' AND end_ts - start_ts >= 300000 AND end_ts > ?1 AND start_ts < ?2",
        [floor, now],
        |r| r.get(0),
    )?;
    Ok(floor.max(iv.unwrap_or(0)).max(afk.unwrap_or(0)))
}

/// Focus ms inside `[lo, hi)`.
pub fn focus_ms_in(conn: &Connection, lo: i64, hi: i64) -> Result<i64, StorageError> {
    Ok(conn.query_row(
        "SELECT COALESCE(SUM(MIN(end_ts, ?2) - MAX(start_ts, ?1)), 0) FROM spans
         WHERE kind='focus' AND start_ts < ?2 AND end_ts > ?1",
        [lo, hi],
        |r| r.get(0),
    )?)
}

/// The label of the interval ending last before `ts` (the live prompt's
/// "Previously" line).
pub fn label_before(conn: &Connection, ts: i64) -> Result<Option<String>, StorageError> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(
            "SELECT t.label FROM intervals i JOIN tasks t ON t.id = i.task_id
             WHERE i.end_ts <= ?1 ORDER BY i.end_ts DESC LIMIT 1",
            [ts],
            |r| r.get(0),
        )
        .optional()?)
}

/// What the resident worker is deriving right now, for the feed's
/// "deriving…" row (meta `derive_progress`; absent when idle).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeriveProgress {
    /// `batch` | `live`.
    pub kind: String,
    pub batch_id: Option<i64>,
    pub start_ts: i64,
    pub end_ts: i64,
    pub started_ts: i64,
    /// The label being typed, or "linking to <task>"; empty until the model
    /// says something.
    pub label: String,
}

pub const DERIVE_PROGRESS_KEY: &str = "derive_progress";

pub fn set_derive_progress(
    conn: &Connection,
    progress: Option<&DeriveProgress>,
) -> Result<(), StorageError> {
    let json = progress.map(|p| serde_json::to_string(p).unwrap_or_default());
    set_meta(conn, DERIVE_PROGRESS_KEY, json.as_deref())
}

pub fn derive_progress(conn: &Connection) -> Result<Option<DeriveProgress>, StorageError> {
    Ok(get_meta(conn, DERIVE_PROGRESS_KEY)?.and_then(|v| serde_json::from_str(&v).ok()))
}

/// The day's tasks for consolidation (m27 chunk 6): every open task with an
/// interval overlapping `[lo, hi)`, with totals, batch spread, notes and
/// rename flags, and the three strongest app/title lines under it.
pub fn day_tasks(
    conn: &Connection,
    lo: i64,
    hi: i64,
) -> Result<Vec<crate::consolidate::DayTask>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT t.id, t.label, t.project, t.external_ref, t.source,
                SUM(MIN(i.end_ts, ?2) - MAX(i.start_ts, ?1)), COUNT(i.id),
                COUNT(DISTINCT COALESCE(i.batch_id, -i.id)),
                EXISTS(SELECT 1 FROM journal_entries j WHERE j.task_id = t.id)
                  OR EXISTS(SELECT 1 FROM checkpoints c WHERE c.task_id = t.id),
                EXISTS(SELECT 1 FROM corrections c WHERE c.task_id = t.id AND c.kind = 'rename')
         FROM tasks t JOIN intervals i ON i.task_id = t.id
         WHERE t.status = 'open' AND i.start_ts < ?2 AND i.end_ts > ?1
         GROUP BY t.id ORDER BY 6 DESC, t.id",
    )?;
    let mut evidence = conn.prepare(
        "SELECT s.app, s.title, SUM(MIN(s.end_ts, i.end_ts) - MAX(s.start_ts, i.start_ts)) AS ms
         FROM intervals i JOIN spans s ON s.kind = 'focus' AND s.start_ts < i.end_ts AND s.end_ts > i.start_ts
         WHERE i.task_id = ?1 AND i.start_ts < ?3 AND i.end_ts > ?2
         GROUP BY s.app, s.title ORDER BY ms DESC LIMIT 3",
    )?;
    let mut out = Vec::new();
    let mut rows = stmt.query([lo, hi])?;
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let source: String = r.get(4)?;
        let ev = evidence
            .query_map(params![id, lo, hi], |e| {
                Ok((e.get(0)?, e.get(1)?, e.get(2)?))
            })?
            .collect::<Result<Vec<(String, String, i64)>, _>>()?;
        out.push(crate::consolidate::DayTask {
            id,
            label: r.get(1)?,
            project: r.get(2)?,
            external_ref: r.get(3)?,
            locked: source == "user",
            total_ms: r.get(5)?,
            intervals: r.get::<_, i64>(6)? as usize,
            batches: r.get::<_, i64>(7)? as usize,
            has_notes: r.get(8)?,
            user_renamed: r.get(9)?,
            evidence: ev,
        });
    }
    Ok(out)
}

/// `(task_id, start_ts, end_ts)` for every interval overlapping `[lo, hi)`,
/// in time order — the orphan fold's neighbour test.
pub fn day_intervals(
    conn: &Connection,
    lo: i64,
    hi: i64,
) -> Result<Vec<(i64, i64, i64)>, StorageError> {
    Ok(conn
        .prepare(
            "SELECT task_id, start_ts, end_ts FROM intervals
             WHERE start_ts < ?2 AND end_ts > ?1 ORDER BY start_ts, id",
        )?
        .query_map([lo, hi], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?)
}

/// Apply a consolidation plan in one transaction and record it as a single
/// `consolidate` correction whose `ctx` is the before-state (JSON) for
/// undo. Folded tasks lose their intervals and close (an orphan derived row
/// nothing references is removed); no `merge`/`rename` correction rows are
/// written — those mean the user spoke. Returns the correction id.
pub fn consolidate_apply(
    conn: &mut Connection,
    ts: jiff::Timestamp,
    day: &str,
    plan: &crate::consolidate::Plan,
) -> Result<i64, StorageError> {
    use crate::consolidate::{Before, MergeBefore, RenameBefore};
    let tx = conn.transaction()?;
    let mut before = Before {
        day: day.to_owned(),
        ..Before::default()
    };
    for &(from, into) in &plan.merges {
        let (label, project, source, created_ts, external_ref): (
            String,
            Option<String>,
            String,
            i64,
            Option<String>,
        ) = tx.query_row(
            "SELECT label, project, source, created_ts, external_ref FROM tasks WHERE id=?1",
            [from],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )?;
        let interval_ids: Vec<i64> = tx
            .prepare("SELECT id FROM intervals WHERE task_id=?1")?
            .query_map([from], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        tx.execute(
            "UPDATE intervals SET task_id=?1 WHERE task_id=?2",
            params![into, from],
        )?;
        tx.execute(
            "UPDATE tasks SET status='closed', closed_ts=?1 WHERE id=?2",
            params![ts_to_ms(ts), from],
        )?;
        before.merges.push(MergeBefore {
            from,
            into,
            label,
            project,
            source,
            created_ts,
            external_ref,
            interval_ids,
        });
    }
    for (id, new_label) in &plan.renames {
        let old_label: String =
            tx.query_row("SELECT label FROM tasks WHERE id=?1", [id], |r| r.get(0))?;
        tx.execute(
            "UPDATE tasks SET label=?1 WHERE id=?2",
            params![new_label, id],
        )?;
        before.renames.push(RenameBefore {
            id: *id,
            old_label,
            new_label: new_label.clone(),
        });
    }
    // The row hangs off a task the run kept (a merge target, else a renamed
    // task) so the foreign key holds.
    let anchor = plan
        .merges
        .iter()
        .map(|m| m.1)
        .find(|into| !plan.merges.iter().any(|m| m.0 == *into))
        .or_else(|| plan.renames.first().map(|r| r.0))
        .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
    let ctx = serde_json::to_string(&before).unwrap_or_default();
    tx.execute(
        "INSERT INTO corrections (ts, task_id, old_label, new_label, old_project, new_project, ctx, kind)
         VALUES (?1, ?2, ?3, ?4, NULL, NULL, ?5, 'consolidate')",
        params![
            ts_to_ms(ts),
            anchor,
            format!("{} merges", plan.merges.len()),
            format!("{} renames", plan.renames.len()),
            ctx
        ],
    )?;
    let id = tx.last_insert_rowid();
    tx.execute(DELETE_ORPHAN_TASKS, [])?;
    set_meta(&tx, &format!("consolidated:{day}"), Some(&id.to_string()))?;
    tx.commit()?;
    Ok(id)
}

/// Reverse one consolidation run: renamed tasks get their labels back,
/// folded tasks are reopened (or recreated under their old id) and their
/// intervals moved home, the correction row and the day's stamp go.
pub fn consolidate_undo(conn: &mut Connection, correction_id: i64) -> Result<(), StorageError> {
    use crate::consolidate::Before;
    use rusqlite::OptionalExtension;
    let tx = conn.transaction()?;
    let Some(ctx) = tx
        .query_row(
            "SELECT ctx FROM corrections WHERE id=?1 AND kind='consolidate'",
            [correction_id],
            |r| r.get::<_, String>(0),
        )
        .optional()?
    else {
        return Ok(());
    };
    let before: Before = serde_json::from_str(&ctx).unwrap_or_default();
    for r in &before.renames {
        tx.execute(
            "UPDATE tasks SET label=?1 WHERE id=?2 AND label=?3",
            params![r.old_label, r.id, r.new_label],
        )?;
    }
    for m in before.merges.iter().rev() {
        // `tasks.id` has no AUTOINCREMENT: a freed id can have been reused by
        // an unrelated task since. Only a row matching the snapshot is ours;
        // otherwise the task comes back under a fresh id.
        let same: Option<i64> = tx
            .query_row(
                "SELECT id FROM tasks WHERE id=?1 AND label=?2 AND created_ts=?3 AND source=?4",
                params![m.from, m.label, m.created_ts, m.source],
                |r| r.get(0),
            )
            .optional()?;
        let home = match same {
            Some(id) => {
                tx.execute(
                    "UPDATE tasks SET status='open', closed_ts=NULL WHERE id=?1",
                    [id],
                )?;
                id
            }
            None => {
                let taken: bool =
                    tx.query_row("SELECT COUNT(*) FROM tasks WHERE id=?1", [m.from], |r| {
                        Ok(r.get::<_, i64>(0)? > 0)
                    })?;
                if taken {
                    tx.execute(
                        "INSERT INTO tasks (label, project, status, source, created_ts, external_ref)
                         VALUES (?1, ?2, 'open', ?3, ?4, ?5)",
                        params![m.label, m.project, m.source, m.created_ts, m.external_ref],
                    )?;
                } else {
                    tx.execute(
                        "INSERT INTO tasks (id, label, project, status, source, created_ts, external_ref)
                         VALUES (?1, ?2, ?3, 'open', ?4, ?5, ?6)",
                        params![m.from, m.label, m.project, m.source, m.created_ts, m.external_ref],
                    )?;
                }
                tx.last_insert_rowid()
            }
        };
        for iv in &m.interval_ids {
            tx.execute(
                "UPDATE intervals SET task_id=?1 WHERE id=?2 AND task_id=?3",
                params![home, iv, m.into],
            )?;
        }
    }
    tx.execute("DELETE FROM corrections WHERE id=?1", [correction_id])?;
    tx.execute(DELETE_ORPHAN_TASKS, [])?;
    set_meta(&tx, &format!("consolidated:{}", before.day), None)?;
    tx.commit()?;
    Ok(())
}

/// The day's consolidation stamp: None = not run, Some(0) = ran and changed
/// nothing, Some(id) = the correction to undo.
pub fn consolidation_of_day(conn: &Connection, day: &str) -> Result<Option<i64>, StorageError> {
    Ok(get_meta(conn, &format!("consolidated:{day}"))?.and_then(|v| v.parse().ok()))
}

pub fn stamp_consolidated(conn: &Connection, day: &str, id: i64) -> Result<(), StorageError> {
    set_meta(conn, &format!("consolidated:{day}"), Some(&id.to_string()))
}

/// The newest live-tier row: its end and the task label (the inspector's
/// "last live pass").
pub fn last_live_interval(conn: &Connection) -> Result<Option<(i64, String)>, StorageError> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(
            "SELECT i.end_ts, t.label FROM intervals i JOIN tasks t ON t.id = i.task_id
             WHERE i.source='live' ORDER BY i.end_ts DESC LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

/// Pre-pass placements in `[lo, hi)` with their task labels:
/// `(start_ts, end_ts, label, reason)` in time order.
pub fn tail_placements(
    conn: &Connection,
    lo: i64,
    hi: i64,
) -> Result<Vec<(i64, i64, String, String)>, StorageError> {
    Ok(conn
        .prepare(
            "SELECT i.start_ts, i.end_ts, t.label, COALESCE(i.reason, '')
             FROM intervals i JOIN tasks t ON t.id = i.task_id
             WHERE i.source='prepass' AND i.start_ts >= ?1 AND i.start_ts < ?2
             ORDER BY i.start_ts",
        )?
        .query_map([lo, hi], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<Result<_, _>>()?)
}

/// The open task anchored to `key` (`tasks.external_ref`), if any.
pub fn open_task_by_ref(conn: &Connection, key: &str) -> Result<Option<OpenTask>, StorageError> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(
            "SELECT id, label, project, source='user' FROM tasks
             WHERE status='open' AND external_ref=?1 ORDER BY id LIMIT 1",
            [key],
            |r| {
                Ok(OpenTask {
                    id: r.get(0)?,
                    label: r.get(1)?,
                    project: r.get(2)?,
                    declared: r.get(3)?,
                })
            },
        )
        .optional()?)
}

/// A task's anchor (`tasks.external_ref`), if set.
pub fn task_external_ref(conn: &Connection, task_id: i64) -> Result<Option<String>, StorageError> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(
            "SELECT external_ref FROM tasks WHERE id=?1",
            [task_id],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// Distinct repos with activity overlapping `[lo, hi)` — a run's repo signal.
pub fn repos_active_in(conn: &Connection, lo: i64, hi: i64) -> Result<Vec<String>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT repo FROM activity_events
         WHERE repo != '' AND ts < ?2 AND COALESCE(end_ts, ts) >= ?1 ORDER BY repo",
    )?;
    let rows = stmt.query_map([lo, hi], |r| r.get(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// End of the task's latest interval (its last activity), if any.
pub fn task_created_ts(conn: &Connection, task_id: i64) -> Result<i64, StorageError> {
    Ok(
        conn.query_row("SELECT created_ts FROM tasks WHERE id=?1", [task_id], |r| {
            r.get(0)
        })?,
    )
}

pub fn task_last_end(conn: &Connection, task_id: i64) -> Result<Option<i64>, StorageError> {
    Ok(conn.query_row(
        "SELECT MAX(end_ts) FROM intervals WHERE task_id=?1",
        [task_id],
        |r| r.get(0),
    )?)
}

/// The pre-pass's provisional intervals overlapping `[lo, hi)` (a batch's
/// window), oldest first: the derive prompt's hints. Read before
/// `store_derivation`, which drops them.
pub fn prepass_hints(conn: &Connection, lo: i64, hi: i64) -> Result<Vec<Placement>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT task_id, start_ts, end_ts, COALESCE(reason, '') FROM intervals
         WHERE source='prepass' AND start_ts < ?2 AND end_ts > ?1 ORDER BY start_ts, id",
    )?;
    let rows = stmt.query_map([lo, hi], |r| {
        Ok(Placement {
            task_id: r.get(0)?,
            start_ts: r.get(1)?,
            end_ts: r.get(2)?,
            reason: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// The open task with this id, if still open.
pub fn open_task_by_id(conn: &Connection, id: i64) -> Result<Option<OpenTask>, StorageError> {
    use rusqlite::OptionalExtension;
    Ok(conn
        .query_row(
            "SELECT id, label, project, source='user' FROM tasks WHERE status='open' AND id=?1",
            [id],
            |r| {
                Ok(OpenTask {
                    id: r.get(0)?,
                    label: r.get(1)?,
                    project: r.get(2)?,
                    declared: r.get(3)?,
                })
            },
        )
        .optional()?)
}

/// End of the newest derived batch: where the pre-pass window opens.
pub fn latest_done_batch_end(conn: &Connection) -> Result<Option<i64>, StorageError> {
    Ok(conn.query_row(
        "SELECT MAX(end_ts) FROM batches WHERE status='done'",
        [],
        |r| r.get(0),
    )?)
}

/// App/title lines kept per feed block.
pub const FEED_LINES: usize = 3;

/// Who placed a feed block: the interval covering it plus its task.
#[derive(Debug, Clone, PartialEq)]
pub struct FeedClaim {
    pub interval_id: i64,
    pub task_id: i64,
    pub label: String,
    pub project: Option<String>,
    /// `derived` | `user` | `prepass`.
    pub source: String,
    pub confidence: f64,
    /// The pre-pass rule that matched (`prepass` rows; kept on a `keep`).
    pub reason: Option<String>,
}

/// One block of the Home feed (m24): an interval of any source, or an
/// unassigned run no interval covers.
#[derive(Debug, Clone, PartialEq)]
pub struct FeedBlock {
    pub start_ts: i64,
    pub end_ts: i64,
    /// Focus ms inside the block.
    pub ms: i64,
    /// `(app, title, ms)`, largest first, at most [`FEED_LINES`].
    pub lines: Vec<(String, String, i64)>,
    /// None = unassigned run.
    pub claim: Option<FeedClaim>,
    /// Run only: a derived batch already covers its start (derive ran and
    /// left it) rather than still waiting for one.
    pub derived: bool,
}

/// The feed over `[lo, hi)`: every interval starting inside it (any source)
/// plus the unassigned runs (gap < `gap_ms` folds), newest first, at most
/// `cap` blocks. An interval's lines are the focus spans it overlaps.
pub fn feed_blocks(
    conn: &Connection,
    lo: i64,
    hi: i64,
    gap_ms: i64,
    cap: usize,
) -> Result<Vec<FeedBlock>, StorageError> {
    let mut blocks = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT i.id, i.task_id, t.label, t.project, i.start_ts, MIN(i.end_ts, ?2),
                i.confidence, i.source, i.reason
         FROM intervals i JOIN tasks t ON t.id = i.task_id
         WHERE i.start_ts >= ?1 AND i.start_ts < ?2
         ORDER BY i.start_ts DESC, i.id DESC LIMIT ?3",
    )?;
    let mut lines_stmt = conn.prepare(
        "SELECT app, title, SUM(MIN(end_ts, ?2) - MAX(start_ts, ?1)) AS ms FROM spans
         WHERE kind='focus' AND start_ts < ?2 AND end_ts > ?1
         GROUP BY app, title ORDER BY ms DESC",
    )?;
    let mut rows = stmt.query(params![lo, hi, cap as i64])?;
    while let Some(row) = rows.next()? {
        let (start_ts, end_ts): (i64, i64) = (row.get(4)?, row.get(5)?);
        let claim = FeedClaim {
            interval_id: row.get(0)?,
            task_id: row.get(1)?,
            label: row.get(2)?,
            project: row.get(3)?,
            source: row.get(7)?,
            confidence: row.get(6)?,
            reason: row.get(8)?,
        };
        let mut ms = 0;
        let mut lines = Vec::new();
        let mut spans = lines_stmt.query([start_ts, end_ts])?;
        while let Some(span) = spans.next()? {
            let line: (String, String, i64) = (span.get(0)?, span.get(1)?, span.get(2)?);
            ms += line.2;
            if lines.len() < FEED_LINES {
                lines.push(line);
            }
        }
        blocks.push(FeedBlock {
            start_ts,
            end_ts,
            ms,
            lines,
            claim: Some(claim),
            derived: false,
        });
    }
    let derived_to = latest_done_batch_end(conn)?.unwrap_or(i64::MIN);
    for run in unassigned_runs(conn, lo, hi, gap_ms)? {
        let mut lines = run.lines;
        lines.truncate(FEED_LINES);
        blocks.push(FeedBlock {
            start_ts: run.start_ts,
            end_ts: run.end_ts,
            ms: run.ms,
            lines,
            claim: None,
            derived: run.start_ts < derived_to,
        });
    }
    blocks.sort_by_key(|b| std::cmp::Reverse(b.start_ts));
    blocks.truncate(cap);
    Ok(blocks)
}

/// Confirm a provisional interval ('assign' correction): it becomes a
/// confidence-1.0 user row (derive never replaces those) and its app/title
/// mix teaches future placements like a hand assign does.
pub fn keep_interval(
    conn: &mut Connection,
    ts: jiff::Timestamp,
    interval_id: i64,
) -> Result<(), StorageError> {
    let tx = conn.transaction()?;
    let (task_id, start_ts, end_ts): (i64, i64, i64) = tx.query_row(
        "SELECT task_id, start_ts, end_ts FROM intervals WHERE id=?1",
        [interval_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    tx.execute(
        "UPDATE intervals SET source='user', confidence=1.0 WHERE id=?1",
        [interval_id],
    )?;
    let (label, project): (String, Option<String>) = tx.query_row(
        "SELECT label, project FROM tasks WHERE id=?1",
        [task_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let ctx = span_ctx(&tx, start_ts, end_ts)?;
    tx.execute(
        "INSERT INTO corrections (ts, task_id, old_label, new_label, old_project, new_project, ctx, kind, interval_id)
         VALUES (?1, ?2, '(unassigned)', ?3, NULL, ?4, ?5, 'assign', ?6)",
        params![ts_to_ms(ts), task_id, label, project, ctx, interval_id],
    )?;
    tx.commit()?;
    Ok(())
}

/// Fold one task into another ('merge' correction): every interval moves to
/// the target, the source closes (a derived source nothing else references is
/// removed). The source's span context plus its label → target label pair
/// become task-grain teaching data for future linking.
pub fn merge_task(
    conn: &mut Connection,
    ts: jiff::Timestamp,
    from_task: i64,
    to_task: i64,
) -> Result<(), StorageError> {
    if from_task == to_task {
        return Ok(());
    }
    let tx = conn.transaction()?;
    let ident = |id: i64| -> Result<(String, Option<String>), rusqlite::Error> {
        tx.query_row("SELECT label, project FROM tasks WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
    };
    let (old_label, old_project) = ident(from_task)?;
    let (new_label, new_project) = ident(to_task)?;
    // Snapshot before the intervals move — afterwards the source has none.
    let ctx = task_span_ctx(&tx, from_task)?;
    tx.execute(
        "INSERT INTO corrections (ts, task_id, old_label, new_label, old_project, new_project, ctx, kind)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'merge')",
        params![
            ts_to_ms(ts),
            to_task,
            old_label,
            new_label,
            old_project,
            new_project,
            ctx
        ],
    )?;
    tx.execute(
        "UPDATE intervals SET task_id=?1 WHERE task_id=?2",
        params![to_task, from_task],
    )?;
    tx.execute(
        "UPDATE tasks SET status='closed', closed_ts=?1 WHERE id=?2",
        params![ts_to_ms(ts), from_task],
    )?;
    tx.execute(DELETE_ORPHAN_TASKS, [])?;
    tx.commit()?;
    Ok(())
}

/// Past corrections whose stored span context matches the given batch spans
/// (FTS over titles + apps, bm25-ranked, deduped by resulting label): up to
/// `k` positives (rename / assign / …) followed by up to `k` ejects, each on
/// its own budget so a run of ejects cannot crowd the few-shot renames out.
pub fn similar_corrections(
    conn: &Connection,
    spans: &[SpanDraft],
    k: usize,
) -> Result<Vec<Correction>, StorageError> {
    let mut ranked = corrections_matching(conn, &fts_or_query(spans), k * 2)?;
    ranked.retain(|c| c.kind != "consolidate");
    let mut out: Vec<Correction> = ranked
        .iter()
        .filter(|c| c.kind != "eject")
        .take(k)
        .cloned()
        .collect();
    // A rename onto a declared task's label outranks free-text renames of
    // equal match: the user named that task on purpose.
    let declared: Vec<String> = conn
        .prepare("SELECT LOWER(label) FROM tasks WHERE status='open' AND source='user'")?
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<Result<_, _>>()?;
    out.sort_by_key(|c| !declared.contains(&c.new_label.to_lowercase()));
    out.extend(ranked.into_iter().filter(|c| c.kind == "eject").take(k));
    Ok(out)
}

/// Best past correction for free text (an unassigned run's app/title mix):
/// the label the user last gave similar work, if any. Terms found in more
/// than half of all corrections (a shell prompt's user@host, a terminal's
/// app name) carry no signal and are dropped first, so a run only gets a
/// suggestion when something distinctive about it matched. Small corpora
/// keep every term: with a handful of rows, frequency says nothing yet.
/// A task the user ejected similar work from (an 'eject' correction in the
/// same match set) is never suggested, whatever its rank.
pub fn suggest_correction(
    conn: &Connection,
    text: &str,
) -> Result<Option<Correction>, StorageError> {
    let ranked = correction_hints(conn, text)?;
    let ejected: Vec<(&str, Option<&str>)> = ranked
        .iter()
        .filter(|c| c.kind == "eject")
        .map(|c| (c.old_label.as_str(), c.old_project.as_deref()))
        .collect();
    Ok(ranked
        .iter()
        .find(|c| {
            c.kind != "eject"
                && !ejected.contains(&(c.new_label.as_str(), c.new_project.as_deref()))
        })
        .cloned())
}

/// Every past correction whose context matches the text's distinctive
/// terms, best first, ejects included — the pre-pass reads the ejects as
/// "never this task for work like this".
pub fn correction_hints(conn: &Connection, text: &str) -> Result<Vec<Correction>, StorageError> {
    let mut ranked = corrections_matching(conn, &distinctive_fts_query(conn, text)?, 16)?;
    ranked.retain(|c| c.kind != "consolidate");
    Ok(ranked)
}

/// OR-of-terms FTS5 query from free text with the corpus-common terms
/// pruned (see `suggest_correction`).
fn distinctive_fts_query(conn: &Connection, text: &str) -> Result<String, StorageError> {
    let mut terms = Vec::new();
    push_fts_terms(text, &mut terms);
    terms.truncate(32);
    let total: i64 = conn.query_row("SELECT COUNT(*) FROM corrections", [], |r| r.get(0))?;
    if total >= IDF_MIN_ROWS {
        let mut df =
            conn.prepare("SELECT COUNT(*) FROM corrections_fts WHERE corrections_fts MATCH ?1")?;
        let mut kept = Vec::new();
        for t in terms {
            let n: i64 = df.query_row([format!("\"{t}\"")], |r| r.get(0))?;
            if n * 2 <= total {
                kept.push(t);
            }
        }
        terms = kept;
    }
    Ok(join_fts_terms(terms))
}

/// Corrections needed before term frequency prunes the suggestion query.
const IDF_MIN_ROWS: i64 = 4;

/// Top-k corrections for an FTS query, bm25-ranked, deduped by kind +
/// resulting label + project.
fn corrections_matching(
    conn: &Connection,
    query: &str,
    k: usize,
) -> Result<Vec<Correction>, StorageError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT c.old_label, c.new_label, c.old_project, c.new_project, c.kind, c.ctx
         FROM corrections_fts f JOIN corrections c ON c.id = f.rowid
         WHERE corrections_fts MATCH ?1 ORDER BY f.rank LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![query, (k * 4) as i64], |r| {
        Ok(Correction {
            old_label: r.get(0)?,
            new_label: r.get(1)?,
            old_project: r.get(2)?,
            new_project: r.get(3)?,
            kind: r.get(4)?,
            ctx: r.get(5)?,
        })
    })?;
    let mut out: Vec<Correction> = Vec::new();
    for c in rows {
        let c = c?;
        if out.iter().any(|o| {
            o.kind == c.kind && o.new_label == c.new_label && o.new_project == c.new_project
        }) {
            continue;
        }
        out.push(c);
        if out.len() >= k {
            break;
        }
    }
    Ok(out)
}

/// OR-of-terms FTS5 query from span titles + apps: alphanumeric words only
/// (safe to embed quoted), deduped, count-capped to keep the query bounded.
fn fts_or_query(spans: &[SpanDraft]) -> String {
    let mut terms: Vec<String> = Vec::new();
    for span in spans {
        if span.kind != SpanKind::Focus {
            continue;
        }
        for source in [span.app.as_str(), span.title.as_str()] {
            push_fts_terms(source, &mut terms);
        }
        if terms.len() >= 32 {
            break;
        }
    }
    join_fts_terms(terms)
}

/// OR-of-terms FTS5 query from free chat text (same term rules as above).
pub fn fts_query_from_text(text: &str) -> String {
    let mut terms = Vec::new();
    push_fts_terms(text, &mut terms);
    join_fts_terms(terms)
}

fn push_fts_terms(source: &str, terms: &mut Vec<String>) {
    for word in source.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if word.chars().count() < 2 {
            continue;
        }
        let word = word.to_lowercase();
        if !terms.contains(&word) {
            terms.push(word);
        }
    }
}

fn join_fts_terms(mut terms: Vec<String>) -> String {
    terms.truncate(32);
    terms
        .iter()
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}

fn task_from_row(r: &rusqlite::Row) -> rusqlite::Result<Task> {
    Ok(Task {
        id: r.get(0)?,
        interval_id: r.get(1)?,
        label: r.get(2)?,
        project: r.get(3)?,
        start_ts: ms_to_ts(r.get(4)?),
        end_ts: ms_to_ts(r.get(5)?),
        confidence: r.get(6)?,
        declared: r.get(7)?,
        description: r.get(8)?,
        external_ref: r.get(9)?,
    })
}

const TASK_COLS: &str = "t.id, i.id, t.label, t.project, i.start_ts, i.end_ts, i.confidence, \
     t.source='user', t.description, t.external_ref";

pub fn tasks_in_range(conn: &Connection, lo: i64, hi: i64) -> Result<Vec<Task>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {TASK_COLS} FROM intervals i JOIN tasks t ON t.id = i.task_id
         WHERE i.end_ts > ?1 AND i.start_ts < ?2 ORDER BY i.start_ts, i.id"
    ))?;
    let rows = stmt.query_map([lo, hi], task_from_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// One (task, app, title) focus-overlap aggregate from [`evidence_in_range`].
pub struct EvidenceRow {
    pub task_id: i64,
    pub app: String,
    pub title: String,
    pub ms: i64,
}

/// Focus spans overlap-joined to task intervals in `[lo, hi)`, grouped by
/// (task, app, title) and summed by overlap time clamped to the range.
/// Ordered per task by overlap descending.
pub fn evidence_in_range(
    conn: &Connection,
    lo: i64,
    hi: i64,
) -> Result<Vec<EvidenceRow>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT i.task_id, s.app, s.title,
                SUM(MIN(s.end_ts, i.end_ts, ?2) - MAX(s.start_ts, i.start_ts, ?1)) AS ms
         FROM spans s JOIN intervals i
           ON s.end_ts > i.start_ts AND s.start_ts < i.end_ts
         WHERE s.kind = 'focus'
           AND s.end_ts > ?1 AND s.start_ts < ?2
           AND i.end_ts > ?1 AND i.start_ts < ?2
         GROUP BY i.task_id, s.app, s.title
         ORDER BY i.task_id, ms DESC",
    )?;
    let rows = stmt.query_map([lo, hi], |r| {
        Ok(EvidenceRow {
            task_id: r.get(0)?,
            app: r.get(1)?,
            title: r.get(2)?,
            ms: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn spans_in_range(conn: &Connection, lo: i64, hi: i64) -> Result<Vec<SpanDraft>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT start_ts, end_ts, app, title, kind, url FROM spans
         WHERE end_ts > ?1 AND start_ts < ?2 ORDER BY start_ts, id",
    )?;
    let rows = stmt.query_map([lo, hi], |r| {
        Ok(SpanDraft {
            start: ms_to_ts(r.get(0)?),
            end: ms_to_ts(r.get(1)?),
            app: r.get(2)?,
            title: r.get(3)?,
            kind: SpanKind::parse(&r.get::<_, String>(4)?).unwrap_or(SpanKind::Focus),
            url: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Intervals of the top-k tasks whose label matches the FTS query,
/// bm25-ranked.
pub fn search_tasks(conn: &Connection, query: &str, k: usize) -> Result<Vec<Task>, StorageError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT {TASK_COLS} FROM intervals i JOIN tasks t ON t.id = i.task_id
         WHERE t.id IN (SELECT rowid FROM tasks_fts WHERE tasks_fts MATCH ?1
                        ORDER BY rank LIMIT ?2)
         ORDER BY i.start_ts, i.id"
    ))?;
    let rows = stmt.query_map(params![query, k as i64], task_from_row)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Top-k focus spans whose title matches the FTS query, bm25-ranked.
pub fn search_spans(
    conn: &Connection,
    query: &str,
    k: usize,
) -> Result<Vec<SpanDraft>, StorageError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT start_ts, end_ts, app, title, kind, url FROM spans
         WHERE id IN (SELECT rowid FROM spans_fts WHERE spans_fts MATCH ?1
                      ORDER BY rank LIMIT ?2)
         ORDER BY start_ts, id",
    )?;
    let rows = stmt.query_map(params![query, k as i64], |r| {
        Ok(SpanDraft {
            start: ms_to_ts(r.get(0)?),
            end: ms_to_ts(r.get(1)?),
            app: r.get(2)?,
            title: r.get(3)?,
            kind: SpanKind::parse(&r.get::<_, String>(4)?).unwrap_or(SpanKind::Focus),
            url: r.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn insert_chat_message(
    conn: &Connection,
    ts: jiff::Timestamp,
    conversation_id: i64,
    role: &str,
    content: &str,
) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO chat_messages (ts, conversation_id, role, content) VALUES (?1, ?2, ?3, ?4)",
        params![ts_to_ms(ts), conversation_id, role, content],
    )?;
    Ok(())
}

/// Last `n` messages of one conversation, oldest first.
pub fn recent_chat_messages(
    conn: &Connection,
    conversation_id: i64,
    n: usize,
) -> Result<Vec<(String, String)>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT role, content FROM (
             SELECT id, role, content FROM chat_messages
             WHERE conversation_id = ?1 ORDER BY id DESC LIMIT ?2
         ) ORDER BY id",
    )?;
    let rows = stmt.query_map(params![conversation_id, n as i64], |r| {
        Ok((r.get(0)?, r.get(1)?))
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn create_conversation(conn: &Connection, ts: jiff::Timestamp) -> Result<i64, StorageError> {
    conn.execute(
        "INSERT INTO conversations (created_ts) VALUES (?1)",
        params![ts_to_ms(ts)],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Conversations by recency of last activity: (id, last_ts, first user
/// question — empty when the conversation has none yet).
pub fn list_conversations(
    conn: &Connection,
    limit: usize,
) -> Result<Vec<(i64, i64, String)>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT c.id,
                COALESCE((SELECT MAX(ts) FROM chat_messages m
                          WHERE m.conversation_id = c.id), c.created_ts) AS last_ts,
                COALESCE((SELECT content FROM chat_messages m
                          WHERE m.conversation_id = c.id AND m.role = 'user'
                          ORDER BY m.id LIMIT 1), '') AS snippet
         FROM conversations c
         ORDER BY last_ts DESC, c.id DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Delete a conversation and every message in it (history menu "×").
pub fn delete_conversation(conn: &mut Connection, id: i64) -> Result<(), StorageError> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM chat_messages WHERE conversation_id=?1", [id])?;
    tx.execute("DELETE FROM conversations WHERE id=?1", [id])?;
    tx.commit()?;
    Ok(())
}

/// Drop conversations that never received a message (older builds created
/// one on every chat-view open). Returns how many went.
pub fn delete_empty_conversations(conn: &Connection) -> Result<usize, StorageError> {
    Ok(conn.execute(
        "DELETE FROM conversations WHERE id NOT IN
         (SELECT conversation_id FROM chat_messages WHERE conversation_id IS NOT NULL)",
        [],
    )?)
}

/// Open tasks nothing has moved for `days`: no interval, and no checkpoint
/// written since the cutoff either — the one checkpoint row per task holds
/// the newest "what's next", so an untouched one means the next steps have
/// not changed since the work stopped. A task that never ran counts from
/// its creation. `days = 0` turns the chip off.
pub fn stuck_tasks(
    conn: &Connection,
    now: jiff::Timestamp,
    days: u32,
) -> Result<Vec<i64>, StorageError> {
    if days == 0 {
        return Ok(Vec::new());
    }
    let cutoff = ts_to_ms(now) - i64::from(days) * 86_400_000;
    let mut stmt = conn.prepare(
        "SELECT t.id FROM tasks t
         WHERE t.status='open'
           AND COALESCE((SELECT MAX(i.end_ts) FROM intervals i WHERE i.task_id = t.id),
                        t.created_ts) <= ?1
           AND COALESCE((SELECT c.ts FROM checkpoints c WHERE c.task_id = t.id), 0) <= ?1
         ORDER BY t.id",
    )?;
    let rows = stmt.query_map([cutoff], |r| r.get(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    #[test]
    fn migrations_are_valid() {
        assert!(super::MIGRATIONS.validate().is_ok());
    }

    // m27 chunk 6: a consolidation run moves intervals and renames in one
    // transaction, records one `consolidate` row, and undo restores the
    // folded task — under a fresh id when its old id was reused meanwhile.
    #[test]
    fn consolidate_apply_and_undo_round_trip() {
        use crate::consolidate::Plan;
        let mut conn = Connection::open_in_memory().unwrap();
        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        for (id, label) in [(1, "keeper"), (2, "dupe"), (3, "vague")] {
            conn.execute(
                "INSERT INTO tasks (id, label, status, source, created_ts) VALUES (?1, ?2, 'open', 'derived', ?1)",
                rusqlite::params![id, label],
            )
            .unwrap();
        }
        for (id, task, lo) in [(10, 1, 0), (11, 2, 100), (12, 2, 200), (13, 3, 300)] {
            conn.execute(
                "INSERT INTO intervals (id, task_id, start_ts, end_ts, confidence) VALUES (?1, ?2, ?3, ?3 + 50, 0.9)",
                rusqlite::params![id, task, lo],
            )
            .unwrap();
        }
        let plan = Plan {
            merges: vec![(2, 1)],
            renames: vec![(3, "investigating vague thing".into())],
        };
        let ts = crate::types::ms_to_ts(1_000);
        let id = super::consolidate_apply(&mut conn, ts, "2026-09-03", &plan).unwrap();
        assert_eq!(
            super::consolidation_of_day(&conn, "2026-09-03").unwrap(),
            Some(id)
        );
        let owner = |conn: &Connection, iv: i64| -> i64 {
            conn.query_row("SELECT task_id FROM intervals WHERE id=?1", [iv], |r| {
                r.get(0)
            })
            .unwrap()
        };
        assert_eq!((owner(&conn, 11), owner(&conn, 12)), (1, 1));
        let label3: String = conn
            .query_row("SELECT label FROM tasks WHERE id=3", [], |r| r.get(0))
            .unwrap();
        assert_eq!(label3, "investigating vague thing");
        // The folded derived task, referenced by nothing, was pruned …
        let gone: i64 = conn
            .query_row("SELECT COUNT(*) FROM tasks WHERE id=2", [], |r| r.get(0))
            .unwrap();
        assert_eq!(gone, 0);
        // … and its id reused by an unrelated task before undo.
        conn.execute(
            "INSERT INTO tasks (id, label, status, source, created_ts) VALUES (2, 'newcomer', 'open', 'user', 5000)",
            [],
        )
        .unwrap();
        super::consolidate_undo(&mut conn, id).unwrap();
        assert_eq!(
            super::consolidation_of_day(&conn, "2026-09-03").unwrap(),
            None
        );
        let label3: String = conn
            .query_row("SELECT label FROM tasks WHERE id=3", [], |r| r.get(0))
            .unwrap();
        assert_eq!(label3, "vague");
        let newcomer: (String, String) = conn
            .query_row("SELECT label, status FROM tasks WHERE id=2", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(newcomer, ("newcomer".into(), "open".into()));
        let home = owner(&conn, 11);
        assert_ne!(home, 1);
        assert_ne!(home, 2);
        assert_eq!(owner(&conn, 12), home);
        let restored: (String, String) = conn
            .query_row("SELECT label, status FROM tasks WHERE id=?1", [home], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(restored, ("dupe".into(), "open".into()));
        let rows: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM corrections WHERE kind='consolidate'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rows, 0);
    }

    // 014: intervals get an insert timestamp from the trigger, derive metrics
    // round-trip, and a skipped AI job is terminal with both timestamps set.
    #[test]
    fn migration_014_metrics_trigger_and_skipped_jobs() {
        let mut conn = Connection::open_in_memory().unwrap();
        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO tasks (id, label, created_ts) VALUES (1, 't', 0)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO batches (id, start_ts, end_ts, status) VALUES (7, 0, 60000, 'done')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence)
             VALUES (1, 7, 0, 60000, 0.9)",
            [],
        )
        .unwrap();
        let created: Option<i64> = conn
            .query_row("SELECT created_ts FROM intervals", [], |r| r.get(0))
            .unwrap();
        assert!(
            created.is_some_and(|c| c > 1_700_000_000_000),
            "{created:?}"
        );

        assert!(super::last_derive(&conn).unwrap().is_none());
        super::record_derive_metrics(
            &conn,
            &super::DeriveMetrics {
                batch_id: 7,
                derived_ts: 5,
                derive_ms: 61_500,
                prompt_tokens: 2100,
                gen_tokens: 140,
            },
        )
        .unwrap();
        let d = super::last_derive(&conn).unwrap().unwrap();
        assert_eq!(
            (d.batch_id, d.derive_ms, d.prompt_tokens, d.gen_tokens),
            (7, 61_500, 2100, 140)
        );
        assert_eq!(super::pending_batch_count(&conn).unwrap(), 0);
        conn.execute(
            "INSERT INTO batches (id, start_ts, end_ts) VALUES (8, 60000, 120000)",
            [],
        )
        .unwrap();
        assert_eq!(super::pending_batch_count(&conn).unwrap(), 1);

        let ts = crate::types::ms_to_ts(1_000);
        let id = super::enqueue_ai_job(&conn, ts, "journal", 0, "{}").unwrap();
        super::claim_ai_job(&conn, id).unwrap().unwrap();
        super::skip_ai_job(&conn, id, "gone").unwrap();
        let (status, started, finished): (String, Option<i64>, Option<i64>) = conn
            .query_row(
                "SELECT status, started_ts, finished_ts FROM ai_jobs WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(status, "skipped");
        assert!(started.is_some() && finished.is_some());
        assert!(super::claim_ai_job(&conn, id).unwrap().is_none());
    }

    // 004 rebuilds `tasks` under live children: existing rows must migrate
    // 1:1 (identity + one interval), corrections stay valid, FTS rebuilt.
    #[test]
    fn migration_004_preserves_task_data() {
        let mut conn = Connection::open_in_memory().unwrap();
        // Mirror open(): bundled sqlite defaults foreign_keys ON.
        conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
        let pre = rusqlite_migration::Migrations::new(vec![
            rusqlite_migration::M::up(include_str!("../migrations/001_schema.sql")),
            rusqlite_migration::M::up(include_str!("../migrations/002_corrections_fts.sql")),
            rusqlite_migration::M::up(include_str!("../migrations/003_spans_url_meta.sql")),
        ]);
        pre.to_latest(&mut conn).unwrap();
        conn.execute_batch(
            "INSERT INTO batches (id, start_ts, end_ts, status) VALUES (7, 100, 200, 'done');
             INSERT INTO tasks (id, batch_id, label, project, start_ts, end_ts, confidence)
                 VALUES (42, 7, 'old label', 'proj', 110, 190, 0.8);
             INSERT INTO corrections (ts, task_id, old_label, new_label, ctx)
                 VALUES (150, 42, 'old label', 'better label', 'term ctx');",
        )
        .unwrap();

        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        conn.execute_batch("PRAGMA foreign_key_check").unwrap();

        let (label, status, source, created): (String, String, String, i64) = conn
            .query_row(
                "SELECT label, status, source, created_ts FROM tasks WHERE id=42",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (label.as_str(), status.as_str(), source.as_str(), created),
            ("old label", "open", "derived", 110)
        );
        let (task_id, batch_id, start, end): (i64, i64, i64, i64) = conn
            .query_row(
                "SELECT task_id, batch_id, start_ts, end_ts FROM intervals",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!((task_id, batch_id, start, end), (42, 7, 110, 190));
        let kind: String = conn
            .query_row("SELECT kind FROM corrections WHERE task_id=42", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(kind, "rename");
        let hits: i64 = conn
            .query_row(
                "SELECT count(*) FROM tasks_fts WHERE tasks_fts MATCH 'label'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(hits, 1, "FTS rebuilt over migrated identity labels");
    }

    // 005 backfills existing chat history into conversation 1; an empty
    // database gets no conversation row.
    #[test]
    fn migration_005_backfills_chat_history() {
        let mut conn = Connection::open_in_memory().unwrap();
        let pre = rusqlite_migration::Migrations::new(vec![
            rusqlite_migration::M::up(include_str!("../migrations/001_schema.sql")),
            rusqlite_migration::M::up(include_str!("../migrations/002_corrections_fts.sql")),
            rusqlite_migration::M::up(include_str!("../migrations/003_spans_url_meta.sql")),
            rusqlite_migration::M::up(include_str!("../migrations/004_task_identity.sql")),
        ]);
        pre.to_latest(&mut conn).unwrap();
        conn.execute_batch(
            "INSERT INTO chat_messages (ts, role, content) VALUES
                 (100, 'user', 'q'), (200, 'assistant', 'a');",
        )
        .unwrap();

        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        let (id, created): (i64, i64) = conn
            .query_row("SELECT id, created_ts FROM conversations", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((id, created), (1, 100));
        let orphans: i64 = conn
            .query_row(
                "SELECT count(*) FROM chat_messages WHERE conversation_id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphans, 0);
    }

    #[test]
    fn migration_005_empty_db_gets_no_conversation() {
        let mut conn = Connection::open_in_memory().unwrap();
        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM conversations", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }

    // 006 adds tasks.description (NULL for existing rows) plus the ai_jobs
    // queue and narratives cache.
    #[test]
    fn migration_006_adds_description_and_ai_jobs() {
        let mut conn = Connection::open_in_memory().unwrap();
        let pre = rusqlite_migration::Migrations::new(vec![
            rusqlite_migration::M::up(include_str!("../migrations/001_schema.sql")),
            rusqlite_migration::M::up(include_str!("../migrations/002_corrections_fts.sql")),
            rusqlite_migration::M::up(include_str!("../migrations/003_spans_url_meta.sql")),
            rusqlite_migration::M::up(include_str!("../migrations/004_task_identity.sql")),
            rusqlite_migration::M::up(include_str!("../migrations/005_chat_conversations.sql")),
        ]);
        pre.to_latest(&mut conn).unwrap();
        conn.execute_batch(
            "INSERT INTO tasks (id, label, status, source, created_ts)
                 VALUES (1, 'work', 'open', 'user', 100);",
        )
        .unwrap();

        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        conn.execute_batch("PRAGMA foreign_key_check").unwrap();
        let desc: Option<String> = conn
            .query_row("SELECT description FROM tasks WHERE id=1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(desc, None);
        for table in ["ai_jobs", "narratives"] {
            let n: i64 = conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table} exists and starts empty");
        }
    }

    /// The bundled migrations up to and including `n` (1-based).
    fn migrations_upto(n: usize) -> rusqlite_migration::Migrations<'static> {
        let all = [
            include_str!("../migrations/001_schema.sql"),
            include_str!("../migrations/002_corrections_fts.sql"),
            include_str!("../migrations/003_spans_url_meta.sql"),
            include_str!("../migrations/004_task_identity.sql"),
            include_str!("../migrations/005_chat_conversations.sql"),
            include_str!("../migrations/006_task_descriptions.sql"),
            include_str!("../migrations/007_vcs_events.sql"),
            include_str!("../migrations/008_task_workspace.sql"),
            include_str!("../migrations/009_standup_drafts.sql"),
            include_str!("../migrations/010_activity_events.sql"),
            include_str!("../migrations/011_interval_source.sql"),
        ];
        rusqlite_migration::Migrations::new(
            all[..n]
                .iter()
                .map(|m| rusqlite_migration::M::up(m))
                .collect(),
        )
    }

    #[test]
    fn migration_011_marks_m23_assigns_as_user() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
        migrations_upto(10).to_latest(&mut conn).unwrap();
        conn.execute_batch(
            "INSERT INTO batches (id, start_ts, end_ts, status) VALUES (1, 0, 100, 'done'), (2, 100, 200, 'done');
             INSERT INTO tasks (id, label, status, source, created_ts) VALUES
               (1, 'assigned', 'open', 'user', 0), (2, 'derived', 'open', 'derived', 0);
             -- 1: the assign's recorded interval; 2: same assign, second batch;
             -- 3: model 1.0 on a task never assigned; 4: model 0.9 on the assigned task.
             INSERT INTO intervals (id, task_id, batch_id, start_ts, end_ts, confidence) VALUES
               (1, 1, 1, 0, 50, 1.0), (2, 1, 2, 100, 150, 1.0), (3, 2, 1, 50, 100, 1.0), (4, 1, 2, 150, 200, 0.9);
             INSERT INTO corrections (ts, task_id, old_label, new_label, ctx, kind, interval_id)
               VALUES (0, 1, '(unassigned)', 'assigned', '', 'assign', 1);",
        )
        .unwrap();
        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        let sources: Vec<(i64, String)> = conn
            .prepare("SELECT id, source FROM intervals ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            sources,
            vec![
                (1, "user".into()),
                (2, "user".into()),
                (3, "derived".into()),
                (4, "derived".into())
            ]
        );
    }

    #[test]
    fn migration_012_rebuild_keeps_ids_refs_and_allows_tail_rows() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "OFF").unwrap();
        migrations_upto(11).to_latest(&mut conn).unwrap();
        conn.execute_batch(
            "INSERT INTO batches (id, start_ts, end_ts, status) VALUES (1, 0, 100, 'done');
             INSERT INTO tasks (id, label, status, source, created_ts) VALUES (1, 't', 'open', 'user', 0);
             INSERT INTO intervals (id, task_id, batch_id, start_ts, end_ts, confidence, source)
               VALUES (5, 1, 1, 0, 50, 1.0, 'user');
             INSERT INTO corrections (ts, task_id, old_label, new_label, ctx, kind, interval_id)
               VALUES (0, 1, '(unassigned)', 't', '', 'assign', 5);",
        )
        .unwrap();
        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        let (id, batch, source, reason): (i64, Option<i64>, String, Option<String>) = conn
            .query_row(
                "SELECT id, batch_id, source, reason FROM intervals",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            (id, batch, source.as_str(), reason),
            (5, Some(1), "user", None)
        );
        // The correction's ref still binds: the row cannot be deleted under it.
        assert!(
            conn.execute("DELETE FROM intervals WHERE id=5", [])
                .is_err()
        );
        conn.execute(
            "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence, source, reason)
             VALUES (1, NULL, 200, 300, 0.5, 'prepass', 'branch X-1')",
            [],
        )
        .unwrap();
        let n: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM intervals WHERE batch_id IS NULL",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn latest_vcs_event_per_repo_picks_newest_of_each() {
        use crate::types::{ActivityEvent, ActivityKind};
        let mut conn = Connection::open_in_memory().unwrap();
        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        let ev = |ms: i64, repo: &str, kind: ActivityKind, branch: &str| {
            let ext_id = matches!(kind, ActivityKind::Commit).then(|| "abc".to_owned());
            ActivityEvent {
                ts: crate::types::ms_to_ts(ms),
                repo: repo.into(),
                branch: branch.into(),
                kind,
                ext_id,
                end_ts: None,
                summary: None,
            }
        };
        assert!(super::latest_vcs_event_per_repo(&conn).unwrap().is_empty());
        super::insert_activity_event(&conn, &ev(1_000, "a", ActivityKind::Checkout, "main"))
            .unwrap();
        super::insert_activity_event(&conn, &ev(2_000, "b", ActivityKind::Checkout, "feat"))
            .unwrap();
        super::insert_activity_event(&conn, &ev(3_000, "a", ActivityKind::Commit, "main")).unwrap();
        let latest = super::latest_vcs_event_per_repo(&conn).unwrap();
        assert_eq!(latest.len(), 2);
        assert_eq!(latest[0].repo, "a");
        assert!(matches!(latest[0].kind, ActivityKind::Commit));
        assert_eq!(latest[0].ts, crate::types::ms_to_ts(3_000));
        assert_eq!(
            (latest[1].repo.as_str(), latest[1].branch.as_str()),
            ("b", "feat")
        );
    }

    #[test]
    fn latest_ai_job_is_newest_of_its_kind() {
        let mut conn = Connection::open_in_memory().unwrap();
        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        let ts = crate::types::ms_to_ts;
        assert!(
            super::latest_ai_job(&conn, "fetch_context")
                .unwrap()
                .is_none()
        );
        let first = super::enqueue_ai_job(&conn, ts(1_000), "fetch_context", 0, "{}").unwrap();
        super::fail_ai_job(&conn, first, "boom").unwrap();
        let (status, created, err) = super::latest_ai_job(&conn, "fetch_context")
            .unwrap()
            .unwrap();
        assert_eq!(
            (status.as_str(), created, err.as_deref()),
            ("failed", ts(1_000), Some("boom"))
        );
        let second = super::enqueue_ai_job(&conn, ts(2_000), "fetch_context", 0, "{}").unwrap();
        super::complete_ai_job(&conn, second, "ok").unwrap();
        super::enqueue_ai_job(&conn, ts(3_000), "narrative", 0, "{}").unwrap();
        let (status, created, err) = super::latest_ai_job(&conn, "fetch_context")
            .unwrap()
            .unwrap();
        assert_eq!((status.as_str(), created, err), ("done", ts(2_000), None));
    }

    // Priority beats insertion order; the interactive floor filters; a claim
    // burns an attempt and a second claim of a running job fails.
    #[test]
    fn ai_job_queue_priority_and_claim() {
        let mut conn = Connection::open_in_memory().unwrap();
        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        let ts = crate::types::ms_to_ts(1_000);
        let low = super::enqueue_ai_job(&conn, ts, "narrative", 0, "{}").unwrap();
        let high = super::enqueue_ai_job(&conn, ts, "suggest_task", 10, "{}").unwrap();

        // Interactive floor sees only the high-priority job.
        assert_eq!(
            super::next_eligible_ai_job(&conn, super::AI_JOB_INTERACTIVE).unwrap(),
            Some(high)
        );
        // Unfiltered, the high-priority job still claims first despite being
        // inserted second.
        assert_eq!(super::next_eligible_ai_job(&conn, 0).unwrap(), Some(high));

        let job = super::claim_ai_job(&conn, high).unwrap().unwrap();
        assert_eq!(
            (job.kind.as_str(), job.payload.as_str()),
            ("suggest_task", "{}")
        );
        // Running jobs are not eligible; a re-claim returns None.
        assert!(super::claim_ai_job(&conn, high).unwrap().is_none());
        assert_eq!(super::next_eligible_ai_job(&conn, 0).unwrap(), Some(low));

        super::complete_ai_job(&conn, high, "ok").unwrap();
        assert_eq!(
            super::ai_job_status(&conn, high).unwrap(),
            Some(("done".into(), Some("ok".into())))
        );
        // Failed jobs retry once, then drop out.
        super::claim_ai_job(&conn, low).unwrap().unwrap();
        super::fail_ai_job(&conn, low, "boom").unwrap();
        assert_eq!(super::next_eligible_ai_job(&conn, 0).unwrap(), Some(low));
        super::claim_ai_job(&conn, low).unwrap().unwrap();
        super::fail_ai_job(&conn, low, "boom").unwrap();
        assert_eq!(super::next_eligible_ai_job(&conn, 0).unwrap(), None);
    }

    // 008 adds the workspace tables; task deletion cascades workspace rows
    // and unscopes (not deletes) the task's conversation, so the existing
    // orphan-guard and prune statements stay valid.
    #[test]
    fn migration_008_workspace_rows_cascade_with_task() {
        let mut conn = Connection::open_in_memory().unwrap();
        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        let ts = crate::types::ms_to_ts(1_000);
        conn.execute_batch(
            "INSERT INTO batches (id, start_ts, end_ts, status) VALUES (1, 0, 100, 'done');
             INSERT INTO tasks (id, label, status, source, created_ts)
                 VALUES (5, 'work', 'open', 'derived', 10);",
        )
        .unwrap();
        super::upsert_task_context(&conn, 5, "mcp", ts, "ticket body").unwrap();
        super::insert_journal_entry(&conn, 5, 1, 10, 90, "did things", "[1]").unwrap();
        super::upsert_checkpoint(&conn, 5, ts, "state", "next").unwrap();
        let conv = super::conversation_for_task(&conn, 5, ts).unwrap();

        conn.execute("DELETE FROM tasks WHERE id=5", []).unwrap();
        for table in ["task_context", "journal_entries", "checkpoints"] {
            let n: i64 = conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table} cascades with its task");
        }
        let scoped: Option<i64> = conn
            .query_row(
                "SELECT task_id FROM conversations WHERE id=?1",
                [conv],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(scoped, None, "conversation survives unscoped");
    }

    // Context upsert replaces; checkpoint upsert overwrites; journal tail is
    // chronological and capped; the per-task conversation is created once.
    #[test]
    fn workspace_accessors() {
        let mut conn = Connection::open_in_memory().unwrap();
        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        let ts = crate::types::ms_to_ts(2_000);
        conn.execute_batch(
            "INSERT INTO batches (id, start_ts, end_ts, status) VALUES
                 (1, 0, 100, 'done'), (2, 100, 200, 'done'), (3, 200, 300, 'done');
             INSERT INTO tasks (id, label, status, source, created_ts)
                 VALUES (5, 'work', 'open', 'user', 10);",
        )
        .unwrap();

        super::upsert_task_context(&conn, 5, "mcp", crate::types::ms_to_ts(1_000), "v1").unwrap();
        super::upsert_task_context(&conn, 5, "mcp", ts, "v2").unwrap();
        assert_eq!(
            super::task_context(&conn, 5).unwrap(),
            Some((2_000, "v2".into()))
        );
        assert_eq!(super::task_context(&conn, 6).unwrap(), None);

        for (i, entry) in ["one", "two", "three"].iter().enumerate() {
            let t = 10 + i as i64 * 10;
            super::insert_journal_entry(&conn, 5, 1 + i as i64, t, t + 5, entry, "[]").unwrap();
        }
        let tail = super::journal_tail(&conn, 5, 2).unwrap();
        assert_eq!(
            tail.iter().map(|e| e.entry.as_str()).collect::<Vec<_>>(),
            ["two", "three"],
            "tail keeps newest n, reads oldest-first"
        );

        super::upsert_checkpoint(&conn, 5, crate::types::ms_to_ts(1_000), "s1", "n1").unwrap();
        super::upsert_checkpoint(&conn, 5, ts, "s2", "n2").unwrap();
        let cp = super::get_checkpoint(&conn, 5).unwrap().unwrap();
        assert_eq!(
            (cp.ts, cp.state.as_str(), cp.next_steps.as_str()),
            (2_000, "s2", "n2")
        );

        let a = super::conversation_for_task(&conn, 5, ts).unwrap();
        let b = super::conversation_for_task(&conn, 5, ts).unwrap();
        assert_eq!(a, b, "same task resumes the same conversation");
    }

    // Batch-scoped evidence covers only this batch's intervals; the journal
    // upsert replaces a re-derived batch's entry instead of duplicating.
    #[test]
    fn task_batch_evidence_and_journal_upsert() {
        let mut conn = Connection::open_in_memory().unwrap();
        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        conn.execute_batch(
            "INSERT INTO batches (id, start_ts, end_ts, status) VALUES
                 (1, 0, 100, 'done'), (2, 100, 200, 'done');
             INSERT INTO tasks (id, label, status, source, created_ts)
                 VALUES (5, 'work', 'open', 'user', 10);
             INSERT INTO intervals (id, task_id, batch_id, start_ts, end_ts, confidence) VALUES
                 (11, 5, 1, 10, 90, 0.9),
                 (12, 5, 2, 110, 190, 0.9);
             INSERT INTO spans (batch_id, start_ts, end_ts, app, title, kind) VALUES
                 (1, 10, 90, 'code', 'editing foo.rs', 'focus'),
                 (2, 110, 190, 'code', 'editing bar.rs', 'focus');",
        )
        .unwrap();
        let (evidence, ids, lo, hi) = super::task_batch_evidence(&conn, 5, 1).unwrap().unwrap();
        assert_eq!(evidence, "code editing foo.rs\n", "batch 2's span excluded");
        assert_eq!((ids, lo, hi), (vec![11], 10, 90));
        assert!(super::task_batch_evidence(&conn, 5, 99).unwrap().is_none());

        super::insert_journal_entry(&conn, 5, 1, 10, 90, "first", "[11]").unwrap();
        super::insert_journal_entry(&conn, 5, 1, 10, 90, "replaced", "[11]").unwrap();
        let tail = super::journal_tail(&conn, 5, 10).unwrap();
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].entry, "replaced");
    }

    // Only tasks with interval activity since their checkpoint (within the
    // 24 h window before the idle instant) need a new checkpoint.
    #[test]
    fn tasks_needing_checkpoint_filters() {
        let mut conn = Connection::open_in_memory().unwrap();
        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        let now = 200_000_000i64;
        conn.execute_batch(&format!(
            "INSERT INTO batches (id, start_ts, end_ts, status) VALUES (1, 0, 100, 'done');
             INSERT INTO tasks (id, label, status, source, created_ts) VALUES
                 (1, 'fresh', 'open', 'user', 10),
                 (2, 'checkpointed', 'open', 'user', 10),
                 (3, 'stale', 'open', 'user', 10);
             INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence) VALUES
                 (1, 1, {a}, {b}, 0.9),
                 (2, 1, {a}, {b}, 0.9),
                 (3, 1, 100, 200, 0.9);",
            a = now - 10_000,
            b = now - 5_000,
        ))
        .unwrap();
        // Task 2's checkpoint postdates its activity; task 3 is older than 24 h.
        super::upsert_checkpoint(&conn, 2, crate::types::ms_to_ts(now), "s", "n").unwrap();
        assert_eq!(super::tasks_needing_checkpoint(&conn, now).unwrap(), [1]);
    }

    #[test]
    fn standup_draft_upsert_replaces() {
        let mut conn = Connection::open_in_memory().unwrap();
        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        let ts = crate::types::ms_to_ts(1_000);
        super::upsert_standup_draft(&conn, "2026-09-01", ts, "first").unwrap();
        super::upsert_standup_draft(&conn, "2026-09-01", crate::types::ms_to_ts(2_000), "second")
            .unwrap();
        assert_eq!(
            super::get_standup_draft(&conn, "2026-09-01").unwrap(),
            Some((2_000, "second".to_owned()))
        );
        assert_eq!(super::get_standup_draft(&conn, "2026-09-02").unwrap(), None);
    }

    // The digest groups a day's journal entries by task (chronological within
    // each), excludes out-of-window entries, and attaches only checkpoints
    // written at or after the window start.
    #[test]
    fn standup_digest_gathers_tasks_and_checkpoints() {
        let mut conn = Connection::open_in_memory().unwrap();
        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        conn.execute_batch(
            "INSERT INTO batches (id, start_ts, end_ts, status) VALUES
                 (1, 0, 100, 'done'), (2, 100, 300, 'done'), (3, 300, 900, 'done');
             INSERT INTO tasks (id, label, project, status, source, created_ts) VALUES
                 (1, 'alpha', 'proj', 'open', 'user', 10),
                 (2, 'beta', NULL, 'open', 'user', 10);",
        )
        .unwrap();
        // Task 1: one entry before the window, two inside. Task 2: one inside.
        super::insert_journal_entry(&conn, 1, 1, 50, 90, "too early", "[]").unwrap();
        super::insert_journal_entry(&conn, 1, 2, 200, 250, "morning", "[]").unwrap();
        super::insert_journal_entry(&conn, 1, 3, 300, 350, "afternoon", "[]").unwrap();
        super::insert_journal_entry(&conn, 2, 2, 220, 260, "beta work", "[]").unwrap();
        // Task 1's checkpoint predates the window: dropped. Task 2's is fresh.
        super::upsert_checkpoint(&conn, 1, crate::types::ms_to_ts(90), "old", "old next").unwrap();
        super::upsert_checkpoint(&conn, 2, crate::types::ms_to_ts(260), "state", "next").unwrap();
        let rows = super::standup_digest(&conn, 100, 400).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].label, "alpha");
        assert_eq!(rows[0].project.as_deref(), Some("proj"));
        let entries: Vec<&str> = rows[0].entries.iter().map(|e| e.entry.as_str()).collect();
        assert_eq!(entries, ["morning", "afternoon"]);
        assert!(rows[0].checkpoint.is_none(), "stale checkpoint dropped");
        assert_eq!(rows[1].label, "beta");
        assert_eq!(rows[1].checkpoint.as_ref().unwrap().state, "state");
        assert!(super::standup_digest(&conn, 500, 600).unwrap().is_empty());
    }

    #[test]
    fn pending_standup_job_dedupes() {
        let mut conn = Connection::open_in_memory().unwrap();
        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        assert!(!super::pending_standup_job(&conn, "2026-09-01").unwrap());
        let ts = crate::types::ms_to_ts(1_000);
        super::enqueue_ai_job(
            &conn,
            ts,
            "standup",
            0,
            &super::standup_payload("2026-09-01"),
        )
        .unwrap();
        assert!(super::pending_standup_job(&conn, "2026-09-01").unwrap());
        assert!(!super::pending_standup_job(&conn, "2026-09-02").unwrap());
    }

    // Workspace edits rewrite the row and log a correction; saving unchanged
    // text logs nothing.
    #[test]
    fn workspace_edits_log_corrections() {
        let mut conn = Connection::open_in_memory().unwrap();
        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        conn.execute_batch(
            "INSERT INTO batches (id, start_ts, end_ts, status) VALUES (1, 0, 100, 'done');
             INSERT INTO tasks (id, label, status, source, created_ts)
                 VALUES (5, 'work', 'open', 'user', 10);",
        )
        .unwrap();
        super::insert_journal_entry(&conn, 5, 1, 10, 90, "draft text", "[]").unwrap();
        let entry_id = super::journal_tail(&conn, 5, 1).unwrap()[0].id;
        let ts = crate::types::ms_to_ts(1_000);
        super::update_journal_entry(&mut conn, ts, entry_id, "draft text").unwrap();
        super::update_journal_entry(&mut conn, ts, entry_id, "fixed text").unwrap();
        assert_eq!(
            super::journal_tail(&conn, 5, 1).unwrap()[0].entry,
            "fixed text"
        );

        super::upsert_checkpoint(&conn, 5, ts, "state", "next").unwrap();
        super::update_checkpoint(&mut conn, ts, 5, "state", "next").unwrap();
        super::update_checkpoint(&mut conn, ts, 5, "better state", "better next").unwrap();
        let cp = super::get_checkpoint(&conn, 5).unwrap().unwrap();
        assert_eq!(
            (cp.state.as_str(), cp.next_steps.as_str(), cp.ts),
            ("better state", "better next", 1_000,)
        );

        let log: Vec<(String, String, String)> = conn
            .prepare("SELECT kind, old_label, new_label FROM corrections ORDER BY id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            log,
            [
                (
                    "journal".to_owned(),
                    "draft text".to_owned(),
                    "fixed text".to_owned()
                ),
                (
                    "checkpoint".to_owned(),
                    "state".to_owned(),
                    "better state".to_owned()
                ),
            ]
        );
    }

    // Stuck: open, no interval for `days`, and no checkpoint written since
    // the cutoff (a fresh checkpoint means the next steps moved on).
    #[test]
    fn stuck_tasks_needs_idle_intervals_and_a_stale_checkpoint() {
        let mut conn = Connection::open_in_memory().unwrap();
        super::MIGRATIONS.to_latest(&mut conn).unwrap();
        let now = 10 * 86_400_000i64;
        let day = 86_400_000i64;
        conn.execute_batch(&format!(
            "INSERT INTO batches (id, start_ts, end_ts, status) VALUES (1, 0, 100, 'done');
             INSERT INTO tasks (id, label, status, source, created_ts)
                 VALUES (1, 'idle', 'open', 'user', 0),
                        (2, 'worked on today', 'open', 'user', 0),
                        (3, 'replanned', 'open', 'user', 0),
                        (4, 'closed', 'closed', 'user', 0),
                        (5, 'declared just now', 'open', 'user', {now});
             INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence)
                 VALUES (1, 1, {five_days_ago}, {five_days_ago}, 0.9),
                        (2, 1, {now}, {now}, 0.9),
                        (3, 1, {five_days_ago}, {five_days_ago}, 0.9),
                        (4, 1, {five_days_ago}, {five_days_ago}, 0.9);
             INSERT INTO checkpoints (task_id, ts, state, next_steps)
                 VALUES (1, {five_days_ago}, 'stalled', 'ask James'),
                        (3, {now}, 'replanned', 'new next step');",
            now = now,
            five_days_ago = now - 5 * day,
        ))
        .unwrap();
        let ts = crate::types::ms_to_ts(now);
        assert_eq!(super::stuck_tasks(&conn, ts, 3).unwrap(), vec![1]);
        // Wider window: the idle task is inside it again, so nothing is stuck.
        assert!(super::stuck_tasks(&conn, ts, 7).unwrap().is_empty());
        // 0 = off.
        assert!(super::stuck_tasks(&conn, ts, 0).unwrap().is_empty());
    }
}
