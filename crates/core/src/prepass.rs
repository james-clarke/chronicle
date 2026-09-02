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
    let mut placed = Vec::new();
    for run in runs.iter().filter(|r| r.ms >= MIN_RUN_MS) {
        let hints = storage::correction_hints(&tx, &run_text(run))?;
        let ejected: Vec<&Correction> = hints.iter().filter(|c| c.kind == "eject").collect();
        let allowed = |t: &OpenTask| {
            !ejected
                .iter()
                .any(|c| c.old_label.eq_ignore_ascii_case(&t.label) && c.old_project == t.project)
        };
        let mut hit = None;
        if let Some(re) = &ticket_re {
            let anchored = anchor_tasks(&[(0, run.start_ts, run.end_ts)], &prior, &vcs, re);
            if let Some((_, key)) = anchored.first()
                && let Some(t) = storage::open_task_by_ref(&tx, key)?
                && allowed(&t)
            {
                hit = Some((t.id, format!("branch {key}")));
            }
        }
        if hit.is_none() {
            let repos = storage::repos_active_in(&tx, run.start_ts, run.end_ts)?;
            let mut best: Option<(i64, i64, &str)> = None;
            for repo in &repos {
                for t in open.iter().filter(|t| {
                    t.project
                        .as_deref()
                        .is_some_and(|p| p.eq_ignore_ascii_case(repo))
                        && allowed(t)
                }) {
                    let last = storage::task_last_end(&tx, t.id)?.unwrap_or(0);
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
