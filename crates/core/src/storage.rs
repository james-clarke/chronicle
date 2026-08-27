use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use rusqlite::{Connection, params};
use rusqlite_migration::{M, Migrations};

use crate::sessionizer::{BatchDraft, SpanDraft, SpanKind};
use crate::types::{
    CaptureEvent, Correction, Event, FocusEvent, NewTask, Task, ms_to_ts, ts_to_ms,
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
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.busy_timeout(Duration::from_secs(5))?;
    MIGRATIONS.to_latest(&mut conn)?;
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
/// triggers. Corrections are user teaching data and are never pruned; a task
/// referenced by one is kept as its context (and the FK requires it), as is
/// a batch still referenced by surviving spans or tasks.
pub fn prune(conn: &Connection, cutoff_ms: i64, batch: usize) -> Result<u64, StorageError> {
    const STMTS: [&str; 5] = [
        "DELETE FROM events WHERE id IN (SELECT id FROM events WHERE ts < ?1 LIMIT ?2)",
        "DELETE FROM spans WHERE id IN (SELECT id FROM spans WHERE end_ts < ?1 LIMIT ?2)",
        "DELETE FROM tasks WHERE id IN (SELECT id FROM tasks WHERE end_ts < ?1 \
         AND id NOT IN (SELECT task_id FROM corrections) LIMIT ?2)",
        "DELETE FROM batches WHERE id IN (SELECT id FROM batches WHERE end_ts < ?1 \
         AND id NOT IN (SELECT batch_id FROM spans WHERE batch_id IS NOT NULL) \
         AND id NOT IN (SELECT batch_id FROM tasks) LIMIT ?2)",
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

/// Last `n` task labels ending at or before `before_ms`, newest first
/// (digest continuity hint).
pub fn recent_labels_before(
    conn: &Connection,
    before_ms: i64,
    n: usize,
) -> Result<Vec<String>, StorageError> {
    let mut stmt =
        conn.prepare("SELECT label FROM tasks WHERE end_ts <= ?1 ORDER BY end_ts DESC LIMIT ?2")?;
    let rows = stmt.query_map(params![before_ms, n as i64], |r| r.get(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Replace the batch's tasks and mark it done. Replacing (not appending)
/// keeps a retried batch idempotent.
pub fn store_tasks(
    conn: &mut Connection,
    batch_id: i64,
    tasks: &[NewTask],
) -> Result<(), StorageError> {
    let tx = conn.transaction()?;
    tx.execute("DELETE FROM tasks WHERE batch_id=?1", [batch_id])?;
    for t in tasks {
        tx.execute(
            "INSERT INTO tasks (batch_id, label, project, start_ts, end_ts, confidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                batch_id,
                t.label,
                t.project,
                ts_to_ms(t.start_ts),
                ts_to_ms(t.end_ts),
                t.confidence
            ],
        )?;
    }
    tx.execute("UPDATE batches SET status='done' WHERE id=?1", [batch_id])?;
    tx.commit()?;
    Ok(())
}

/// Record a user edit of a task's label/project and apply it to the task row.
/// The correction stores an FTS-searchable snapshot of the span context the
/// task covered, so future batches with similar activity can retrieve it.
pub fn insert_correction(
    conn: &mut Connection,
    ts: jiff::Timestamp,
    task_id: i64,
    new_label: &str,
    new_project: Option<&str>,
) -> Result<(), StorageError> {
    let tx = conn.transaction()?;
    let (old_label, old_project, batch_id, start_ts, end_ts): (
        String,
        Option<String>,
        i64,
        i64,
        i64,
    ) = tx.query_row(
        "SELECT label, project, batch_id, start_ts, end_ts FROM tasks WHERE id=?1",
        [task_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    let mut ctx = String::new();
    {
        let mut stmt = tx.prepare(
            "SELECT DISTINCT app, title FROM spans
             WHERE batch_id=?1 AND kind='focus' AND start_ts < ?2 AND end_ts > ?3
             ORDER BY app, title",
        )?;
        let mut rows = stmt.query(params![batch_id, end_ts, start_ts])?;
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
        "INSERT INTO corrections (ts, task_id, old_label, new_label, old_project, new_project, ctx)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
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
        batch_id: r.get(1)?,
        label: r.get(2)?,
        project: r.get(3)?,
        start_ts: ms_to_ts(r.get(4)?),
        end_ts: ms_to_ts(r.get(5)?),
        confidence: r.get(6)?,
    })
}

const TASK_COLS: &str = "id, batch_id, label, project, start_ts, end_ts, confidence";

pub fn tasks_in_range(conn: &Connection, lo: i64, hi: i64) -> Result<Vec<Task>, StorageError> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {TASK_COLS} FROM tasks
         WHERE end_ts > ?1 AND start_ts < ?2 ORDER BY start_ts, id"
    ))?;
    let rows = stmt.query_map([lo, hi], task_from_row)?;
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

/// Top-k tasks whose label matches the FTS query, bm25-ranked.
pub fn search_tasks(conn: &Connection, query: &str, k: usize) -> Result<Vec<Task>, StorageError> {
    if query.is_empty() {
        return Ok(Vec::new());
    }
    let mut stmt = conn.prepare(&format!(
        "SELECT {TASK_COLS} FROM tasks
         WHERE id IN (SELECT rowid FROM tasks_fts WHERE tasks_fts MATCH ?1
                      ORDER BY rank LIMIT ?2)
         ORDER BY start_ts, id"
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
    #[test]
    fn migrations_are_valid() {
        assert!(super::MIGRATIONS.validate().is_ok());
    }
}
