//! Git repo poller (m15): emits `VcsEvent`s for branch checkouts and new
//! commits. Reads `.git/HEAD` and ref files directly every poll (two tiny
//! reads per repo); shells out to git(1) only when a new commit needs its
//! subject line. No hooks installed, nothing written to the repo.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chronicle_core::types::{CaptureEvent, VcsEvent, VcsKind};
use crossbeam_channel::Sender;
use jiff::Timestamp;

use crate::{BoxError, FocusProvider};

const POLL: Duration = Duration::from_secs(20);

pub struct GitProvider {
    repos: Vec<Repo>,
}

struct Repo {
    /// Directory name, the `repo` field on events.
    name: String,
    work_dir: PathBuf,
    git_dir: PathBuf,
    state: Option<RepoState>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RepoState {
    branch: String,
    head: Option<String>,
}

impl GitProvider {
    /// Paths that don't resolve to a git dir are skipped with a warning —
    /// a renamed repo must not kill the provider thread.
    pub fn new(paths: &[PathBuf]) -> Self {
        let repos = paths
            .iter()
            .filter_map(|p| {
                let git_dir = resolve_git_dir(p)?;
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| p.display().to_string());
                Some(Repo {
                    name,
                    work_dir: p.clone(),
                    git_dir,
                    state: None,
                })
            })
            .collect();
        Self { repos }
    }

    pub fn is_empty(&self) -> bool {
        self.repos.is_empty()
    }
}

impl FocusProvider for GitProvider {
    fn run(mut self, tx: Sender<CaptureEvent>) -> Result<(), BoxError> {
        // Announce current branches once so the anchoring timeline has a
        // starting state; storage drops the re-announcement if unchanged.
        for repo in &mut self.repos {
            repo.state = read_state(&repo.git_dir);
            if let Some(s) = &repo.state
                && tx
                    .send(CaptureEvent::Vcs(checkout_event(&repo.name, s)))
                    .is_err()
            {
                return Ok(());
            }
        }
        loop {
            std::thread::sleep(POLL);
            for repo in &mut self.repos {
                let new = read_state(&repo.git_dir);
                let Some(event) = diff_state(repo.state.as_ref(), new.as_ref(), &repo.name) else {
                    repo.state = new;
                    continue;
                };
                let event = match event.kind {
                    VcsKind::Commit => VcsEvent {
                        summary: commit_subject(
                            &repo.work_dir,
                            event.commit_id.as_deref().unwrap_or("HEAD"),
                        ),
                        ..event
                    },
                    VcsKind::Checkout => event,
                };
                repo.state = new;
                if tx.send(CaptureEvent::Vcs(event)).is_err() {
                    return Ok(());
                }
            }
        }
    }
}

fn checkout_event(name: &str, s: &RepoState) -> VcsEvent {
    VcsEvent {
        ts: Timestamp::now(),
        repo: name.to_owned(),
        branch: s.branch.clone(),
        kind: VcsKind::Checkout,
        commit_id: None,
        summary: None,
    }
}

/// Branch change → checkout; same branch with a moved head → commit. A branch
/// switch swallows the simultaneous head move — the checkout is the story.
fn diff_state(old: Option<&RepoState>, new: Option<&RepoState>, name: &str) -> Option<VcsEvent> {
    let new = new?;
    match old {
        Some(o) if o.branch != new.branch => Some(checkout_event(name, new)),
        Some(o) if o.head != new.head && new.head.is_some() => Some(VcsEvent {
            ts: Timestamp::now(),
            repo: name.to_owned(),
            branch: new.branch.clone(),
            kind: VcsKind::Commit,
            commit_id: new.head.clone(),
            summary: None,
        }),
        Some(_) => None,
        None => Some(checkout_event(name, new)),
    }
}

/// `.git` may be a directory or, in worktrees/submodules, a file holding
/// `gitdir: <path>`.
fn resolve_git_dir(repo: &Path) -> Option<PathBuf> {
    let dot = repo.join(".git");
    if dot.is_dir() {
        return Some(dot);
    }
    let text = std::fs::read_to_string(&dot).ok()?;
    let rel = text.strip_prefix("gitdir:")?.trim();
    let dir = repo.join(rel);
    dir.is_dir().then_some(dir)
}

