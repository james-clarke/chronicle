//! Shell history collector (m26 chunk 4): atuin history folded into
//! `shell` spans per repo; never the command line.
//!
//! atuin's `history.db` is read-only and `query_only` here — the daemon is a
//! reader of somebody else's database. Only `cwd`, the program name and the
//! duration leave the SQL boundary; the command line (secrets, tokens, paths)
//! is dropped inside `read_since`.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::time::Duration;

use chronicle_core::types::{ActivityEvent, ActivityKind, CaptureEvent, ms_to_ts};
use crossbeam_channel::Sender;
use jiff::Timestamp;
use rusqlite::{Connection, OpenFlags, params};

use crate::{BoxError, FocusProvider};

const POLL: Duration = Duration::from_secs(60);
/// Commands further apart than this are separate stretches of terminal work.
/// Shared with the shell-hook fold (shell_hook.rs), which closes on the same
/// gap.
pub(crate) const GAP_MS: i64 = 10 * 60 * 1000;
/// A command left running (a dev server, a `sleep`) must not stretch the span
/// past the gap that would have closed it.
const MAX_CMD_MS: i64 = GAP_MS;
const BATCH: i64 = 5000;

/// atuin's default history path.
pub fn default_db_path() -> PathBuf {
    chronicle_core::config::expand_home("~/.local/share/atuin/history.db")
}

pub struct ShellProvider {
    db: PathBuf,
    fold: Fold,
    /// atuin stores nanoseconds since the epoch in `history.timestamp`.
    last_seen_ns: i64,
}

impl ShellProvider {
    pub fn new(db: PathBuf, repos: &[PathBuf]) -> Self {
        Self {
            db,
            fold: Fold::new(repos),
            last_seen_ns: 0,
        }
    }
}

impl FocusProvider for ShellProvider {
    fn run(mut self, tx: Sender<CaptureEvent>) -> Result<(), BoxError> {
        let conn = Connection::open_with_flags(
            &self.db,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.pragma_update(None, "query_only", true)?;
        // History from before the daemon started belongs to batches already
        // derived; start at now.
        self.last_seen_ns = Timestamp::now().as_millisecond().saturating_mul(1_000_000);
        loop {
            std::thread::sleep(POLL);
            let (cmds, last_ns) = match read_since(&conn, self.last_seen_ns) {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!("atuin history read failed: {e}");
                    continue;
                }
            };
            self.last_seen_ns = last_ns;
            for event in self.fold.step(&cmds, Timestamp::now().as_millisecond()) {
                if tx.send(CaptureEvent::Activity(event)).is_err() {
                    return Ok(());
                }
            }
        }
    }
}

/// What a history row is reduced to. No command line, by construction.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cmd {
    start_ms: i64,
    end_ms: i64,
    program: String,
    cwd: String,
}

/// Rows newer than `since_ns`, oldest first, plus the newest timestamp seen
/// (the next poll's watermark).
fn read_since(conn: &Connection, since_ns: i64) -> rusqlite::Result<(Vec<Cmd>, i64)> {
    let mut stmt = conn.prepare(
        "SELECT timestamp, duration, command, cwd FROM history
         WHERE timestamp > ?1 AND deleted_at IS NULL
         ORDER BY timestamp LIMIT ?2",
    )?;
    let rows = stmt.query_map(params![since_ns, BATCH], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
        ))
    })?;
    let mut last_ns = since_ns;
    let mut out = Vec::new();
    for row in rows {
        let (ts_ns, dur_ns, command, cwd) = row?;
        last_ns = last_ns.max(ts_ns);
        // atuin writes -1 while the command is still running.
        let dur_ms = (dur_ns.max(0) / 1_000_000).min(MAX_CMD_MS);
        let start_ms = ts_ns / 1_000_000;
        if let Some(program) = program_name(&command) {
            out.push(Cmd {
                start_ms,
                end_ms: start_ms + dur_ms,
                program,
                cwd,
            });
        }
    }
    Ok((out, last_ns))
}

/// `argv[0]`, skipping leading `NAME=value` assignments so an inline secret
/// (`GH_TOKEN=… gh …`) is never kept.
fn program_name(command: &str) -> Option<String> {
    command
        .split_whitespace()
        .find(|t| !is_env_assignment(t))
        .map(str::to_owned)
}

fn is_env_assignment(token: &str) -> bool {
    match token.split_once('=') {
        Some((name, _)) => {
            !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        }
        None => false,
    }
}

/// One open span per repo (the empty name is the outside-any-repo span).
struct Fold {
    repos: Vec<(String, PathBuf)>,
    open: HashMap<String, Span>,
}

struct Span {
    repo: String,
    /// First command's cwd; the span's identity together with its start.
    cwd: String,
    start_ms: i64,
    last_ms: i64,
    counts: HashMap<String, usize>,
}

