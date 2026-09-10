//! Chronicle-installed git hooks (m37 chunk 2): opt-in. Each hook is one
//! backgrounded, silenced shell line appended after whatever is already in
//! the file (Husky, pre-commit, a hand-written hook) — never replacing it —
//! marked with a trailing `# chronicle` comment so install/remove can find
//! and strip just that line.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use chronicle_core::types::{ActivityEvent, ActivityKind};
use jiff::Timestamp;

pub const HOOK_NAMES: [&str; 3] = ["post-checkout", "post-commit", "post-rewrite"];

const MARKER: &str = "# chronicle";

/// One line: backgrounded (never blocks the git command that fired the
/// hook), output silenced (never pollutes hook stdout/stderr), marked with
/// a trailing comment `install`/`remove` grep for.
pub fn hook_line(exe: &Path, name: &str) -> String {
    format!(
        "{} hook {name} \"$@\" >/dev/null 2>&1 & {MARKER}",
        exe.display()
    )
}

/// The hooks dir git will actually run: honours `core.hooksPath` via
/// `git -C <repo> rev-parse --git-path hooks`, falling back to
/// `<git_dir>/hooks` when the git binary itself is unavailable.
fn hooks_dir(repo: &Path) -> PathBuf {
    if let Ok(out) = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["rev-parse", "--git-path", "hooks"])
        .output()
        && out.status.success()
    {
        let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !s.is_empty() {
            let p = PathBuf::from(&s);
            return if p.is_absolute() { p } else { repo.join(p) };
        }
    }
    crate::git::resolve_git_dir(repo)
        .map(|d| d.join("hooks"))
        .unwrap_or_else(|| repo.join(".git/hooks"))
}

#[cfg(unix)]
fn set_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perm = std::fs::metadata(path)?.permissions();
    perm.set_mode(0o755);
    std::fs::set_permissions(path, perm)
}

#[cfg(not(unix))]
fn set_executable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn has_marker(content: &str) -> bool {
    content.lines().any(|l| l.trim_end().ends_with(MARKER))
}

/// Installs the marker line into each of [`HOOK_NAMES`], creating the file
/// (with a `#!/bin/sh` shebang, mode 0755) if it doesn't exist, appending
/// after existing content if it does and lacks the marker, or leaving it
/// untouched if the marker is already there. Returns the files actually
/// created or appended to.
pub fn install(repo: &Path, exe: &Path) -> std::io::Result<Vec<PathBuf>> {
    let dir = hooks_dir(repo);
    std::fs::create_dir_all(&dir)?;
    let mut touched = Vec::new();
    for name in HOOK_NAMES {
        let path = dir.join(name);
        let line = hook_line(exe, name);
        if !path.exists() {
            let mut f = std::fs::File::create(&path)?;
            writeln!(f, "#!/bin/sh")?;
            writeln!(f, "{line}")?;
            drop(f);
            set_executable(&path)?;
            touched.push(path);
            continue;
        }
        let mut content = std::fs::read_to_string(&path)?;
        if has_marker(&content) {
            continue;
        }
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&line);
        content.push('\n');
        std::fs::write(&path, content)?;
        set_executable(&path)?;
        touched.push(path);
    }
    Ok(touched)
}

/// Strips the marker line from each hook file, deleting a file left with
/// nothing but the shebang (and blank lines) — the file Chronicle itself
/// created, since anything else already in the file would still be there.
/// Returns the files touched (edited or deleted).
pub fn remove(repo: &Path) -> std::io::Result<Vec<PathBuf>> {
    let dir = hooks_dir(repo);
    let mut touched = Vec::new();
    for name in HOOK_NAMES {
        let path = dir.join(name);
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if !has_marker(&content) {
            continue;
        }
        let remaining: Vec<&str> = content
            .lines()
            .filter(|l| !l.trim_end().ends_with(MARKER))
            .collect();
        let only_shebang = remaining
            .iter()
            .all(|l| l.trim().is_empty() || l.trim() == "#!/bin/sh");
        if only_shebang {
            std::fs::remove_file(&path)?;
        } else {
            let mut new_content = remaining.join("\n");
            if !new_content.is_empty() {
                new_content.push('\n');
            }
            std::fs::write(&path, new_content)?;
        }
        touched.push(path);
    }
    Ok(touched)
}

pub struct HookStatus {
    pub installed: Vec<&'static str>,
    pub hooks_dir: PathBuf,
}

