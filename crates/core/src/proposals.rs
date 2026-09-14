//! Proposed tasks (m24): unassigned runs the pre-pass could not place are
//! clustered — two runs belong together when they share a distinctive title
//! token (one rare across the day's titles) or a repo active during both —
//! and every cluster with enough focus becomes a `proposals` row the feed
//! shows as a card. A `suggest_task` job names it from the cluster's own
//! spans; accept declares the task and claims the runs, dismiss parks the
//! cluster for the day (no correction: "not a task" is not a teaching
//! signal about any task).
//!
//! Since m44 chunk 2 the segmenter's new clusters come here too, as
//! `segment` proposals: their time sits on the project's other work and
//! confirming one makes the task and moves the stretches onto it.

use std::collections::HashMap;

use jiff::Timestamp;
use rusqlite::{Connection, params};

use crate::prepass::RUN_GAP_MS;
use crate::storage::{self, StorageError, UnassignedRun};
use crate::types::{SuggestedTask, ts_to_ms};

/// A cluster needs this much focus before it is proposed.
pub const MIN_CLUSTER_MS: i64 = 10 * 60_000;
/// How far back clusters are rebuilt each tick.
const WINDOW_MS: i64 = 12 * 3_600_000;
/// Distinct titles needed before frequency prunes tokens.
const IDF_MIN_TITLES: usize = 4;
/// Shortest token that can link two runs.
const MIN_TOKEN_CHARS: usize = 4;
/// App/title lines kept per cluster.
const MAX_LINES: usize = 4;
/// Tokens that name browsers, sites and window furniture rather than work.
const STOP: &[&str] = &[
    "mozilla", "firefox", "google", "chrome", "chromium", "https", "http", "window", "untitled",
    "private", "browsing", "tab", "tabs", "page", "pages", "home", "file", "edit", "view",
];

/// One cluster of unassigned runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    /// `(start_ts, end_ts)` of each run, oldest first.
    pub runs: Vec<(i64, i64)>,
    pub start_ts: i64,
    pub end_ts: i64,
    /// Focus ms across the runs.
    pub ms: i64,
    /// The repo most of the cluster's runs saw, if any.
    pub project: Option<String>,
    /// `(app, title, ms)`, largest first, at most [`MAX_LINES`].
    pub lines: Vec<(String, String, i64)>,
}

/// A proposal as the feed shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Proposal {
    pub id: i64,
    /// `runs` (unassigned runs the pre-pass could not place) or `segment`
    /// (a cluster the segmenter would have minted a task for).
    pub source: String,
    pub start_ts: i64,
    pub end_ts: i64,
    pub ms: i64,
    pub runs: Vec<(i64, i64)>,
    pub project: Option<String>,
    /// None until the naming job lands (or when it failed).
    pub label: Option<String>,
    pub description: Option<String>,
    /// A naming job is queued or running.
    pub naming: bool,
    /// `(app, title, ms)` of the cluster, largest first.
    pub lines: Vec<(String, String, i64)>,
}

/// Lower-case tokens of a title worth linking on: at least
/// [`MIN_TOKEN_CHARS`] long, containing a letter, not the app's own name,
/// not window furniture.
pub fn tokens(app: &str, title: &str) -> Vec<String> {
    let app = app.to_lowercase();
    let mut out: Vec<String> = Vec::new();
    for word in title.split(|c: char| !c.is_alphanumeric() && c != '_') {
        if word.chars().count() < MIN_TOKEN_CHARS || !word.chars().any(char::is_alphabetic) {
            continue;
        }
        let word = word.to_lowercase();
        if word == app || STOP.contains(&word.as_str()) || out.contains(&word) {
            continue;
        }
        out.push(word);
    }
    out
}