impl Span {
    /// Every emit carries the counts so far; storage's upsert keeps the
    /// newest non-empty summary under the same `ext_id`.
    fn event(&self) -> ActivityEvent {
        ActivityEvent {
            ts: ms_to_ts(self.start_ms),
            end_ts: Some(ms_to_ts(self.last_ms)),
            repo: self.repo.clone(),
            branch: String::new(),
            kind: ActivityKind::Shell,
            ext_id: Some(format!("{}#{}", self.cwd, self.start_ms)),
            summary: Some(summarize_counts(&self.counts)),
            detail: None,
        }
    }
}

/// Top three by count, ties by name: "cargo ×12 · git ×5". Shared with the
/// shell-hook fold (shell_hook.rs), which folds the same way per place.
pub(crate) fn summarize_counts(counts: &HashMap<String, usize>) -> String {
    let mut counts: Vec<(&String, &usize)> = counts.iter().collect();
    counts.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    counts
        .iter()
        .take(3)
        .map(|(p, n)| format!("{p} \u{d7}{n}"))
        .collect::<Vec<_>>()
        .join(" \u{b7} ")
}

impl Fold {
    fn new(repos: &[PathBuf]) -> Self {
        let repos = repos
            .iter()
            .filter_map(|p| {
                let name = p.file_name()?.to_string_lossy().into_owned();
                Some((name, p.clone()))
            })
            .collect();
        Self {
            repos,
            open: HashMap::new(),
        }
    }

    /// Deepest configured repo containing `cwd`, else "" (outside-repo work,
    /// which never anchors placement).
    fn repo_of(&self, cwd: &str) -> String {
        let cwd = Path::new(cwd);
        self.repos
            .iter()
            .filter(|(_, path)| cwd.starts_with(path))
            .max_by_key(|(_, path)| path.components().count())
            .map(|(name, _)| name.clone())
            .unwrap_or_default()
    }

