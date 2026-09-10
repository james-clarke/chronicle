//! Session-format providers (m37 chunk 0): every coding tool's local
//! transcript store, read the same shape `ai_sessions.rs` already reads
//! Claude Code's. A `SessionFormat` finds its sources under a root
//! (`scan`) and reads new records incrementally from an opaque cursor
//! (`read`); `AiSessionProvider` folds the records into segments and
//! emits `ai_session` events, `<format>:<session>[#n]` for everything but
//! Claude Code (which keeps its bare id so nothing already stored
//! re-mints, m22).
//!
//! Amp is dropped: its local thread path is unverified.

pub mod aider;
pub mod claude;
pub mod cline;
pub mod codex;
pub mod copilot;
pub mod cursor;
pub mod gemini;
pub mod opencode;

use std::path::{Path, PathBuf};

use jiff::Timestamp;

/// Prompts are clipped to this many characters, same as Claude's transcripts
/// (ai_sessions.rs's `PROMPT_CHARS`).
pub(crate) const PROMPT_CHARS: usize = 120;

/// Clip `text` to `PROMPT_CHARS` characters, trimmed; `None` for empty text.
pub(crate) fn clip(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut it = text.chars();
    let head: String = it.by_ref().take(PROMPT_CHARS).collect();
    Some(if it.next().is_some() {
        format!("{head}\u{2026}")
    } else {
        head
    })
}

/// One record of session evidence a format's reader yields, in the shape
/// `ai_sessions.rs` folds into a segment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rec {
    pub ts: Timestamp,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    /// The user's turn text, already clipped (see `clip`).
    pub prompt: Option<String>,
    /// Files this record touched, relative to `cwd` when the format knows
    /// how to make them so.
    pub paths: Vec<String>,
    /// This record wrote a file (a tool call or edit, not just talk).
    pub write: bool,
    /// The tool's title for the conversation, when this record names or
    /// renames it.
    pub title: Option<String>,
    pub session_id: String,
}

/// One session source a format's `scan` found under a root: a file, or an
/// entry point into a shared database that many sessions live in (the
/// records it yields carry their own `session_id`; the provider groups by
/// that, not by `Source`).
#[derive(Debug, Clone)]
pub struct Source {
    pub path: PathBuf,
    pub session_id: String,
    /// A cwd the source implies before any record says otherwise (Aider:
    /// the candidate root it was scanned at; Copilot: `workspace.yaml`).
    pub cwd_hint: Option<String>,
}

/// A local coding tool's session store.
pub trait SessionFormat: Send {
    /// `"claude" | "codex" | "gemini" | "copilot" | "aider" | "cline" |
    /// "opencode" | "cursor"`.
    fn name(&self) -> &'static str;
    /// Default directories under `home` that hold this format's sessions,
    /// `~` already expanded, only the ones that exist. Empty when the
    /// format has no fixed root and is instead scanned at each candidate
    /// cwd (Aider): the provider then calls `scan(candidate)` per
    /// candidate directly.
    fn roots(&self, home: &Path) -> Vec<PathBuf>;
    /// Session sources under `root`.
    fn scan(&self, root: &Path) -> Vec<Source>;
    /// Records new since `cursor` (a byte offset for a file, a row id or
    /// millisecond timestamp for a database — opaque to the caller), and
    /// the cursor to pass back in next time.
    fn read(&self, source: &Source, cursor: u64) -> (Vec<Rec>, u64);
}

pub fn all() -> Vec<Box<dyn SessionFormat>> {
    vec![
        Box::new(claude::Claude),
        Box::new(codex::Codex),
        Box::new(gemini::Gemini),
        Box::new(copilot::Copilot),
        Box::new(aider::Aider),
        Box::new(cline::Cline),
        Box::new(opencode::OpenCode),
        Box::new(cursor::Cursor),
    ]
}

pub fn by_name(name: &str) -> Option<Box<dyn SessionFormat>> {
    all().into_iter().find(|f| f.name() == name)
}

/// Complete lines from byte `from` in a file, and the offset just past the
/// last newline. Shared by every line-delimited format's incremental
/// `read`; the cursor a format hands back is always newline-aligned, so
/// (unlike `ai_sessions.rs`'s head/tail-skipping `first_sight`) there is
/// never a fragment to second-guess.
pub(crate) fn read_lines_from(path: &Path, from: u64) -> (Vec<String>, u64) {
    use std::io::{Read, Seek, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return (Vec::new(), from);
    };
    if file.seek(SeekFrom::Start(from)).is_err() {
        return (Vec::new(), from);
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return (Vec::new(), from);
    }
    let Some(last_nl) = bytes.iter().rposition(|&b| b == b'\n') else {
        return (Vec::new(), from);
    };
    let text = String::from_utf8_lossy(&bytes[..last_nl]);
    (
        text.lines().map(str::to_owned).collect(),
        from + last_nl as u64 + 1,
    )
}

/// Files under `root`, at most `max_depth` directories deep, for which
/// `want` returns true.
pub(crate) fn walk_files(
    root: &Path,
    max_depth: usize,
    want: impl Fn(&Path) -> bool,
) -> Vec<PathBuf> {
    fn rec(dir: &Path, depth: usize, want: &dyn Fn(&Path) -> bool, out: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if depth > 0 {
                    rec(&path, depth - 1, want, out);
                }
            } else if want(&path) {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    rec(root, max_depth, &want, &mut out);
    out
}

/// Copy a (possibly live) sqlite file to a throwaway path under the temp
/// dir so a format can query it read-only without racing the tool that
/// owns it; the caller removes the copy when done. `None` when the source
/// does not exist or can't be copied — every DB-backed format treats that
/// as "nothing new" rather than an error.
pub(crate) fn copy_db(path: &Path, tag: &str) -> Option<PathBuf> {
    if !path.is_file() {
        return None;
    }
    let tmp = std::env::temp_dir().join(format!(
        "chronicle-{tag}-{}-{}.db",
        std::process::id(),
        Timestamp::now().as_microsecond()
    ));
    std::fs::copy(path, &tmp).ok()?;
    Some(tmp)
}
