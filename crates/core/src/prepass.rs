//! Deterministic pre-pass (m24): before the batch derive gets to look at the
//! live tail, place each fresh unassigned run under a task using the rules
//! that already exist elsewhere in the pipeline, in order: the branch active
//! during the run names a ticket key ([`crate::anchor`]), a repo seen during
//! the run names a project (the m22 repo-aware rule), the run's app/title
//! mix matches a past correction (m23's `suggest_correction`). A hit is a
//! provisional interval (`source = 'prepass'`, confidence 0.5) that every
//! surface shows immediately and the next derive replaces. No model.
//!
//! An 'eject' correction is a hard negative here: a task the user pulled
//! similar work out of is skipped by every rule, so the pre-pass cannot put
//! the same block back on the next tick.

use std::collections::HashMap;

use jiff::Timestamp;
use regex::Regex;
use rusqlite::Connection;

use crate::anchor::anchor_tasks;
use crate::config::Config;
use crate::storage::{self, Placement, StorageError, UnassignedRun};
use crate::types::{Correction, OpenTask, ts_to_ms};

/// Unassigned focus closer than this folds into one run (the triage view's
/// threshold).
pub const RUN_GAP_MS: i64 = 5 * 60_000;
/// Runs shorter than this are left to settle (they are re-read next tick).
const MIN_RUN_MS: i64 = 60_000;
/// Never look further back than this when no batch has derived yet.
const MAX_WINDOW_MS: i64 = 12 * 3_600_000;
/// Largest app/title lines that form a run's correction query.
const MAX_LINES: usize = 4;
/// Open tasks considered (declared first, then most recently active).
const OPEN_CAP: usize = 64;
/// Title-key rule: the dominant key needs this much screen time in the run.
const KEY_MIN_MS: i64 = 2 * 60_000;
/// Repo rule: a candidate task must have an interval ending within this of
/// the run's start (or be declared today) — the recency gate that stops one
/// stale declared task from magnetizing every stretch in its repo.
const REPO_RECENT_MS: i64 = 2 * 3_600_000;

