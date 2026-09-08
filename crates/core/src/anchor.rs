//! Deterministic task anchoring (m15): the branch active for the majority of
//! a task's interval time names the task's `external_ref` (ticket key
//! extracted from the branch name), gated on actual in-window activity on
//! that key near the task's intervals — a checkout or commit on the branch,
//! or a PR event whose title carries the key (m22). A task with no branch
//! majority still anchors when exactly one key appears in PR titles inside
//! its intervals (reviewing someone's ACME-123 PR from `main`). A ref
//! comes only from the task's own place (m32 chunk 4): a task with a
//! project takes branches and PRs from that repo alone, so an agent
//! committing in `contoso` cannot name a `chronicle` task; and a PR row is
//! strong only when its branch was checked out, or its repo sat in a
//! shell, within a day of it. No LLM involved; anchoring never overwrites
//! an existing ref (enforced in storage).

use std::collections::HashMap;

use regex::Regex;

use crate::types::{ActivityEvent, ActivityKind};

/// A PR row counts when the person was on it: a checkout or commit on a
/// branch carrying its key in its repo, or a shell / cwd row in its repo,
/// within this long of the PR's update.
const PR_NEARBY_MS: i64 = 24 * 3_600_000;

/// `intervals` are `(task_id, start_ms, end_ms)` as returned by
/// [`crate::storage::store_derivation`]. `prior` is the latest checkout per
/// repo before the window ([`crate::storage::branch_state_before`]);
/// `in_window` the vcs, PR, cwd and shell events inside it. `projects` is
/// each task's declared project, when it has one; a task absent from it
/// takes evidence from every repo. Returns `(task_id, ticket_key)` for
/// tasks where one key's branch time covers > 50% of the task's interval
/// time AND an in-window event on that key (vcs on the branch, or a strong
/// PR titled with it) lands inside the task's intervals (parked branches
/// never anchor by presence alone); failing a branch majority, the single
/// key named by strong PR events inside the intervals.
pub fn anchor_tasks(
    intervals: &[(i64, i64, i64)],
    prior: &[ActivityEvent],
    in_window: &[ActivityEvent],
    ticket_re: &Regex,
    projects: &HashMap<i64, String>,
) -> Vec<(i64, String)> {
    // A task's own place: its project (`a/b` names two repos). No project
    // constrains nothing.
    let own_place = |task: i64, repo: &str| -> bool {
        projects.get(&task).is_none_or(|p| {
            p.split('/')
                .map(str::trim)
                .any(|part| part.eq_ignore_ascii_case(repo))
        })
    };
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
    let mut segs: Vec<(i64, i64, &str, &str)> = Vec::new();
    for (repo, list) in repos.iter_mut() {
        list.sort_by_key(|(ts, _)| *ts);
        for i in 0..list.len() {
            let (from, branch) = list[i];
            let to = list.get(i + 1).map_or(i64::MAX, |(ts, _)| *ts);
            if from < to
                && let Some(m) = ticket_re.find(branch)
            {
                segs.push((from, to, m.as_str(), repo));
            }
        }
    }

    let mut total_ms: HashMap<i64, i64> = HashMap::new();
    let mut key_ms: HashMap<(i64, &str), i64> = HashMap::new();
    for &(task, lo, hi) in intervals {
        *total_ms.entry(task).or_default() += hi - lo;
        for &(s, e, key, repo) in &segs {
            if !own_place(task, repo) {
                continue;
            }
            let ov = hi.min(e).saturating_sub(lo.max(s));
            if ov > 0 {
                *key_ms.entry((task, key)).or_default() += ov;
            }
        }
    }

    // A PR row is strong when the person was on it (m32 chunk 4): its key
    // on a checked-out or committed branch in its repo, or its repo in a
    // shell, within a day. A teammate's PR in a repo never opened is weak
    // and names nothing.
    let pr_strong = |pr: &ActivityEvent, key: &str| -> bool {
        let t = pr.ts.as_millisecond();
        prior.iter().chain(in_window).any(|o| {
            (o.ts.as_millisecond() - t).abs() <= PR_NEARBY_MS
                && o.repo.eq_ignore_ascii_case(&pr.repo)
                && match o.kind {
                    ActivityKind::Checkout | ActivityKind::Commit => {
                        ticket_re.find(&o.branch).is_some_and(|m| m.as_str() == key)
                    }
                    ActivityKind::Cwd | ActivityKind::Shell => true,
                    _ => false,
                }
        })
    };

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
    // Keys PR titles name inside each task's intervals — the fallback
    // anchor for tasks with no branch majority.
    let mut pr_keys: HashMap<i64, std::collections::BTreeSet<&str>> = HashMap::new();
    for e in in_window {
        let key = if e.kind.is_pr() {
            e.summary.as_deref().and_then(|s| ticket_re.find(s))
        } else if e.kind.is_vcs() {
            ticket_re.find(&e.branch)
        } else {
            None
        };
        let Some(m) = key else {
            continue;
        };
        if e.kind.is_pr() && !pr_strong(e, m.as_str()) {
            continue;
        }
        let t = e.ts.as_millisecond();
        for &(task, lo, hi) in intervals {
            if !own_place(task, &e.repo) {
                continue;
            }
            if t >= lo - ACTIVITY_GRACE_MS && t < hi + ACTIVITY_GRACE_MS {
                active.insert((task, m.as_str()));
                if e.kind.is_pr() {
                    pr_keys.entry(task).or_default().insert(m.as_str());
                }
            }
        }
    }

    let mut out: Vec<(i64, String)> = best
        .into_iter()
        .filter(|(task, (ov, _))| ov * 2 > total_ms.get(task).copied().unwrap_or(0))
        .filter(|&(task, (_, key))| active.contains(&(task, key)))
        .map(|(task, (_, key))| (task, key.to_owned()))
        .collect();
    for (task, keys) in pr_keys {
        if keys.len() == 1 && !out.iter().any(|(t, _)| *t == task) {
            out.push((task, (*keys.iter().next().unwrap()).to_owned()));
        }
    }
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
            detail: None,
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
            detail: None,
        }
    }

    fn pr(ts_ms: i64, repo: &str, title: &str) -> ActivityEvent {
        ActivityEvent {
            ts: ms_to_ts(ts_ms),
            repo: repo.into(),
            branch: String::new(),
            kind: ActivityKind::PrReviewed,
            ext_id: Some("url".into()),
            end_ts: None,
            summary: Some(title.into()),
            detail: None,
        }
    }

    fn re() -> Regex {
        Regex::new("[A-Z][A-Z0-9]+-[0-9]+").unwrap()
    }

    #[test]
    fn pr_title_inside_interval_gates_parked_branch() {
        // Pushing PRs on a long-lived ticketed branch is activity on it.
        let prior = [checkout(0, "app", "ABC-123-sending-plans")];
        let inwin = [pr(30_000, "app", "#7 ABC-123 send plans \u{b7} open")];
        let got = anchor_tasks(
            &[(1, 1_000, 61_000)],
            &prior,
            &inwin,
            &re(),
            &HashMap::new(),
        );
        assert_eq!(got, vec![(1, "ABC-123".to_owned())]);
    }

    fn cwd(ts_ms: i64, repo: &str) -> ActivityEvent {
        ActivityEvent {
            ts: ms_to_ts(ts_ms),
            repo: repo.into(),
            branch: String::new(),
            kind: ActivityKind::Cwd,
            ext_id: Some(format!("cwd:1:{repo}")),
            end_ts: Some(ms_to_ts(ts_ms)),
            summary: None,
            detail: None,
        }
    }

    fn projects(pairs: &[(i64, &str)]) -> HashMap<i64, String> {
        pairs.iter().map(|(t, p)| (*t, (*p).to_owned())).collect()
    }

    #[test]
    fn single_pr_key_anchors_without_a_branch() {
        // Reviewing from `main`: no branch majority, one key in PR titles,
        // and a shell in the repo that day makes the PR rows strong.
        let prior = [checkout(0, "app", "main")];
        let inwin = [
            cwd(20_000, "app"),
            pr(30_000, "app", "#7 DEF-4 fix login"),
            pr(40_000, "app", "#8 DEF-4 fix login again"),
        ];
        let got = anchor_tasks(
            &[(1, 1_000, 61_000)],
            &prior,
            &inwin,
            &re(),
            &HashMap::new(),
        );
        assert_eq!(got, vec![(1, "DEF-4".to_owned())]);
    }

    #[test]
    fn a_pr_in_a_repo_never_opened_is_weak() {
        // A teammate's PR updated in a repo with no checkout on its branch
        // and no shell in it that day names nothing; a shell a day later
        // is too far.
        let inwin = [pr(30_000, "app", "#7 DEF-4 fix login")];
        let got = anchor_tasks(&[(1, 1_000, 61_000)], &[], &inwin, &re(), &HashMap::new());
        assert!(got.is_empty());
        let late = [
            pr(30_000, "app", "#7 DEF-4 fix login"),
            cwd(30_000 + PR_NEARBY_MS + 1, "app"),
        ];
        let got = anchor_tasks(&[(1, 1_000, 61_000)], &[], &late, &re(), &HashMap::new());
        assert!(got.is_empty());
        // A checkout of the PR's branch two hours later (past the interval's
        // grace, so no branch majority) makes the PR strong in its own repo
        // only.
        let other = [
            pr(30_000, "app", "#7 DEF-4 fix login"),
            checkout(7_230_000, "lib", "DEF-4-fix"),
        ];
        let got = anchor_tasks(&[(1, 1_000, 61_000)], &[], &other, &re(), &HashMap::new());
        assert!(got.is_empty());
        let own = [
            pr(30_000, "app", "#7 DEF-4 fix login"),
            checkout(7_230_000, "app", "DEF-4-fix"),
        ];
        let got = anchor_tasks(&[(1, 1_000, 61_000)], &[], &own, &re(), &HashMap::new());
        assert_eq!(got, vec![(1, "DEF-4".to_owned())]);
    }

    #[test]
    fn a_task_takes_refs_from_its_own_repo_only() {
        // An agent committing on a ticketed contoso branch while the person
        // works a chronicle task: the branch covers the whole interval and
        // the commit lands inside it, but the task's place is chronicle.
        let prior = [checkout(0, "contoso", "ABC-123-x")];
        let inwin = [commit(30_000, "contoso", "ABC-123-x")];
        let intervals = [(1, 1_000, 61_000)];
        let got = anchor_tasks(
            &intervals,
            &prior,
            &inwin,
            &re(),
            &projects(&[(1, "chronicle")]),
        );
        assert!(got.is_empty());
        // The same task with no project, or with the repo among its
        // projects, anchors as before.
        let got = anchor_tasks(&intervals, &prior, &inwin, &re(), &HashMap::new());
        assert_eq!(got, vec![(1, "ABC-123".to_owned())]);
        let got = anchor_tasks(
            &intervals,
            &prior,
            &inwin,
            &re(),
            &projects(&[(1, "mailer/contoso")]),
        );
        assert_eq!(got, vec![(1, "ABC-123".to_owned())]);
        // A strong PR in a foreign repo is foreign too.
        let inwin = [cwd(20_000, "contoso"), pr(30_000, "contoso", "#7 DEF-4 x")];
        let got = anchor_tasks(
            &intervals,
            &[],
            &inwin,
            &re(),
            &projects(&[(1, "chronicle")]),
        );
        assert!(got.is_empty());
    }

    #[test]
    fn competing_pr_keys_anchor_nothing() {
        let inwin = [
            pr(30_000, "app", "#7 DEF-4 fix login"),
            pr(40_000, "app", "#8 DEF-5 other"),
        ];
        let got = anchor_tasks(&[(1, 1_000, 61_000)], &[], &inwin, &re(), &HashMap::new());
        assert!(got.is_empty());
    }

    #[test]
    fn pr_key_never_overrides_a_branch_majority() {
        let prior = [checkout(0, "app", "ABC-1-x")];
        let inwin = [
            commit(30_000, "app", "ABC-1-x"),
            pr(40_000, "app", "#8 DEF-5 review"),
        ];
        let got = anchor_tasks(
            &[(1, 1_000, 61_000)],
            &prior,
            &inwin,
            &re(),
            &HashMap::new(),
        );
        assert_eq!(got, vec![(1, "ABC-1".to_owned())]);
    }

    #[test]
    fn parked_prior_branch_alone_never_anchors() {
        let prior = [checkout(0, "app", "ABC-123-sending-plans")];
        let intervals = [(1, 1_000, 61_000)];
        let got = anchor_tasks(&intervals, &prior, &[], &re(), &HashMap::new());
        assert!(got.is_empty());
    }

    #[test]
    fn commit_inside_interval_anchors_parked_branch() {
        let prior = [checkout(0, "app", "ABC-123-sending-plans")];
        let inwin = [commit(30_000, "app", "ABC-123-sending-plans")];
        let intervals = [(1, 1_000, 61_000)];
        let got = anchor_tasks(&intervals, &prior, &inwin, &re(), &HashMap::new());
        assert_eq!(got, vec![(1, "ABC-123".to_owned())]);
    }

    #[test]
    fn checkout_shortly_before_interval_anchors() {
        // Switch-then-focus: checkout 5 min before the task's first
        // interval still passes the activity grace.
        let inwin = [checkout(0, "app", "ABC-123-x")];
        let intervals = [(1, 300_000, 3_900_000)];
        let got = anchor_tasks(&intervals, &[], &inwin, &re(), &HashMap::new());
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
        let got = anchor_tasks(&intervals, &prior, &inwin, &re(), &HashMap::new());
        assert_eq!(got, vec![(2, "ABC-123".to_owned())]);
    }

    #[test]
    fn unticketed_branch_anchors_nothing() {
        let prior = [checkout(0, "app", "main")];
        let got = anchor_tasks(&[(1, 1_000, 61_000)], &prior, &[], &re(), &HashMap::new());
        assert!(got.is_empty());
    }

    #[test]
    fn majority_branch_wins_after_mid_window_switch() {
        let prior = [checkout(0, "app", "ABC-1-x")];
        // Switch at 20s of a 0–60s interval: ABC-2 holds 40 of 60s.
        let inwin = [checkout(20_000, "app", "ABC-2-y")];
        let got = anchor_tasks(&[(1, 0, 60_000)], &prior, &inwin, &re(), &HashMap::new());
        assert_eq!(got, vec![(1, "ABC-2".to_owned())]);
    }

    #[test]
    fn exact_half_is_not_a_majority() {
        let prior = [checkout(0, "app", "ABC-1-x")];
        let inwin = [checkout(30_000, "app", "main")];
        let got = anchor_tasks(&[(1, 0, 60_000)], &prior, &inwin, &re(), &HashMap::new());
        assert!(got.is_empty());
    }

    #[test]
    fn tasks_anchor_independently() {
        // Task 1 sat on parked ABC-1 (no in-window activity → unanchored);
        // task 2 starts at the DEF-2 checkout and anchors to it.
        let prior = [checkout(0, "app", "ABC-1-x")];
        let inwin = [checkout(60_000, "app", "DEF-2-y")];
        let intervals = [(1, 0, 60_000), (2, 60_000, 120_000)];
        let got = anchor_tasks(&intervals, &prior, &inwin, &re(), &HashMap::new());
        assert_eq!(got, vec![(2, "DEF-2".to_owned())]);
    }
}