fn read_state(git_dir: &Path) -> Option<RepoState> {
    let head = std::fs::read_to_string(git_dir.join("HEAD")).ok()?;
    let head = head.trim();
    if let Some(r) = head.strip_prefix("ref: ") {
        let branch = r.strip_prefix("refs/heads/").unwrap_or(r).to_owned();
        Some(RepoState {
            head: resolve_ref(git_dir, r),
            branch,
        })
    } else {
        // Detached: the short hash stands in for a branch name.
        Some(RepoState {
            branch: head.chars().take(12).collect(),
            head: Some(head.to_owned()),
        })
    }
}

fn resolve_ref(git_dir: &Path, r: &str) -> Option<String> {
    if let Ok(hash) = std::fs::read_to_string(git_dir.join(r)) {
        return Some(hash.trim().to_owned());
    }
    let packed = std::fs::read_to_string(git_dir.join("packed-refs")).ok()?;
    packed
        .lines()
        .filter(|l| !l.starts_with(['#', '^']))
        .find_map(|l| {
            let (hash, name) = l.split_once(' ')?;
            (name == r).then(|| hash.to_owned())
        })
}

fn commit_subject(work_dir: &Path, hash: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["-C"])
        .arg(work_dir)
        .args(["log", "-1", "--format=%s", hash])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempRepo(PathBuf);

    impl TempRepo {
        fn new(branch: &str, hash: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "chronicle-git-test-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            let git = dir.join(".git");
            std::fs::create_dir_all(git.join("refs/heads")).unwrap();
            std::fs::write(git.join("HEAD"), format!("ref: refs/heads/{branch}\n")).unwrap();
            std::fs::write(git.join("refs/heads").join(branch), format!("{hash}\n")).unwrap();
            Self(dir)
        }
        fn git_dir(&self) -> PathBuf {
            self.0.join(".git")
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn reads_branch_and_loose_ref() {
        let repo = TempRepo::new("ABC-1-x", "abc123");
        let s = read_state(&repo.git_dir()).unwrap();
        assert_eq!(s.branch, "ABC-1-x");
        assert_eq!(s.head.as_deref(), Some("abc123"));
    }

    #[test]
    fn falls_back_to_packed_refs() {
        let repo = TempRepo::new("main", "aaa");
        std::fs::remove_file(repo.git_dir().join("refs/heads/main")).unwrap();
        std::fs::write(
            repo.git_dir().join("packed-refs"),
            "# pack-refs with: peeled fully-peeled sorted\nbbb refs/heads/main\n",
        )
        .unwrap();
        let s = read_state(&repo.git_dir()).unwrap();
        assert_eq!(s.head.as_deref(), Some("bbb"));
    }

    #[test]
    fn detached_head_uses_short_hash_as_branch() {
        let repo = TempRepo::new("main", "aaa");
        std::fs::write(
            repo.git_dir().join("HEAD"),
            "0123456789abcdef0123456789abcdef01234567\n",
        )
        .unwrap();
        let s = read_state(&repo.git_dir()).unwrap();
        assert_eq!(s.branch, "0123456789ab");
    }

    #[test]
    fn diff_detects_checkout_and_commit() {
        let a = RepoState {
            branch: "main".into(),
            head: Some("aaa".into()),
        };
        let b = RepoState {
            branch: "ABC-1-x".into(),
            head: Some("aaa".into()),
        };
        let c = RepoState {
            branch: "ABC-1-x".into(),
            head: Some("bbb".into()),
        };
        assert!(diff_state(Some(&a), Some(&a), "r").is_none());
        let sw = diff_state(Some(&a), Some(&b), "r").unwrap();
        assert_eq!(sw.kind, VcsKind::Checkout);
        assert_eq!(sw.branch, "ABC-1-x");
        let cm = diff_state(Some(&b), Some(&c), "r").unwrap();
        assert_eq!(cm.kind, VcsKind::Commit);
        assert_eq!(cm.commit_id.as_deref(), Some("bbb"));
        // Branch switch swallows the simultaneous head move.
        assert_eq!(
            diff_state(Some(&a), Some(&c), "r").unwrap().kind,
            VcsKind::Checkout
        );
    }
}