/// One pre-pass over `[end of the newest derived batch, now)`: last tick's
/// provisional rows are dropped, the window's unassigned runs re-read, and
/// every run a rule places gets a fresh provisional interval — all in one
/// transaction. User intervals are never touched. Returns what was placed.
pub fn run(
    conn: &mut Connection,
    config: &Config,
    now: Timestamp,
) -> Result<Vec<Placement>, StorageError> {
    let hi = ts_to_ms(now);
    let lo = storage::latest_done_batch_end(conn)?
        .unwrap_or(0)
        .max(hi - MAX_WINDOW_MS);
    let tx = conn.transaction()?;
    storage::clear_prepass(&tx, lo, hi)?;
    let runs = storage::unassigned_runs(&tx, lo, hi, RUN_GAP_MS)?;
    let open = storage::open_tasks(&tx, OPEN_CAP)?;
    let ticket_re = Regex::new(&config.ticket_regex).ok();
    let prior = storage::branch_state_before(&tx, lo)?;
    let vcs = storage::vcs_in_range(&tx, lo, hi)?;
    let distractions = crate::evidence::compile_patterns(&config.distraction_patterns);
    let today_start = now
        .to_zoned(jiff::tz::TimeZone::system())
        .start_of_day()
        .map(|z| z.timestamp().as_millisecond())
        .unwrap_or(hi);
    // Runs actually eligible for placement: long enough to settle, not a
    // distraction stretch. Filtered once so the branch anchor batch below
    // (indexed by position in this list) lines up with the run loop.
    let candidates: Vec<&UnassignedRun> = runs
        .iter()
        .filter(|r| r.ms >= MIN_RUN_MS)
        .filter(|r| {
            !r.lines
                .first()
                .is_some_and(|l| crate::evidence::is_distraction(&l.0, &l.1, &distractions))
        })
        .collect();
    // Branch-anchor hits for every candidate run in one pass: `anchor_tasks`
    // aggregates independently per task id, so indexing runs as task ids
    // batches the whole tick's checkout timeline instead of rebuilding it
    // once per run.
    let anchors: HashMap<i64, String> = ticket_re
        .as_ref()
        .map(|re| {
            let intervals: Vec<(i64, i64, i64)> = candidates
                .iter()
                .enumerate()
                .map(|(i, r)| (i as i64, r.start_ts, r.end_ts))
                .collect();
            anchor_tasks(&intervals, &prior, &vcs, re)
                .into_iter()
                .collect()
        })
        .unwrap_or_default();
    // Every candidate run's repo signal in one query: it tells the ref rules
    // which of two open tasks sharing a ticket key the work belongs to, and
    // the repo rule below reuses it instead of querying per run.
    let ranges: Vec<(i64, i64)> = candidates.iter().map(|r| (r.start_ts, r.end_ts)).collect();
    let run_repos = storage::repos_active_in_many(&tx, &ranges)?;
    // Repo rule's recency inputs, batched once for the whole open-task list;
    // a run placed onto a task this tick updates its own last-end so a later
    // run in the same tick still sees it (the rule's within-tick
    // reinforcement — the same effect the per-pair queries had).
    let ids: Vec<i64> = open.iter().map(|t| t.id).collect();
    let mut recency = storage::task_recency(&tx, &ids)?;
    let mut placed = Vec::new();
    for (i, run) in candidates.iter().enumerate() {
        let hints = storage::correction_hints(&tx, &run_text(run))?;
        let ejected: Vec<&Correction> = hints.iter().filter(|c| c.kind == "eject").collect();
        let allowed = |t: &OpenTask| {
            !ejected
                .iter()
                .any(|c| c.old_label.eq_ignore_ascii_case(&t.label) && c.old_project == t.project)
        };
        let mut repos = run_repos.get(i).cloned().unwrap_or_default();
        // Spans feed every rule: cwd repos disambiguate the branch anchor
        // too, so they are read before it.
        let spans = storage::spans_in_range(&tx, run.start_ts, run.end_ts)?;
        for (repo, _) in crate::evidence::cwd_repos(&spans, run.start_ts, run.end_ts) {
            if !repos.iter().any(|r| r.eq_ignore_ascii_case(&repo)) {
                repos.push(repo);
            }
        }
        let mut hit = None;
        if let Some(key) = anchors.get(&(i as i64))
            && let Some(t) = storage::open_task_by_ref(&tx, key, &repos)?
            && allowed(&t)
        {
            hit = Some((t.id, format!("branch {key}")));
        }
        // A ticket key on screen (Jira page title, PR URL) for most of the
        // run names the task as firmly as a branch does.
        if hit.is_none()
            && let Some(re) = &ticket_re
        {
            let keys = crate::evidence::keys_in_spans(&spans, re, run.start_ts, run.end_ts);
            let total: i64 = keys.iter().map(|k| k.ms).sum();
            if let Some(top) = keys.first()
                && top.ms >= KEY_MIN_MS
                && top.ms * 2 >= total
                && let Some(t) = storage::open_task_by_ref(&tx, &top.key, &repos)?
                && allowed(&t)
            {
                hit = Some((t.id, format!("title {}", top.key)));
            }
        }
        if hit.is_none() {
            let mut best: Option<(i64, i64, &str)> = None;
            for repo in &repos {
                for t in open.iter().filter(|t| {
                    t.project
                        .as_deref()
                        .is_some_and(|p| p.eq_ignore_ascii_case(repo))
                        && allowed(t)
                }) {
                    let (last_end, created_ts) = recency.get(&t.id).copied().unwrap_or((None, 0));
                    let last = last_end.unwrap_or(0);
                    let recent = last >= run.start_ts - REPO_RECENT_MS
                        || (t.declared && created_ts >= today_start);
                    if !recent {
                        continue;
                    }
                    if best.is_none_or(|b| last > b.1) {
                        best = Some((t.id, last, repo));
                    }
                }
            }
            if let Some((id, _, repo)) = best {
                hit = Some((id, format!("repo {repo}")));
            }
        }
        if hit.is_none()
            && let Some(c) = hints.iter().find(|c| {
                c.kind != "eject"
                    && !ejected.iter().any(|e| {
                        e.old_label.eq_ignore_ascii_case(&c.new_label)
                            && e.old_project == c.new_project
                    })
            })
            && let Some(t) = open
                .iter()
                .find(|t| t.label.eq_ignore_ascii_case(&c.new_label) && allowed(t))
        {
            hit = Some((t.id, "past correction".to_owned()));
        }
        if let Some((task_id, reason)) = hit {
            let p = Placement {
                task_id,
                start_ts: run.start_ts,
                end_ts: run.end_ts,
                reason,
            };
            storage::insert_prepass(&tx, &p)?;
            // Keep the recency map current: a later run's repo rule must see
            // this tick's own placements, same as the per-pair queries did.
            let e = recency.entry(task_id).or_insert((None, 0));
            e.0 = Some(e.0.map_or(run.end_ts, |v| v.max(run.end_ts)));
            placed.push(p);
        }
    }
    tx.commit()?;
    Ok(placed)
}

/// The run's largest app/title lines as one correction query.
pub fn run_text(run: &UnassignedRun) -> String {
    run.lines
        .iter()
        .take(MAX_LINES)
        .map(|(app, title, _)| format!("{app} {title}"))
        .collect::<Vec<_>>()
        .join(" ")
}
