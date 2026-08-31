use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use rusqlite::{Connection, params};
use rusqlite_migration::{M, Migrations};

use crate::sessionizer::{BatchDraft, SpanDraft, SpanKind};
use crate::types::{
    CaptureEvent, Correction, Event, FocusEvent, NewInterval, OpenTask, Task, TaskSlot, ms_to_ts,
    ts_to_ms,
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
    ])
});

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
    }
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
    tx.commit()?;
    Ok(())
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
    Ok(conn.execute(
        "UPDATE batches SET status='failed' WHERE status='running'",
        [],
    )?)
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
    const STMTS: [&str; 6] = [
        "DELETE FROM events WHERE id IN (SELECT id FROM events WHERE ts < ?1 LIMIT ?2)",
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
         AND id NOT IN (SELECT batch_id FROM intervals) LIMIT ?2)",
        "DELETE FROM chat_messages WHERE id IN (SELECT id FROM chat_messages WHERE ts < ?1 LIMIT ?2)",
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
/// idempotent.
pub fn store_derivation(
    conn: &mut Connection,
    batch_id: i64,
    slots: &[TaskSlot],
    intervals: &[NewInterval],
) -> Result<(), StorageError> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM intervals WHERE batch_id=?1", [batch_id])?;
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
    for iv in intervals {
        let Some(Some(task_id)) = ids.get(iv.slot) else {
            continue;
        };
        tx.execute(
            "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                task_id,
                batch_id,
                ts_to_ms(iv.start_ts),
                ts_to_ms(iv.end_ts),
                iv.confidence
            ],
        )?;
    }
    tx.execute(DELETE_ORPHAN_TASKS, [])?;
    tx.execute("UPDATE batches SET status='done' WHERE id=?1", [batch_id])?;
    tx.commit()?;
    Ok(())
}

/// Focus spans overlapping any of the task's intervals, as "app title" lines
/// (FTS-searchable snapshot of what the corrected work looked like).
fn task_span_ctx(tx: &rusqlite::Transaction, task_id: i64) -> Result<String, StorageError> {
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

/// Move one interval to another task ('reassign' correction). The old task's
/// label → new task's label pair plus the interval's span context become
/// teaching data; an orphaned derived source task is removed.
pub fn reassign_interval(
    conn: &mut Connection,
    ts: jiff::Timestamp,
    interval_id: i64,
    to_task: i64,
) -> Result<(), StorageError> {
    let tx = conn.transaction()?;
    let (from_task, start_ts, end_ts): (i64, i64, i64) = tx.query_row(
        "SELECT task_id, start_ts, end_ts FROM intervals WHERE id=?1",
        [interval_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    if from_task == to_task {
        return Ok(());
    }
    let ident = |id: i64| -> Result<(String, Option<String>), rusqlite::Error> {
        tx.query_row("SELECT label, project FROM tasks WHERE id=?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
    };
    let (old_label, old_project) = ident(from_task)?;
    let (new_label, new_project) = ident(to_task)?;
    let mut ctx = String::new();
    {
        let mut stmt = tx.prepare(
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
    }
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
        "UPDATE intervals SET task_id=?1 WHERE id=?2",
        params![to_task, interval_id],
    )?;
    tx.execute(DELETE_ORPHAN_TASKS, [])?;
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

/// Top-k past corrections whose stored span context matches the given batch
/// spans (FTS over titles + apps, bm25-ranked, deduped by resulting label).
pub fn similar_corrections(
    conn: &Connection,
    spans: &[SpanDraft],
    k: usize,
) -> Result<Vec<Correction>, StorageError> {
    let query = fts_or_query(spans);
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(
        "SELECT c.old_label, c.new_label, c.old_project, c.new_project
         FROM corrections_fts f JOIN corrections c ON c.id = f.rowid
         WHERE corrections_fts MATCH ?1 ORDER BY f.rank LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![query, (k * 4) as i64], |r| {
        Ok(Correction {
            old_label: r.get(0)?,
            new_label: r.get(1)?,
            old_project: r.get(2)?,
            new_project: r.get(3)?,
        })
    })?;
    let mut out: Vec<Correction> = Vec::new();
    for c in rows {
        let c = c?;
        if out
            .iter()
            .any(|o| o.new_label == c.new_label && o.new_project == c.new_project)
        {
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
    })
}

const TASK_COLS: &str = "t.id, i.id, t.label, t.project, i.start_ts, i.end_ts, i.confidence, \
     t.source='user'";

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
    role: &str,
    content: &str,
) -> Result<(), StorageError> {
    conn.execute(
        "INSERT INTO chat_messages (ts, role, content) VALUES (?1, ?2, ?3)",
        params![ts_to_ms(ts), role, content],
    )?;
    Ok(())
}

/// Last `n` chat messages, oldest first.
pub fn recent_chat_messages(
    conn: &Connection,
    n: usize,
) -> Result<Vec<(String, String)>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT role, content FROM (
             SELECT id, role, content FROM chat_messages ORDER BY id DESC LIMIT ?1
         ) ORDER BY id",
    )?;
    let rows = stmt.query_map([n as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    #[test]
    fn migrations_are_valid() {
        assert!(super::MIGRATIONS.validate().is_ok());
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
}