/// Cluster runs: union by shared distinctive token or shared repo.
/// `titles` are the day's distinct `(app, title)` pairs (token frequency
/// is measured against them: a token in more than half is not distinctive);
/// `repos[i]` are the repos active during `runs[i]`.
pub fn cluster_runs(
    runs: &[UnassignedRun],
    titles: &[(String, String)],
    repos: &[Vec<String>],
) -> Vec<Cluster> {
    let mut df: HashMap<String, usize> = HashMap::new();
    for (app, title) in titles {
        for t in tokens(app, title) {
            *df.entry(t).or_default() += 1;
        }
    }
    let distinctive =
        |t: &str| titles.len() < IDF_MIN_TITLES || df.get(t).is_none_or(|n| n * 2 <= titles.len());
    let run_tokens: Vec<Vec<String>> = runs
        .iter()
        .map(|r| {
            let mut ts: Vec<String> = Vec::new();
            for (app, title, _) in &r.lines {
                for t in tokens(app, title) {
                    if distinctive(&t) && !ts.contains(&t) {
                        ts.push(t);
                    }
                }
            }
            ts
        })
        .collect();
    // Union-find over run indices.
    let mut parent: Vec<usize> = (0..runs.len()).collect();
    fn find(parent: &mut [usize], i: usize) -> usize {
        let mut i = i;
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    for i in 0..runs.len() {
        for j in (i + 1)..runs.len() {
            let linked = run_tokens[i].iter().any(|t| run_tokens[j].contains(t))
                || repos
                    .get(i)
                    .zip(repos.get(j))
                    .is_some_and(|(a, b)| a.iter().any(|r| b.contains(r)));
            if linked {
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                if a != b {
                    parent[b] = a;
                }
            }
        }
    }
    let mut groups: Vec<(usize, Vec<usize>)> = Vec::new();
    for i in 0..runs.len() {
        let root = find(&mut parent, i);
        match groups.iter_mut().find(|g| g.0 == root) {
            Some(g) => g.1.push(i),
            None => groups.push((root, vec![i])),
        }
    }
    let mut clusters: Vec<Cluster> = groups
        .into_iter()
        .map(|(_, members)| {
            let mut lines: Vec<(String, String, i64)> = Vec::new();
            let mut repo_votes: HashMap<&str, usize> = HashMap::new();
            for &i in &members {
                for (app, title, ms) in &runs[i].lines {
                    match lines.iter_mut().find(|l| &l.0 == app && &l.1 == title) {
                        Some(l) => l.2 += ms,
                        None => lines.push((app.clone(), title.clone(), *ms)),
                    }
                }
                if let Some(rs) = repos.get(i) {
                    for r in rs {
                        *repo_votes.entry(r.as_str()).or_default() += 1;
                    }
                }
            }
            lines.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
            lines.truncate(MAX_LINES);
            let mut votes: Vec<(&str, usize)> = repo_votes.into_iter().collect();
            votes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(b.0)));
            let mut run_spans: Vec<(i64, i64)> = members
                .iter()
                .map(|&i| (runs[i].start_ts, runs[i].end_ts))
                .collect();
            run_spans.sort();
            Cluster {
                start_ts: run_spans[0].0,
                end_ts: run_spans.iter().map(|r| r.1).max().unwrap_or(0),
                ms: members.iter().map(|&i| runs[i].ms).sum(),
                project: votes.first().map(|(r, _)| (*r).to_owned()),
                lines,
                runs: run_spans,
            }
        })
        .collect();
    clusters.sort_by_key(|c| c.start_ts);
    clusters
}