pub fn status(repo: &Path) -> HookStatus {
    let dir = hooks_dir(repo);
    let installed = HOOK_NAMES
        .into_iter()
        .filter(|name| {
            std::fs::read_to_string(dir.join(name))
                .map(|c| has_marker(&c))
                .unwrap_or(false)
        })
        .collect();
    HookStatus {
        installed,
        hooks_dir: dir,
    }
}

/// What `chronicle hook <name>` collects, run by the shell line `install`
/// wrote, inside the repo's work dir.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HookPost {
    /// Absolute work dir.
    pub repo: String,
    /// `post-checkout` | `post-commit` | `post-rewrite`.
    pub event: String,
    pub branch: String,
    /// HEAD sha.
    pub commit: String,
    /// Commit subject, `post-commit` only.
    pub subject: Option<String>,
    pub ts_ms: i64,
}

/// `git`'s hook args: `post-checkout` gets `<old> <new> <flag>` where flag
/// `0` is a file checkout (not a branch switch) — returns `None`.
/// `post-rewrite` reads the list of rewritten commits from stdin; this
/// ignores that and just reports the resulting HEAD.
pub fn collect(name: &str, cwd: &Path, args: &[String]) -> Option<HookPost> {
    if name == "post-checkout" && args.get(2).map(String::as_str) == Some("0") {
        return None;
    }
    let branch = run_git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    let commit = run_git(cwd, &["rev-parse", "HEAD"])?;
    let subject = (name == "post-commit")
        .then(|| run_git(cwd, &["log", "-1", "--format=%s"]))
        .flatten();
    let repo = cwd
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.display().to_string());
    Some(HookPost {
        repo,
        event: name.to_owned(),
        branch,
        commit,
        subject,
        ts_ms: Timestamp::now().as_millisecond(),
    })
}

fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!s.is_empty()).then_some(s)
}