    /// Folds `cmds` (timestamp order) into the open spans and returns the
    /// events to store: closed spans, then the still-open spans that grew,
    /// with a refreshed `end_ts`.
    fn step(&mut self, cmds: &[Cmd], now_ms: i64) -> Vec<ActivityEvent> {
        let mut out = Vec::new();
        let mut touched: BTreeSet<String> = BTreeSet::new();
        for c in cmds {
            let repo = self.repo_of(&c.cwd);
            let extend = self
                .open
                .get(&repo)
                .is_some_and(|s| c.start_ms - s.last_ms <= GAP_MS);
            if extend {
                let s = self.open.get_mut(&repo).expect("checked above");
                s.last_ms = s.last_ms.max(c.end_ms);
                *s.counts.entry(c.program.clone()).or_default() += 1;
            } else {
                if let Some(prev) = self.open.remove(&repo) {
                    out.push(prev.event());
                }
                self.open.insert(
                    repo.clone(),
                    Span {
                        repo: repo.clone(),
                        cwd: c.cwd.clone(),
                        start_ms: c.start_ms,
                        last_ms: c.end_ms,
                        counts: HashMap::from([(c.program.clone(), 1)]),
                    },
                );
            }
            touched.insert(repo);
        }
        let stale: Vec<String> = self
            .open
            .iter()
            .filter(|(_, s)| now_ms - s.last_ms > GAP_MS)
            .map(|(k, _)| k.clone())
            .collect();
        for k in stale {
            let s = self.open.remove(&k).expect("just listed");
            touched.remove(&k);
            out.push(s.event());
        }
        for k in touched {
            if let Some(s) = self.open.get(&k) {
                out.push(s.event());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCHEMA: &str = "CREATE TABLE history (
        id text primary key, timestamp integer not null, duration integer not null,
        exit integer not null, command text not null, cwd text not null,
        session text not null, hostname text not null, deleted_at integer)";

    fn history(rows: &[(i64, i64, &str, &str, Option<i64>)]) -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(SCHEMA).unwrap();
        for (i, (ts, dur, cmd, cwd, deleted)) in rows.iter().enumerate() {
            conn.execute(
                "INSERT INTO history VALUES (?1, ?2, ?3, 0, ?4, ?5, 's', 'h', ?6)",
                params![i.to_string(), ts, dur, cmd, cwd, deleted],
            )
            .unwrap();
        }
        conn
    }

    const T0: i64 = 1_788_451_000_000_000_000;
    const MIN: i64 = 60_000_000_000;

    #[test]
    fn reads_only_new_undeleted_rows_and_keeps_no_command_line() {
        let conn = history(&[
            (
                T0,
                2_000_000_000,
                "cargo test --workspace",
                "/home/x/dev/c",
                None,
            ),
            (T0 + MIN, -1, "git push", "/home/x/dev/c", None),
            (T0 + 2 * MIN, 1, "rm secret", "/home/x/dev/c", Some(1)),
            (T0 - MIN, 1, "old", "/home/x/dev/c", None),
        ]);
        let (cmds, last) = read_since(&conn, T0 - 1).unwrap();
        assert_eq!(last, T0 + MIN);
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0].program, "cargo");
        assert_eq!(cmds[0].end_ms - cmds[0].start_ms, 2000);
        // -1 (still running) is clamped, not negative.
        assert_eq!(cmds[1].program, "git");
        assert_eq!(cmds[1].end_ms, cmds[1].start_ms);
        assert!(read_since(&conn, T0 + MIN).unwrap().0.is_empty());
    }

    #[test]
    fn program_name_skips_env_assignments() {
        assert_eq!(program_name("cargo build").as_deref(), Some("cargo"));
        assert_eq!(
            program_name("GH_TOKEN=abc123 gh pr list").as_deref(),
            Some("gh")
        );
        assert_eq!(program_name("  ").as_deref(), None);
    }

    fn cmd(start_ms: i64, dur_ms: i64, program: &str, cwd: &str) -> Cmd {
        Cmd {
            start_ms,
            end_ms: start_ms + dur_ms,
            program: program.to_owned(),
            cwd: cwd.to_owned(),
        }
    }

    #[test]
    fn folds_a_burst_per_repo_and_summarizes_the_top_three() {
        let mut fold = Fold::new(&[PathBuf::from("/home/x/dev/chronicle")]);
        let t = 1_788_451_000_000;
        let burst = [
            cmd(t, 1000, "cargo", "/home/x/dev/chronicle"),
            cmd(t + 60_000, 500, "git", "/home/x/dev/chronicle/crates"),
            cmd(t + 120_000, 500, "cargo", "/home/x/dev/chronicle"),
            cmd(t + 180_000, 500, "rtk", "/home/x/dev/chronicle"),
            cmd(t + 240_000, 500, "vim", "/home/x/dev/chronicle"),
            cmd(t + 300_000, 500, "ls", "/home/x/tmp"),
        ];
        let open = fold.step(&burst, t + 300_000);
        assert_eq!(open.len(), 2, "one open span per repo");
        let chronicle = open.iter().find(|e| e.repo == "chronicle").unwrap();
        assert_eq!(chronicle.kind, ActivityKind::Shell);
        assert_eq!(
            chronicle.summary.as_deref(),
            Some("cargo \u{d7}2 \u{b7} git \u{d7}1 \u{b7} rtk \u{d7}1")
        );
        assert_eq!(
            chronicle.ext_id.as_deref(),
            Some(format!("/home/x/dev/chronicle#{t}").as_str())
        );
        assert_eq!(chronicle.ts, ms_to_ts(t));
        assert_eq!(chronicle.end_ts, Some(ms_to_ts(t + 240_500)));
        assert!(
            open.iter().any(|e| e.repo.is_empty()),
            "outside any configured repo"
        );

        // Eleven idle minutes close both spans under the same ext_id.
        let closed = fold.step(&[], t + 300_000 + 11 * 60_000);
        assert_eq!(closed.len(), 2);
        let chronicle_close = closed.iter().find(|e| e.repo == "chronicle").unwrap();
        assert_eq!(chronicle_close.ext_id, chronicle.ext_id);
        assert_eq!(
            chronicle_close.summary.as_deref(),
            Some("cargo \u{d7}2 \u{b7} git \u{d7}1 \u{b7} rtk \u{d7}1")
        );
        assert_eq!(
            closed.iter().find(|e| e.repo.is_empty()).unwrap().summary,
            Some("ls \u{d7}1".to_owned())
        );
    }

    #[test]
    fn a_gap_starts_a_new_span() {
        let mut fold = Fold::new(&[PathBuf::from("/home/x/dev/chronicle")]);
        let t = 1_788_451_000_000;
        let first = fold.step(&[cmd(t, 0, "cargo", "/home/x/dev/chronicle")], t);
        assert_eq!(first.len(), 1);
        let late = t + 11 * 60_000;
        let events = fold.step(&[cmd(late, 0, "git", "/home/x/dev/chronicle")], late);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].summary.as_deref(), Some("cargo \u{d7}1"));
        assert_eq!(events[0].ext_id, first[0].ext_id);
        assert_eq!(events[1].summary.as_deref(), Some("git \u{d7}1"));
        assert_eq!(
            events[1].ext_id.as_deref(),
            Some(format!("/home/x/dev/chronicle#{late}").as_str())
        );
    }

    #[test]
    fn history_rows_fold_end_to_end() {
        let conn = history(&[
            (
                T0,
                1_000_000_000,
                "cargo build",
                "/home/x/dev/chronicle",
                None,
            ),
            (T0 + MIN, 0, "cargo test", "/home/x/dev/chronicle", None),
            (T0 + 20 * MIN, 0, "git log", "/home/x/dev/chronicle", None),
        ]);
        let (cmds, _) = read_since(&conn, 0).unwrap();
        let mut fold = Fold::new(&[PathBuf::from("/home/x/dev/chronicle")]);
        let events = fold.step(&cmds, T0 / 1_000_000 + 20 * 60_000);
        assert_eq!(
            events.len(),
            2,
            "the closed cargo span, then the open git span"
        );
        assert_eq!(events[0].summary.as_deref(), Some("cargo \u{d7}2"));
        assert_eq!(events[0].repo, "chronicle");
        assert_eq!(events[1].summary.as_deref(), Some("git \u{d7}1"));
    }
}