/// One tick: rebuild the clusters over the last [`WINDOW_MS`], rewrite the
/// open proposals to match (a cluster is keyed by its earliest run: it grows
/// in place, a head that got claimed drops the row), queue a naming job for
/// each new proposal, and copy finished names in. Accepted and dismissed
/// rows are left alone. Returns the open proposals' starts.
pub fn refresh(
    conn: &mut Connection,
    now: Timestamp,
    distractions: &[regex::Regex],
) -> Result<Vec<i64>, StorageError> {
    let hi = ts_to_ms(now);
    let lo = hi - WINDOW_MS;
    // A distraction stretch (video, social) or the app's own window never
    // seeds a proposal.
    let runs: Vec<_> = storage::unassigned_runs(conn, lo, hi, RUN_GAP_MS)?
        .into_iter()
        .filter(|r| {
            !r.lines
                .first()
                .is_some_and(|l| crate::evidence::is_furniture(&l.0, &l.1, distractions))
        })
        .collect();
    let titles = day_titles(conn, lo, hi)?;
    let ranges: Vec<(i64, i64)> = runs.iter().map(|r| (r.start_ts, r.end_ts)).collect();
    let repos = storage::repos_active_in_many(conn, &ranges)?;
    let clusters = cluster_runs(&runs, &titles, &repos);
    let tx = conn.transaction()?;
    let heads: Vec<i64> = clusters
        .iter()
        .filter(|c| c.ms >= MIN_CLUSTER_MS)
        .map(|c| c.start_ts)
        .collect();
    {
        let mut stale =
            tx.prepare("SELECT start_ts FROM proposals WHERE status='open' AND source='runs'")?;
        let open: Vec<i64> = stale
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        for s in open.iter().filter(|s| !heads.contains(s)) {
            tx.execute(
                "DELETE FROM proposals WHERE start_ts=?1 AND source='runs'",
                [s],
            )?;
        }
    }
    for c in clusters.iter().filter(|c| c.ms >= MIN_CLUSTER_MS) {
        let runs_json = serde_json::to_string(&c.runs).unwrap_or_else(|_| "[]".into());
        tx.execute(
            "INSERT INTO proposals (start_ts, end_ts, ms, runs, project, ts)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(source, start_ts) DO UPDATE SET
                 end_ts=excluded.end_ts, ms=excluded.ms, runs=excluded.runs,
                 project=COALESCE(proposals.project, excluded.project)
             WHERE proposals.status='open'",
            params![c.start_ts, c.end_ts, c.ms, runs_json, c.project, hi],
        )?;
    }
    // Name new proposals from their own spans; pick up finished names.
    let pending: Vec<(i64, i64, i64, Option<i64>)> = {
        let mut stmt = tx.prepare(
            "SELECT id, start_ts, end_ts, job_id FROM proposals
             WHERE status='open' AND label IS NULL",
        )?;
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?
    };
    for (id, start_ts, end_ts, job_id) in pending {
        match job_id {
            None => {
                let payload = format!("{{\"lo\":{start_ts},\"hi\":{end_ts}}}");
                let job = storage::enqueue_ai_job(&tx, now, "suggest_task", 0, &payload)?;
                tx.execute(
                    "UPDATE proposals SET job_id=?1 WHERE id=?2",
                    params![job, id],
                )?;
            }
            Some(job) => {
                if let Some((status, result)) = storage::ai_job_status(&tx, job)?
                    && status == "done"
                    && let Some(s) = result
                        .as_deref()
                        .and_then(|r| serde_json::from_str::<SuggestedTask>(r).ok())
                {
                    tx.execute(
                        "UPDATE proposals SET label=?1, description=?2,
                             project=COALESCE(project, ?3) WHERE id=?4",
                        params![s.label, s.description, s.project, id],
                    )?;
                }
            }
        }
    }
    tx.commit()?;
    Ok(heads)
}

