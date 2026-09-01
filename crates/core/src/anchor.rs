//! Deterministic task anchoring (m15): the branch active for the majority of
//! a task's interval time names the task's `external_ref` (ticket key
//! extracted from the branch name). No LLM involved; anchoring never
//! overwrites an existing ref (enforced in storage).

use std::collections::HashMap;

use regex::Regex;

use crate::types::{VcsEvent, VcsKind};

/// `intervals` are `(task_id, start_ms, end_ms)` as returned by
/// [`crate::storage::store_derivation`]. `prior` is the latest checkout per
/// repo before the window ([`crate::storage::branch_state_before`]);
/// `in_window` the vcs events inside it. Returns `(task_id, ticket_key)` for
/// tasks where one key's branch time covers > 50% of the task's interval
/// time.
pub fn anchor_tasks(
    intervals: &[(i64, i64, i64)],
    prior: &[VcsEvent],
    in_window: &[VcsEvent],
    ticket_re: &Regex,
) -> Vec<(i64, String)> {
    // Per-repo checkout timeline: a branch is active from its checkout until
    // the same repo's next checkout. Prior state is active from the start.
    let mut repos: HashMap<&str, Vec<(i64, &str)>> = HashMap::new();
    for e in prior.iter().filter(|e| e.kind == VcsKind::Checkout) {
        repos
            .entry(e.repo.as_str())
            .or_default()
            .push((i64::MIN, e.branch.as_str()));
    }
    for e in in_window.iter().filter(|e| e.kind == VcsKind::Checkout) {
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
    let mut out: Vec<(i64, String)> = best
        .into_iter()
        .filter(|(task, (ov, _))| ov * 2 > total_ms.get(task).copied().unwrap_or(0))
        .map(|(task, (_, key))| (task, key.to_owned()))
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ms_to_ts;

    fn checkout(ts_ms: i64, repo: &str, branch: &str) -> VcsEvent {
        VcsEvent {
            ts: ms_to_ts(ts_ms),
            repo: repo.into(),
            branch: branch.into(),
            kind: VcsKind::Checkout,
            commit_id: None,
            summary: None,
        }
    }

    fn re() -> Regex {
        Regex::new("[A-Z][A-Z0-9]+-[0-9]+").unwrap()
    }

    #[test]
    fn ticketed_branch_covering_window_anchors() {
        let prior = [checkout(0, "app", "ABC-123-sending-plans")];
        let intervals = [(1, 1_000, 61_000)];
        let got = anchor_tasks(&intervals, &prior, &[], &re());
        assert_eq!(got, vec![(1, "ABC-123".to_owned())]);
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
        let prior = [checkout(0, "app", "ABC-1-x")];
        let inwin = [checkout(60_000, "app", "DEF-2-y")];
        let intervals = [(1, 0, 60_000), (2, 60_000, 120_000)];
        let got = anchor_tasks(&intervals, &prior, &inwin, &re());
        assert_eq!(got, vec![(1, "ABC-1".to_owned()), (2, "DEF-2".to_owned())]);
    }
}