/// `post-checkout` → one `Checkout`; `post-commit` → one `Commit`;
/// `post-rewrite` produces nothing (no `ActivityKind` for it yet).
///
/// Storage note on `Commit`: `ActivityKind::Commit` dedupes as
/// [`chronicle_core::types::Dedupe::None`], and
/// `insert_activity_event` (`crates/core/src/storage.rs`) stores `None`
/// rows with `INSERT OR IGNORE` against the unique index
/// `(kind, ext_id, ts) WHERE ext_id IS NOT NULL`
/// (`crates/core/migrations/010_activity_events.sql:11`). This sets
/// `ext_id` to the commit sha — identical to what the m15 poller
/// (`crate::git`) uses for its own later observation of the same commit —
/// but because `ts` is part of that unique key and the hook's `ts_ms`
/// (commit time) will almost never equal the poller's `ts` (up to 20s
/// later, its next poll), the two rows do NOT collide: **the same commit
/// would be stored twice**, once from the hook and once from the poller.
/// Fixing that needs a storage-side change (upserting `Dedupe::None`
/// `Commit` rows on `(kind, ext_id)` alone, dropping `ts` from the key) —
/// out of scope here.
pub fn to_events(post: &HookPost) -> Vec<ActivityEvent> {
    let ts = Timestamp::from_millisecond(post.ts_ms).unwrap_or_else(|_| Timestamp::now());
    match post.event.as_str() {
        "post-checkout" => vec![ActivityEvent {
            ts,
            end_ts: None,
            repo: post.repo.clone(),
            branch: post.branch.clone(),
            kind: ActivityKind::Checkout,
            ext_id: Some(format!("hook:{}@{}", post.commit, post.ts_ms)),
            summary: None,
            detail: None,
        }],
        "post-commit" => vec![ActivityEvent {
            ts,
            end_ts: None,
            repo: post.repo.clone(),
            branch: post.branch.clone(),
            kind: ActivityKind::Commit,
            ext_id: Some(post.commit.clone()),
            summary: post.subject.clone(),
            detail: None,
        }],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let run = |args: &[&str]| {
            Command::new("git")
                .arg("-C")
                .arg(dir.path())
                .args(args)
                .output()
                .unwrap()
        };
        run(&["init", "-q"]);
        run(&["config", "user.email", "t@t.example"]);
        run(&["config", "user.name", "t"]);
        std::fs::write(dir.path().join("f"), "x").unwrap();
        run(&["add", "."]);
        run(&["commit", "-q", "-m", "init"]);
        dir
    }

    fn hooks_path(repo: &Path, name: &str) -> PathBuf {
        repo.join(".git/hooks").join(name)
    }

    #[test]
    fn install_creates_three_hooks_marked_and_executable() {
        let repo = init_repo();
        let exe = PathBuf::from("/usr/bin/chronicle");
        let touched = install(repo.path(), &exe).unwrap();
        assert_eq!(touched.len(), 3, "{touched:?}");
        for name in HOOK_NAMES {
            let path = hooks_path(repo.path(), name);
            let content = std::fs::read_to_string(&path).unwrap();
            assert!(content.starts_with("#!/bin/sh\n"), "{name}: {content}");
            assert!(has_marker(&content), "{name}: {content}");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o755, "{name}");
            }
        }
    }

    #[test]
    fn install_twice_is_idempotent() {
        let repo = init_repo();
        let exe = PathBuf::from("/usr/bin/chronicle");
        install(repo.path(), &exe).unwrap();
        let before = std::fs::read_to_string(hooks_path(repo.path(), "post-commit")).unwrap();
        let touched_again = install(repo.path(), &exe).unwrap();
        let after = std::fs::read_to_string(hooks_path(repo.path(), "post-commit")).unwrap();
        assert!(touched_again.is_empty(), "{touched_again:?}");
        assert_eq!(before, after);
    }

    #[test]
    fn install_appends_after_existing_hook_content() {
        let repo = init_repo();
        let dir = repo.path().join(".git/hooks");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("post-commit"), "#!/bin/sh\necho husky\n").unwrap();
        let exe = PathBuf::from("/usr/bin/chronicle");
        install(repo.path(), &exe).unwrap();
        let content = std::fs::read_to_string(dir.join("post-commit")).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines[0], "#!/bin/sh");
        assert_eq!(lines[1], "echo husky");
        assert!(lines[2].ends_with(MARKER), "{content}");
    }

    #[test]
    fn remove_deletes_files_chronicle_created() {
        let repo = init_repo();
        let exe = PathBuf::from("/usr/bin/chronicle");
        install(repo.path(), &exe).unwrap();
        let touched = remove(repo.path()).unwrap();
        assert_eq!(touched.len(), 3, "{touched:?}");
        for name in HOOK_NAMES {
            assert!(
                !hooks_path(repo.path(), name).exists(),
                "{name} should be gone"
            );
        }
    }

    #[test]
    fn remove_keeps_other_content_strips_only_marker() {
        let repo = init_repo();
        let dir = repo.path().join(".git/hooks");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("post-commit"), "#!/bin/sh\necho husky\n").unwrap();
        let exe = PathBuf::from("/usr/bin/chronicle");
        install(repo.path(), &exe).unwrap();
        remove(repo.path()).unwrap();
        let content = std::fs::read_to_string(dir.join("post-commit")).unwrap();
        assert!(content.contains("echo husky"), "{content}");
        assert!(!has_marker(&content), "{content}");
        assert!(hooks_path(repo.path(), "post-commit").exists());
    }

    #[test]
    fn collect_file_checkout_is_none() {
        let repo = init_repo();
        let args = ["a".to_string(), "b".to_string(), "0".to_string()];
        assert!(collect("post-checkout", repo.path(), &args).is_none());
    }

    #[test]
    fn collect_post_commit_reads_subject_and_head() {
        let repo = init_repo();
        let post = collect("post-commit", repo.path(), &[]).unwrap();
        assert_eq!(post.event, "post-commit");
        assert_eq!(post.subject.as_deref(), Some("init"));
        assert!(!post.commit.is_empty());
        assert!(!post.branch.is_empty());
    }

    #[test]
    fn to_events_maps_checkout_and_commit() {
        let post = HookPost {
            repo: "chronicle".into(),
            event: "post-checkout".into(),
            branch: "main".into(),
            commit: "abc123".into(),
            subject: None,
            ts_ms: 1_000,
        };
        let events = to_events(&post);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, ActivityKind::Checkout);
        assert_eq!(events[0].ext_id.as_deref(), Some("hook:abc123@1000"));

        let post = HookPost {
            event: "post-commit".into(),
            subject: Some("fix: thing".into()),
            ..post
        };
        let events = to_events(&post);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, ActivityKind::Commit);
        assert_eq!(events[0].ext_id.as_deref(), Some("abc123"));
        assert_eq!(events[0].summary.as_deref(), Some("fix: thing"));

        let post = HookPost {
            event: "post-rewrite".into(),
            ..post
        };
        assert!(to_events(&post).is_empty());
    }
}