/// Distinct focus `(app, title)` pairs starting inside `[lo, hi)`.
fn day_titles(conn: &Connection, lo: i64, hi: i64) -> Result<Vec<(String, String)>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT app, title FROM spans
         WHERE kind='focus' AND start_ts >= ?1 AND start_ts < ?2",
    )?;
    let rows = stmt.query_map([lo, hi], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Open proposals starting inside `[lo, hi)`, newest first, with their
/// current app/title mix.
pub fn open_proposals(conn: &Connection, lo: i64, hi: i64) -> Result<Vec<Proposal>, StorageError> {
    let mut stmt = conn.prepare(
        "SELECT p.id, p.start_ts, p.end_ts, p.ms, p.runs, p.project, p.label, p.description,
                COALESCE(j.status IN ('pending', 'running'), 0), p.source
         FROM proposals p LEFT JOIN ai_jobs j ON j.id = p.job_id
         WHERE p.status='open' AND p.start_ts >= ?1 AND p.start_ts < ?2
         ORDER BY p.start_ts DESC",
    )?;
    let mut out = Vec::new();
    let mut rows = stmt.query([lo, hi])?;
    let mut ls = conn.prepare_cached(
        "SELECT app, title, SUM(MIN(end_ts, ?2) - MAX(start_ts, ?1)) FROM spans
         WHERE kind='focus' AND start_ts < ?2 AND end_ts > ?1 GROUP BY app, title",
    )?;
    while let Some(r) = rows.next()? {
        let runs_json: String = r.get(4)?;
        let runs: Vec<(i64, i64)> = serde_json::from_str(&runs_json).unwrap_or_default();
        let mut lines: Vec<(String, String, i64)> = Vec::new();
        for &(s, e) in &runs {
            let mut sp = ls.query([s, e])?;
            while let Some(l) = sp.next()? {
                let (app, title, ms): (String, String, i64) = (l.get(0)?, l.get(1)?, l.get(2)?);
                match lines.iter_mut().find(|x| x.0 == app && x.1 == title) {
                    Some(x) => x.2 += ms,
                    None => lines.push((app, title, ms)),
                }
            }
        }
        lines.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        lines.truncate(MAX_LINES);
        out.push(Proposal {
            id: r.get(0)?,
            start_ts: r.get(1)?,
            end_ts: r.get(2)?,
            ms: r.get(3)?,
            runs,
            project: r.get(5)?,
            label: r.get(6)?,
            description: r.get(7)?,
            naming: r.get::<_, i64>(8)? != 0,
            source: r.get(9)?,
            lines,
        });
    }
    Ok(out)
}

/// Accept: `label` (the proposal's, or the user's fallback) becomes a task
/// on the proposal's project that takes the proposal's stretches. A
/// `runs` proposal declares a user task and claims the runs still
/// unassigned; a `segment` proposal makes a derived task, the same as the
/// segmenter used to mint, and moves the stretches off the project's
/// other work onto it as rows the person placed, so a later placement
/// keeps them. Returns `(task_id, ms claimed)`.
pub fn accept(
    conn: &mut Connection,
    ts: Timestamp,
    id: i64,
    label: &str,
) -> Result<(i64, i64), StorageError> {
    let (runs_json, project, description, source, start_ts): (
        String,
        Option<String>,
        Option<String>,
        String,
        i64,
    ) = conn.query_row(
        "SELECT runs, project, description, source, start_ts FROM proposals
         WHERE id=?1 AND status='open'",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )?;
    let runs: Vec<(i64, i64)> = serde_json::from_str(&runs_json).unwrap_or_default();
    let task_id = if source == "segment" {
        conn.execute(
            "INSERT INTO tasks (label, project, status, source, created_ts)
             VALUES (?1, ?2, 'open', 'derived', ?3)",
            params![label, project, start_ts],
        )?;
        conn.last_insert_rowid()
    } else {
        storage::insert_user_task(conn, ts, label, project.as_deref())?
    };
    if description.is_some() {
        storage::set_task_description(conn, task_id, description.as_deref())?;
    }
    let claimed = claim(conn, ts, &runs, &source, task_id)?;
    conn.execute(
        "UPDATE proposals SET status='accepted', task_id=?1, label=?2 WHERE id=?3",
        params![task_id, label, id],
    )?;
    Ok((task_id, claimed))
}

/// Merge: the proposal's stretches go to an existing task instead of a
/// new one. Returns the ms claimed.
pub fn merge(
    conn: &mut Connection,
    ts: Timestamp,
    id: i64,
    to_task: i64,
) -> Result<i64, StorageError> {
    let (runs_json, source): (String, String) = conn.query_row(
        "SELECT runs, source FROM proposals WHERE id=?1 AND status='open'",
        [id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let runs: Vec<(i64, i64)> = serde_json::from_str(&runs_json).unwrap_or_default();
    let claimed = claim(conn, ts, &runs, &source, to_task)?;
    conn.execute(
        "UPDATE proposals SET status='accepted', task_id=?1 WHERE id=?2",
        params![to_task, id],
    )?;
    Ok(claimed)
}

/// The proposal's stretches onto `task_id`, by source.
fn claim(
    conn: &mut Connection,
    ts: Timestamp,
    runs: &[(i64, i64)],
    source: &str,
    task_id: i64,
) -> Result<i64, StorageError> {
    let mut claimed = 0;
    if source == "segment" {
        for &(s, e) in runs {
            claimed += storage::claim_segment_range(conn, ts, s, e, task_id)?;
        }
    } else {
        for &(s, e) in runs {
            claimed += storage::assign_unassigned(conn, ts, s, e, task_id)?;
        }
    }
    Ok(claimed)
}

/// "Not a task": the cluster stays unassigned and is not proposed again
/// today (its head keeps the row).
pub fn dismiss(conn: &Connection, id: i64) -> Result<(), StorageError> {
    conn.execute(
        "UPDATE proposals SET status='dismissed' WHERE id=?1 AND status='open'",
        [id],
    )?;
    Ok(())
}
