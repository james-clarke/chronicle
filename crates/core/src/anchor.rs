//! Deterministic task anchoring (m15): the branch active for the majority of
//! a task's interval time names the task's `external_ref` (ticket key
//! extracted from the branch name), gated on actual in-window vcs activity
//! on that branch near the task's intervals. No LLM involved; anchoring
//! never overwrites an existing ref (enforced in storage).

use std::collections::HashMap;

use regex::Regex;

use crate::types::{ActivityEvent, ActivityKind};

/// `intervals` are `(task_id, start_ms, end_ms)` as returned by
/// [`crate::storage::store_derivation`]. `prior` is the latest checkout per
/// repo before the window ([`crate::storage::branch_state_before`]);
/// `in_window` the vcs events inside it. Returns `(task_id, ticket_key)` for
/// tasks where one key's branch time covers > 50% of the task's interval
/// time AND an in-window vcs event on that branch lands inside the task's
/// intervals (parked branches never anchor by presence alone).
pub fn anchor_tasks(
    intervals: &[(i64, i64, i64)],
    prior: &[ActivityEvent],
    in_window: &[ActivityEvent],
    ticket_re: &Regex,
) -> Vec<(i64, String)> {
    // Per-repo checkout timeline: a branch is active from its checkout until
    // the same repo's next checkout. Prior state is active from the start.
    let mut repos: HashMap<&str, Vec<(i64, &str)>> = HashMap::new();
    for e in prior.iter().filter(|e| e.kind == ActivityKind::Checkout) {
        repos
            .entry(e.repo.as_str())
            .or_default()
            .push((i64::MIN, e.branch.as_str()));
    }
    for e in in_window
        .iter()
        .filter(|e| e.kind == ActivityKind::Checkout)
    {
        repos
            .entry(e.repo.as_str())
            .or_default()
            .push((e.ts.as_millisecond(), e.branch.as_str()));
    }
    let mut segs: Vec<(i64, i64, &str)> = Vec::new();
    for list in repos.values_mut() {
        list.sort_by_key(|(ts, _)| *ts);
        for i in 0..list.len() {
            let (from, branch) = list[i];
            let to = list.get(i + 1).map_or(i64::MAX, |(ts, _)| *ts);
            if from < to
                && let Some(m) = ticket_re.find(branch)
            {
                segs.push((from, to, m.as_str()));
            }
        }
    }

    let mut total_ms: HashMap<i64, i64> = HashMap::new();
    let mut key_ms: HashMap<(i64, &str), i64> = HashMap::new();
    for &(task, lo, hi) in intervals {
        *total_ms.entry(task).or_default() += hi - lo;
        for &(s, e, key) in &segs {
            let ov = hi.min(e).saturating_sub(lo.max(s));
            if ov > 0 {
                *key_ms.entry((task, key)).or_default() += ov;
            }
        }
    }

    let mut best: HashMap<i64, (i64, &str)> = HashMap::new();
    for ((task, key), ov) in key_ms {
        let b = best.entry(task).or_insert((0, ""));
        if ov > b.0 || (ov == b.0 && key < b.1) {
            *b = (ov, key);
        }
    }
    // Activity gate: a parked branch must not anchor by mere presence (a
    // ticketed branch left checked out in one repo otherwise anchors every
    // task all day, poisoning context fetches and journals). Some in-window
    // vcs event on the key's branch — checkout or commit — has to land
    // inside the task's own intervals; the symmetric grace covers
    // switch-then-focus (checkout precedes the first focus evidence) and
    // commit-after-switch-away (commit trails the last).
    const ACTIVITY_GRACE_MS: i64 = 10 * 60 * 1000;
    let mut active: std::collections::HashSet<(i64, &str)> = std::collections::HashSet::new();
    for e in in_window {
        let Some(m) = ticket_re.find(&e.branch) else {
            continue;
        };
        let t = e.ts.as_millisecond();
        for &(task, lo, hi) in intervals {
            if t >= lo - ACTIVITY_GRACE_MS && t < hi + ACTIVITY_GRACE_MS {
                active.insert((task, m.as_str()));
            }
        }
    }

    let mut out: Vec<(i64, String)> = best
        .into_iter()
        .filter(|(task, (ov, _))| ov * 2 > total_ms.get(task).copied().unwrap_or(0))
        .filter(|&(task, (_, key))| active.contains(&(task, key)))
        .map(|(task, (_, key))| (task, key.to_owned()))
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ms_to_ts;

    fn checkout(ts_ms: i64, repo: &str, branch: &str) -> ActivityEvent {
        ActivityEvent {
            ts: ms_to_ts(ts_ms),
            repo: repo.into(),
            branch: branch.into(),
            kind: ActivityKind::Checkout,
            ext_id: None,
            end_ts: None,
            summary: None,
        }
    }

    fn commit(ts_ms: i64, repo: &str, branch: &str) -> ActivityEvent {
        ActivityEvent {
            ts: ms_to_ts(ts_ms),
            repo: repo.into(),
            branch: branch.into(),
            kind: ActivityKind::Commit,
            ext_id: Some("deadbeef".into()),
            end_ts: None,
            summary: Some("x".into()),
        }
    }

    fn re() -> Regex {
        Regex::new("[A-Z][A-Z0-9]+-[0-9]+").unwrap()
    }

    #[test]
    fn parked_prior_branch_alone_never_anchors() {
        let prior = [checkout(0, "app", "ABC-123-sending-plans")];
        let intervals = [(1, 1_000, 61_000)];
        let got = anchor_tasks(&intervals, &prior, &[], &re());
        assert!(got.is_empty());
    }

    #[test]
    fn commit_inside_interval_anchors_parked_branch() {
        let prior = [checkout(0, "app", "ABC-123-sending-plans")];
        let inwin = [commit(30_000, "app", "ABC-123-sending-plans")];
        let intervals = [(1, 1_000, 61_000)];
        let got = anchor_tasks(&intervals, &prior, &inwin, &re());
        assert_eq!(got, vec![(1, "ABC-123".to_owned())]);
    }

    #[test]
    fn checkout_shortly_before_interval_anchors() {
        // Switch-then-focus: checkout 5 min before the task's first
        // interval still passes the activity grace.
        let inwin = [checkout(0, "app", "ABC-123-x")];
        let intervals = [(1, 300_000, 3_900_000)];
        let got = anchor_tasks(&intervals, &[], &inwin, &re());
        assert_eq!(got, vec![(1, "ABC-123".to_owned())]);
    }

    #[test]
    fn event_in_other_tasks_interval_does_not_anchor() {
        // Activity in task 2's window (beyond task 1's grace) must not
        // anchor task 1, even though the branch covers task 1's whole
        // interval too.
        let prior = [checkout(0, "app", "ABC-123-x")];
        let inwin = [commit(720_000, "app", "ABC-123-x")];
        let intervals = [(1, 0, 60_000), (2, 700_000, 760_000)];
        let got = anchor_tasks(&intervals, &prior, &inwin, &re());
        assert_eq!(got, vec![(2, "ABC-123".to_owned())]);
    }

    #[test]
    fn unticketed_branch_anchors_nothing() {
        let prior = [checkout(0, "app", "main")];
        let got = anchor_tasks(&[(1, 1_000, 61_000)], &prior, &[], &re());
        assert!(got.is_empty());
    }

    #[test]
    fn majority_branch_wins_after_mid_window_switch() {
        let prior = [checkout(0, "app", "ABC-1-x")];
        // Switch at 20s of a 0–60s interval: ABC-2 holds 40 of 60s.
        let inwin = [checkout(20_000, "app", "ABC-2-y")];
        let got = anchor_tasks(&[(1, 0, 60_000)], &prior, &inwin, &re());
        assert_eq!(got, vec![(1, "ABC-2".to_owned())]);
    }

    #[test]
    fn exact_half_is_not_a_majority() {
        let prior = [checkout(0, "app", "ABC-1-x")];
        let inwin = [checkout(30_000, "app", "main")];
        let got = anchor_tasks(&[(1, 0, 60_000)], &prior, &inwin, &re());
        assert!(got.is_empty());
    }

    #[test]
    fn tasks_anchor_independently() {
        // Task 1 sat on parked ABC-1 (no in-window activity → unanchored);
        // task 2 starts at the DEF-2 checkout and anchors to it.
        let prior = [checkout(0, "app", "ABC-1-x")];
        let inwin = [checkout(60_000, "app", "DEF-2-y")];
        let intervals = [(1, 0, 60_000), (2, 60_000, 120_000)];
        let got = anchor_tasks(&intervals, &prior, &inwin, &re());
        assert_eq!(got, vec![(2, "DEF-2".to_owned())]);
    }
}
