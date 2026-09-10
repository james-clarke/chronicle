//! Reflog backfill (m37 chunk 2): recover checkout history the live git
//! poller (`git.rs`) never saw because it wasn't running yet, by reading
//! `git reflog show -g HEAD` for a window in the past. Read-only; shells
//! out to `git log`'s sibling `git reflog show`, nothing is written.
//!
//! Dedupe note: `ActivityKind::Checkout` uses `Dedupe::LatestCheckout`
//! (see `chronicle_core::storage::insert_activity_event`), which drops an
//! incoming checkout only when its branch equals the *single* most
//! recently stored checkout row for that repo (by `ts DESC, id DESC`) —
//! it never looks at `ts` ordering or `ext_id` to decide. So:
//! - Two backfill runs over the same window are idempotent regardless:
//!   `ext_id` embeds `<sha>@<ts_ms>`, so a rerun hits the `(kind, ext_id,
//!   ts)` unique index (migration 010) and is ignored outright.
//! - A backfill inserted oldest-first, before the poller has recorded any
//!   live checkout for the repo, dedupes exactly like the poller does:
//!   consecutive identical branches collapse to one row, which is correct.
//! - But if the poller has *already* recorded a live checkout for the
//!   repo with a branch and `ts` newer than the backfill window, that live
//!   row is what "latest" resolves to while backfilling — a historical
//!   reflog checkout whose branch happens to match that live branch is
//!   silently dropped even though it is a distinct, older event. This
//!   under-counts rather than duplicates, and only when the branch name
//!   repeats; it is a known gap, not a bug this module can fix without
//!   changing `insert_activity_event`.

use std::path::Path;

use chronicle_core::types::{ActivityEvent, ActivityKind, ts_to_ms};
use jiff::Timestamp;

/// Checkout events from `repo`'s reflog with `ts` in `[since_ms, now_ms)`,
/// oldest first. Empty when `repo` has no git history, `git` is missing, or
/// the reflog is empty — never panics.
pub fn reflog_checkouts(repo: &Path, since_ms: i64, now_ms: i64) -> Vec<ActivityEvent> {
    let Ok(out) = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["reflog", "show", "--date=iso-strict", "-g", "HEAD"])
        .output()
    else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&out.stdout);
    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| repo.display().to_string());

    // `git reflog` prints newest first; collect then reverse for oldest-first.
    let mut events: Vec<ActivityEvent> = text
        .lines()
        .filter_map(parse_line)
        .filter_map(|(sha, ts, branch)| {
            let ms = ts_to_ms(ts);
            if ms < since_ms || ms >= now_ms {
                return None;
            }
            Some(ActivityEvent {
                ts,
                end_ts: None,
                repo: name.clone(),
                branch,
                kind: ActivityKind::Checkout,
                ext_id: Some(format!("reflog:{sha}@{ms}")),
                summary: None,
                detail: None,
            })
        })
        .collect();
    events.reverse();
    events
}

/// `<sha> HEAD@{<iso-strict date>}: checkout: moving from <a> to <b>` →
/// `(sha, date, b)`. Any other entry kind (`commit:`, `commit (amend):`,
/// `pull:`, …) returns `None`.
fn parse_line(line: &str) -> Option<(String, Timestamp, String)> {
    let (sha, rest) = line.split_once(' ')?;
    let rest = rest.strip_prefix("HEAD@{")?;
    let (date_str, rest) = rest.split_once('}')?;
    let msg = rest.strip_prefix(": ")?;
    let moving = msg.strip_prefix("checkout: ")?;
    let (_, to) = moving.rsplit_once(" to ")?;
    let ts: Timestamp = date_str.parse().ok()?;
    Some((sha.to_owned(), ts, to.trim().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_checkout_line() {
        let line =
            "abc1234 HEAD@{2026-09-09T20:47:11-04:00}: checkout: moving from main to ABC-1-x";
        let (sha, ts, branch) = parse_line(line).unwrap();
        assert_eq!(sha, "abc1234");
        assert_eq!(branch, "ABC-1-x");
        assert_eq!(ts.to_string(), "2026-09-10T00:47:11Z");
    }

    #[test]
    fn parses_detached_head_target() {
        let line = "deadbee HEAD@{2026-09-09T12:00:00-04:00}: checkout: moving from main to deadbeefcafefeed";
        let (_, _, branch) = parse_line(line).unwrap();
        assert_eq!(branch, "deadbeefcafefeed");
    }

    #[test]
    fn skips_non_checkout_lines() {
        let line = "abc1234 HEAD@{2026-09-09T20:47:11-04:00}: commit: fix thing";
        assert!(parse_line(line).is_none());
    }

    #[test]
    fn reflog_checkouts_on_missing_repo_is_empty() {
        assert!(reflog_checkouts(Path::new("/no/such/repo-xyz"), 0, i64::MAX).is_empty());
    }

    /// Integration: init a temp repo, commit, branch, check out — one
    /// checkout should be found. Skipped when `git` isn't on PATH.
    #[test]
    fn finds_checkout_in_real_repo() {
        if std::process::Command::new("git")
            .arg("--version")
            .output()
            .is_err()
        {
            return;
        }
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path();
        let run = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .arg("-C")
                .arg(repo)
                .args(args)
                .env("GIT_AUTHOR_NAME", "t")
                .env("GIT_AUTHOR_EMAIL", "t@t.com")
                .env("GIT_COMMITTER_NAME", "t")
                .env("GIT_COMMITTER_EMAIL", "t@t.com")
                .status()
                .unwrap();
            assert!(status.success());
        };
        run(&["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("f.txt"), "1").unwrap();
        run(&["add", "f.txt"]);
        run(&["commit", "-q", "-m", "init"]);
        run(&["checkout", "-q", "-b", "feature"]);

        let events = reflog_checkouts(repo, 0, i64::MAX);
        assert!(
            events.iter().any(|e| e.branch == "feature"
                && e.kind == ActivityKind::Checkout
                && e.repo == repo.file_name().unwrap().to_string_lossy()),
            "expected a checkout to feature, got {events:?}"
        );
    }
}
