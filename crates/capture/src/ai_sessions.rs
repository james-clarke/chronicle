//! AI coding session watcher (m22): tails Claude Code transcripts under
//! `~/.claude/projects/<project>/<session>.jsonl` and emits `ai_session`
//! spans, refreshed as lines land. A transcript is one span until its lines
//! pause for longer than `SESSION_GAP`; the next line then opens a new span
//! (`<session>#2`, `#3`, …) so a chat left open all day does not read as a
//! nine-hour session. Read-only, incremental (bytes past the last seen
//! length), never recurses (subagent transcripts live under
//! `<session>/subagents/`), and only ever keeps each span's first prompt
//! clipped — never the conversation. Per span it also keeps the minutes the
//! transcript was written to, the minutes a prompt was typed, and the
//! titles the tool gave the conversation (`ai-title` records: the text it
//! puts in the terminal title), so the anchor extractor can attach a
//! terminal span to the session that owned its title (m32 chunk 2).

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chronicle_core::types::{ActivityEvent, ActivityKind, CaptureEvent};
use crossbeam_channel::Sender;
use jiff::Timestamp;

use crate::{BoxError, FocusProvider};

const POLL: Duration = Duration::from_secs(20);
/// Transcripts untouched for longer than this at boot are history, not
/// sessions to resume tracking.
const BOOT_WINDOW: Duration = Duration::from_secs(24 * 60 * 60);
/// Head lines scanned for the start line and first prompt on first sight.
const HEAD_LINES: usize = 200;
const TAIL_BYTES: u64 = 64 * 1024;
const PROMPT_CHARS: usize = 120;
/// The session's last reply is kept this long.
const ASSISTANT_CHARS: usize = 300;
/// Prompts and touched paths kept per segment for the span anchors.
const MAX_PROMPTS: usize = 12;
const MAX_PATHS: usize = 40;
/// Write minutes kept per segment (the newest); a segment ends at a 30-min
/// pause, so this covers four hours of continuous writing.
const MAX_WRITES: usize = 240;
/// A pause between transcript lines longer than this ends the span.
const SESSION_GAP: Duration = Duration::from_secs(30 * 60);
/// Distinct conversation titles kept per segment (a session is renamed
/// a few times at most).
const MAX_TITLES: usize = 8;

pub struct AiSessionProvider {
    dirs: Vec<PathBuf>,
    files: HashMap<PathBuf, FileState>,
}

#[derive(Debug, Default)]
struct FileState {
    /// Bytes consumed (up to and including the last complete line).
    len: u64,
    ident: Option<Ident>,
    /// Spans in order; only the last one grows.
    segments: Vec<Segment>,
    /// The next line joins the open segment whatever the gap: first sight
    /// skips the middle of a big transcript, and that skip is not a pause.
    bridge: bool,
    /// The latest title record seen; a segment opened later starts with it.
    title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Ident {
    repo: String,
    session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Segment {
    ts: Timestamp,
    end: Timestamp,
    /// Branch on the segment's first line.
    branch: String,
    prompt: Option<String>,
    /// Every typed prompt (clipped), oldest first, up to `MAX_PROMPTS`.
    prompts: Vec<String>,
    /// Files the tool read or edited, relative to the cwd, first-seen
    /// order, up to `MAX_PATHS`.
    paths: Vec<String>,
    /// When the transcript was written to, one entry per distinct minute
    /// (UTC ms, oldest first), the newest `MAX_WRITES` kept. The anchor
    /// extractor attaches a terminal span to the session that wrote most
    /// recently before or during it.
    writes: Vec<i64>,
    /// Minutes a prompt was typed (a `user` record that is not a tool
    /// result), same shape and cap as `writes`. A prompt is the person at
    /// the keyboard; a write is the tool at work.
    prompt_minutes: Vec<i64>,
    /// Titles the tool gave the conversation while this segment was open,
    /// oldest first, distinct, up to `MAX_TITLES`.
    titles: Vec<String>,
    /// `(minute, index into paths)` per file touch, oldest first, one per
    /// path and minute, the newest `MAX_WRITES` kept: which files the tool
    /// was on around a given moment.
    touches: Vec<(i64, usize)>,
    /// The newest assistant text seen (clipped): how the session ended, for
    /// narratives (m32 chunk 5).
    last_assistant: Option<String>,
    /// `end` as of the last emitted event.
    sent: Option<Timestamp>,
}

impl Segment {
    fn detail(&self) -> Option<String> {
        if self.prompts.is_empty() && self.paths.is_empty() && self.writes.is_empty() {
            return None;
        }
        let mut v = serde_json::json!({
            "prompts": self.prompts,
            "paths": self.paths,
            "writes": self.writes,
            "prompt_minutes": self.prompt_minutes,
            "titles": self.titles,
            "touches": self.touches,
        });
        if let Some(a) = &self.last_assistant {
            v["last_assistant"] = serde_json::Value::String(a.clone());
        }
        Some(v.to_string())
    }

    fn push_title(&mut self, title: &str) {
        if self.titles.iter().any(|t| t == title) {
            return;
        }
        if self.titles.len() >= MAX_TITLES {
            self.titles.remove(0);
        }
        self.titles.push(title.to_owned());
    }
}

impl FileState {
    fn ext_id(&self, index: usize) -> Option<String> {
        let id = &self.ident.as_ref()?.session_id;
        Some(if index == 0 {
            id.clone()
        } else {
            format!("{id}#{}", index + 1)
        })
    }
}

/// One transcript line that carries session evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Line {
    ts: Timestamp,
    cwd: String,
    branch: String,
    session_id: String,
    prompt: Option<String>,
    /// An `assistant` record's last text block (clipped).
    assistant: Option<String>,
    /// File paths in this line's tool calls, relative to `cwd`.
    paths: Vec<String>,
    /// A `user` record that is not a tool result: something was typed (or a
    /// command run), whether or not it survived as a prompt.
    typed: bool,
}

/// One transcript record the tracker acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Rec {
    Line(Line),
    /// An `ai-title` record: the conversation's current title.
    Title(String),
}

impl AiSessionProvider {
    pub fn new(dirs: &[PathBuf]) -> Self {
        Self {
            dirs: dirs.iter().filter(|d| d.is_dir()).cloned().collect(),
            files: HashMap::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.dirs.is_empty()
    }

    /// One poll: pick up new/grown transcripts, return the sessions whose
    /// end moved. `now` bounds the boot window.
    fn scan(&mut self, now: SystemTime) -> Vec<ActivityEvent> {
        let mut out = Vec::new();
        for path in list_transcripts(&self.dirs) {
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let len = meta.len();
            let state = match self.files.get_mut(&path) {
                Some(s) => s,
                None => {
                    let fresh = meta
                        .modified()
                        .ok()
                        .and_then(|m| now.duration_since(m).ok())
                        .is_some_and(|age| age <= BOOT_WINDOW);
                    if !fresh {
                        continue;
                    }
                    let state = first_sight(&path, len);
                    self.files.entry(path.clone()).or_insert(state)
                }
            };
            if len < state.len {
                // Truncated or rewritten: start over.
                *state = first_sight(&path, len);
            } else if len > state.len {
                let (lines, consumed) = read_lines_from(&path, state.len);
                state.len = consumed;
                for rec in lines.iter().filter_map(|l| parse(l)) {
                    absorb(state, rec);
                }
            }
            let Some(ident) = state.ident.clone() else {
                continue;
            };
            for i in 0..state.segments.len() {
                let ext_id = state.ext_id(i);
                let seg = &mut state.segments[i];
                if seg.sent == Some(seg.end) {
                    continue;
                }
                seg.sent = Some(seg.end);
                out.push(ActivityEvent {
                    ts: seg.ts,
                    end_ts: Some(seg.end),
                    repo: ident.repo.clone(),
                    branch: seg.branch.clone(),
                    kind: ActivityKind::AiSession,
                    ext_id,
                    summary: seg.prompt.clone(),
                    detail: seg.detail(),
                });
            }
        }
        out
    }
}

/// Every session in the transcripts under `dirs` modified at or after
/// `since`, read in full (no head/tail skip), one event per segment. For
/// `chronicle backfill-sessions`: rows captured before the collector kept
/// prompt minutes and titles get them from the transcript. Transcripts are
/// grouped by session id so the caller can replace a session's rows whole.
pub fn replay_transcripts(
    dirs: &[PathBuf],
    since: SystemTime,
) -> Vec<(String, Vec<ActivityEvent>)> {
    let mut out = Vec::new();
    for path in list_transcripts(dirs) {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.modified().is_ok_and(|m| m < since) {
            continue;
        }
        let Ok(file) = File::open(&path) else {
            continue;
        };
        let mut state = FileState::default();
        for line in BufReader::new(file).lines().map_while(Result::ok) {
            if let Some(rec) = parse(&line) {
                absorb(&mut state, rec);
            }
        }
        let Some(ident) = state.ident.clone() else {
            continue;
        };
        let events = (0..state.segments.len())
            .map(|i| {
                let seg = &state.segments[i];
                ActivityEvent {
                    ts: seg.ts,
                    end_ts: Some(seg.end),
                    repo: ident.repo.clone(),
                    branch: seg.branch.clone(),
                    kind: ActivityKind::AiSession,
                    ext_id: state.ext_id(i),
                    summary: seg.prompt.clone(),
                    detail: seg.detail(),
                }
            })
            .collect();
        out.push((ident.session_id, events));
    }
    out
}

impl FocusProvider for AiSessionProvider {
    fn run(mut self, tx: Sender<CaptureEvent>) -> Result<(), BoxError> {
        crate::poll_loop(&tx, POLL, move || self.scan(SystemTime::now()))
    }
}

/// `<dir>/<project>/*.jsonl`, one level down only.
fn list_transcripts(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(projects) = std::fs::read_dir(dir) else {
            continue;
        };
        for project in projects.flatten() {
            let Ok(files) = std::fs::read_dir(project.path()) else {
                continue;
            };
            out.extend(
                files
                    .flatten()
                    .map(|f| f.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "jsonl") && p.is_file()),
            );
        }
    }
    out
}

/// Head for the start line + first prompt, tail for the latest timestamp;
/// the whole middle is skipped (transcripts run to hundreds of MB).
fn first_sight(path: &Path, len: u64) -> FileState {
    let mut state = FileState::default();
    let Ok(file) = File::open(path) else {
        return state;
    };
    let mut reader = BufReader::new(file);
    let mut buf = String::new();
    for _ in 0..HEAD_LINES {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if let Some(rec) = parse(&buf) {
            absorb(&mut state, rec);
            if state.segments.first().is_some_and(|s| s.prompt.is_some()) {
                break;
            }
        }
    }
    let tail_from = len.saturating_sub(TAIL_BYTES);
    state.bridge = tail_from > 0;
    let (lines, consumed) = read_lines_from(path, tail_from);
    for rec in lines.iter().filter_map(|l| parse(l)) {
        absorb(&mut state, rec);
    }
    state.len = consumed.max(state.len);
    state
}

/// Complete lines from byte `from`; returns them with the offset just past
/// the last newline (a partial trailing line is re-read next poll). When
/// `from` lands mid-line the first fragment is dropped.
fn read_lines_from(path: &Path, from: u64) -> (Vec<String>, u64) {
    let Ok(mut file) = File::open(path) else {
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
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    if from > 0 && !lines.is_empty() {
        // Only trust the fragment when it looks like a whole line.
        if !lines[0].starts_with('{') {
            lines.remove(0);
        }
    }
    (lines, from + last_nl as u64 + 1)
}

fn absorb(state: &mut FileState, rec: Rec) {
    let line = match rec {
        Rec::Line(line) => line,
        Rec::Title(title) => {
            if let Some(seg) = state.segments.last_mut() {
                seg.push_title(&title);
            }
            state.title = Some(title);
            return;
        }
    };
    let line = &line;
    if state.ident.is_none() {
        state.ident = Some(Ident {
            repo: repo_name(&line.cwd),
            session_id: line.session_id.clone(),
        });
    }
    let bridge = std::mem::take(&mut state.bridge);
    let gap = SESSION_GAP.as_millis() as i64;
    match state.segments.last_mut() {
        Some(seg) if bridge || line.ts.as_millisecond() - seg.end.as_millisecond() <= gap => {
            if seg.ts > line.ts {
                seg.ts = line.ts;
            }
            if seg.end < line.ts {
                seg.end = line.ts;
            }
            if seg.prompt.is_none() {
                seg.prompt = line.prompt.clone();
            }
            seg.absorb_detail(line);
        }
        _ => {
            let mut seg = Segment {
                ts: line.ts,
                end: line.ts,
                branch: line.branch.clone(),
                prompt: line.prompt.clone(),
                prompts: Vec::new(),
                paths: Vec::new(),
                writes: Vec::new(),
                prompt_minutes: Vec::new(),
                titles: state.title.iter().cloned().collect(),
                touches: Vec::new(),
                last_assistant: None,
                sent: None,
            };
            seg.absorb_detail(line);
            state.segments.push(seg);
        }
    }
}

impl Segment {
    fn absorb_detail(&mut self, line: &Line) {
        let minute = line.ts.as_millisecond() / 60_000 * 60_000;
        if self.writes.last() != Some(&minute) {
            self.writes.push(minute);
            if self.writes.len() > MAX_WRITES {
                self.writes.remove(0);
            }
        }
        if line.typed && self.prompt_minutes.last() != Some(&minute) {
            self.prompt_minutes.push(minute);
            if self.prompt_minutes.len() > MAX_WRITES {
                self.prompt_minutes.remove(0);
            }
        }
        if let Some(p) = &line.prompt
            && self.prompts.len() < MAX_PROMPTS
            && self.prompts.last() != Some(p)
        {
            self.prompts.push(p.clone());
        }
        if line.assistant.is_some() {
            self.last_assistant = line.assistant.clone();
        }
        for p in &line.paths {
            let idx = match self.paths.iter().position(|q| q == p) {
                Some(i) => i,
                None if self.paths.len() < MAX_PATHS => {
                    self.paths.push(p.clone());
                    self.paths.len() - 1
                }
                None => continue,
            };
            if self.touches.contains(&(minute, idx)) {
                continue;
            }
            self.touches.push((minute, idx));
            if self.touches.len() > MAX_WRITES {
                self.touches.remove(0);
            }
        }
    }
}

/// The cwd's basename as the session's place; a session started in the
/// home directory (or the filesystem root) has none.
fn repo_name(cwd: &str) -> String {
    let path = Path::new(cwd);
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    if home.as_deref() == Some(path) {
        return String::new();
    }
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// A record the tracker acts on: a timestamped `user`/`assistant` line, or
/// an `ai-title` record (the title the tool shows in the terminal; it
/// carries no timestamp and names the open segment). Sidechain (subagent)
/// lines and other bookkeeping (`mode`, `attachment`, …) are nothing.
fn parse(raw: &str) -> Option<Rec> {
    let v: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    let kind = v.get("type")?.as_str()?;
    if kind == "ai-title" {
        let title = v.get("aiTitle")?.as_str()?.trim();
        return (!title.is_empty()).then(|| Rec::Title(title.to_owned()));
    }
    parse_line_value(&v, kind).map(Rec::Line)
}

#[cfg(test)]
fn parse_line(raw: &str) -> Option<Line> {
    match parse(raw)? {
        Rec::Line(line) => Some(line),
        Rec::Title(_) => None,
    }
}

fn parse_line_value(v: &serde_json::Value, kind: &str) -> Option<Line> {
    if kind != "user" && kind != "assistant" {
        return None;
    }
    if v.get("isSidechain").and_then(|s| s.as_bool()) == Some(true) {
        return None;
    }
    let ts: Timestamp = v.get("timestamp")?.as_str()?.parse().ok()?;
    let cwd = v.get("cwd")?.as_str()?.to_owned();
    let session_id = v.get("sessionId")?.as_str()?.to_owned();
    let branch = v
        .get("gitBranch")
        .and_then(|b| b.as_str())
        .unwrap_or_default()
        .to_owned();
    let content = v.get("message").and_then(|m| m.get("content"));
    let prompt = (kind == "user").then(|| prompt_text(content?)).flatten();
    let assistant = (kind == "assistant")
        .then(|| assistant_text(content?))
        .flatten();
    // Tool results come back as `user` records too; injected context
    // (`isMeta`: skill files, local command output) is not typing either.
    let typed = kind == "user"
        && v.get("isMeta").and_then(|m| m.as_bool()) != Some(true)
        && !content.is_some_and(is_tool_result);
    let paths = (kind == "assistant")
        .then(|| {
            v.get("message")
                .and_then(|m| m.get("content"))
                .map(|c| tool_paths(c, &cwd))
        })
        .flatten()
        .unwrap_or_default();
    Some(Line {
        ts,
        cwd,
        branch,
        session_id,
        prompt,
        assistant,
        paths,
        typed,
    })
}

/// A `user` record whose content is (only ever) `tool_result` blocks.
fn is_tool_result(content: &serde_json::Value) -> bool {
    content.as_array().is_some_and(|blocks| {
        blocks
            .iter()
            .any(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_result"))
    })
}

/// File paths named by the line's `tool_use` blocks (`file_path`, `path`,
/// `notebook_path`), made relative to `cwd` when under it. Tool names are
/// not checked: any tool that takes a path touched that file.
fn tool_paths(content: &serde_json::Value, cwd: &str) -> Vec<String> {
    const KEYS: [&str; 3] = ["file_path", "path", "notebook_path"];
    let Some(blocks) = content.as_array() else {
        return Vec::new();
    };
    let prefix = format!("{}/", cwd.trim_end_matches('/'));
    blocks
        .iter()
        .filter(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
        .filter_map(|b| b.get("input"))
        .flat_map(|input| {
            KEYS.iter()
                .filter_map(move |k| input.get(*k).and_then(|p| p.as_str()))
        })
        .filter(|p| !p.is_empty())
        .map(|p| p.strip_prefix(&prefix).unwrap_or(p).to_owned())
        .collect()
}

/// A typed prompt: a string or the first text block. Tool results, tagged
/// system/command payloads (`<…>`) and injected skill files are not prompts.
fn prompt_text(content: &serde_json::Value) -> Option<String> {
    let text = match content {
        serde_json::Value::String(s) => s.as_str(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))?
            .get("text")?
            .as_str()?,
        _ => return None,
    };
    let text = text.trim();
    if text.is_empty() || text.starts_with('<') || text.starts_with("Base directory for this skill")
    {
        return None;
    }
    let mut it = text.chars();
    let head: String = it.by_ref().take(PROMPT_CHARS).collect();
    Some(if it.next().is_some() {
        head + "\u{2026}"
    } else {
        head
    })
}

/// The last text block of an assistant record, clipped like a prompt: the
/// reply the session ended on once the record is the newest.
fn assistant_text(content: &serde_json::Value) -> Option<String> {
    let text = match content {
        serde_json::Value::String(s) => s.as_str(),
        serde_json::Value::Array(blocks) => blocks
            .iter()
            .rev()
            .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))?
            .get("text")?
            .as_str()?,
        _ => return None,
    };
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut it = text.chars();
    let head: String = it.by_ref().take(ASSISTANT_CHARS).collect();
    Some(if it.next().is_some() {
        head + "\u{2026}"
    } else {
        head
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const USER: &str = r#"{"type":"user","cwd":"/home/u/dev/app","gitBranch":"ABC-1-x","sessionId":"s1","timestamp":"2026-09-02T19:00:00.000Z","isSidechain":false,"message":{"role":"user","content":"fix the flaky test"}}"#;
    const MODE: &str = r#"{"type":"mode","sessionId":"s1"}"#;
    const SIDE: &str = r#"{"type":"user","cwd":"/home/u/dev/app","gitBranch":"ABC-1-x","sessionId":"s1","timestamp":"2026-09-02T19:30:00.000Z","isSidechain":true,"message":{"role":"user","content":"sub"}}"#;
    const ASSISTANT: &str = r#"{"type":"assistant","cwd":"/home/u/dev/app","gitBranch":"ABC-1-x","sessionId":"s1","timestamp":"2026-09-02T19:05:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"ok"},{"type":"tool_use","name":"Edit","input":{"file_path":"/home/u/dev/app/src/lib.rs","old_string":"a"}}]}}"#;
    const RESULT: &str = r#"{"type":"user","cwd":"/home/u/dev/app","gitBranch":"ABC-1-x","sessionId":"s1","timestamp":"2026-09-02T19:05:30.000Z","isSidechain":false,"message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"done"}]}}"#;
    const TITLE: &str = r#"{"type":"ai-title","aiTitle":"Flaky test fix","sessionId":"s1"}"#;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    #[test]
    fn parses_only_timestamped_main_lines() {
        let l = parse_line(USER).unwrap();
        assert_eq!(l.cwd, "/home/u/dev/app");
        assert_eq!(l.branch, "ABC-1-x");
        assert_eq!(l.prompt.as_deref(), Some("fix the flaky test"));
        assert!(parse_line(MODE).is_none());
        assert!(
            parse_line(SIDE).is_none(),
            "subagent lines are not evidence"
        );
        assert!(parse_line(ASSISTANT).unwrap().prompt.is_none());
        let tagged = USER.replace("fix the flaky test", "<command-name>/foo</command-name>");
        assert!(parse_line(&tagged).unwrap().prompt.is_none());
        let skill = USER.replace("fix the flaky test", "Base directory for this skill: /x");
        assert!(parse_line(&skill).unwrap().prompt.is_none());
        let long = USER.replace("fix the flaky test", &"x".repeat(200));
        assert_eq!(
            parse_line(&long).unwrap().prompt.unwrap().chars().count(),
            PROMPT_CHARS + 1
        );
    }

    // m32 chunk 2: typing is any user record that is not a tool result or
    // injected context; the title record names the conversation.
    #[test]
    fn typed_and_titled() {
        assert!(parse_line(USER).unwrap().typed);
        let tagged = USER.replace("fix the flaky test", "<command-name>/foo</command-name>");
        assert!(
            parse_line(&tagged).unwrap().typed,
            "a slash command is typed"
        );
        assert!(!parse_line(RESULT).unwrap().typed, "a tool result is not");
        assert!(!parse_line(ASSISTANT).unwrap().typed);
        let meta = USER.replace(
            r#""isSidechain":false"#,
            r#""isSidechain":false,"isMeta":true"#,
        );
        assert!(!parse_line(&meta).unwrap().typed, "injected context is not");
        assert_eq!(parse(TITLE), Some(Rec::Title("Flaky test fix".into())));
        assert!(parse(r#"{"type":"ai-title","aiTitle":"  ","sessionId":"s1"}"#).is_none());

        let mut state = FileState::default();
        absorb(&mut state, parse(TITLE).unwrap());
        assert!(state.segments.is_empty(), "a title before any line waits");
        absorb(&mut state, parse(USER).unwrap());
        absorb(&mut state, parse(ASSISTANT).unwrap());
        absorb(&mut state, parse(RESULT).unwrap());
        absorb(&mut state, parse(TITLE).unwrap());
        let renamed = TITLE.replace("Flaky test fix", "Docs");
        absorb(&mut state, parse(&renamed).unwrap());
        let seg = &state.segments[0];
        let m = |s: &str| ts(s).as_millisecond();
        assert_eq!(seg.prompt_minutes, [m("2026-09-02T19:00:00Z")]);
        assert_eq!(
            seg.writes,
            [m("2026-09-02T19:00:00Z"), m("2026-09-02T19:05:00Z")]
        );
        assert_eq!(seg.titles, ["Flaky test fix", "Docs"]);
        assert_eq!(seg.touches, [(m("2026-09-02T19:05:00Z"), 0)]);
        // The same file in a later minute is a new touch; a second file in
        // the same minute too.
        absorb(
            &mut state,
            parse(&ASSISTANT.replace("19:05:00", "19:07:00")).unwrap(),
        );
        absorb(
            &mut state,
            parse(
                &ASSISTANT
                    .replace("19:05:00", "19:07:10")
                    .replace("lib.rs", "main.rs"),
            )
            .unwrap(),
        );
        assert_eq!(state.segments[0].paths, ["src/lib.rs", "src/main.rs"]);
        assert_eq!(
            state.segments[0].touches,
            [
                (m("2026-09-02T19:05:00Z"), 0),
                (m("2026-09-02T19:07:00Z"), 0),
                (m("2026-09-02T19:07:00Z"), 1)
            ]
        );
        // A segment opened after a gap starts with the latest title.
        let later = USER.replace("19:00:00", "20:15:00");
        absorb(&mut state, parse(&later).unwrap());
        assert_eq!(state.segments[1].titles, ["Docs"]);
        assert_eq!(
            state.segments[1].prompt_minutes,
            [m("2026-09-02T20:15:00Z")]
        );
    }

    #[test]
    fn replay_reads_whole_transcripts() {
        let root = std::env::temp_dir().join(format!("chronicle-ai-replay-{}", std::process::id()));
        let project = root.join("p");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&project).unwrap();
        // A middle bigger than the tail window, with the title and a prompt
        // inside it: first sight would miss both, a replay does not.
        let mut body = format!("{USER}\n{TITLE}\n");
        let filler = ASSISTANT.replace("19:05:00", "19:10:00");
        for _ in 0..HEAD_LINES + 1_500 {
            body.push_str(&filler);
            body.push('\n');
        }
        body.push_str(&USER.replace("19:00:00", "19:20:00"));
        body.push('\n');
        body.push_str(&ASSISTANT.replace("19:05:00", "21:00:00"));
        body.push('\n');
        std::fs::write(project.join("s1.jsonl"), &body).unwrap();

        let future = SystemTime::now() + Duration::from_secs(3_600);
        assert!(replay_transcripts(std::slice::from_ref(&root), future).is_empty());
        let got = replay_transcripts(std::slice::from_ref(&root), SystemTime::UNIX_EPOCH);
        assert_eq!(got.len(), 1, "{got:?}");
        let (sid, events) = &got[0];
        assert_eq!(sid, "s1");
        assert_eq!(events.len(), 2, "{events:?}");
        let d: serde_json::Value =
            serde_json::from_str(events[0].detail.as_deref().unwrap()).unwrap();
        assert_eq!(d["titles"], serde_json::json!(["Flaky test fix"]));
        assert_eq!(d["prompt_minutes"].as_array().unwrap().len(), 2);
        assert_eq!(events[1].ext_id.as_deref(), Some("s1#2"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn writes_are_bucketed_to_the_minute() {
        let mut state = FileState::default();
        absorb(&mut state, parse(USER).unwrap());
        let burst = USER.replace("19:00:00.000", "19:00:40.000");
        absorb(&mut state, parse(&burst).unwrap());
        let later = USER.replace("19:00:00.000", "19:01:30.000");
        absorb(&mut state, parse(&later).unwrap());
        let seg = &state.segments[0];
        let m = |s: &str| ts(s).as_millisecond();
        assert_eq!(
            seg.writes,
            [m("2026-09-02T19:00:00Z"), m("2026-09-02T19:01:00Z")]
        );
        let d: serde_json::Value = serde_json::from_str(&seg.detail().unwrap()).unwrap();
        assert_eq!(d["writes"].as_array().unwrap().len(), 2);
    }

    #[test]
    fn scan_tracks_growth_and_skips_subagent_dirs() {
        let root = std::env::temp_dir().join(format!("chronicle-ai-{}", std::process::id()));
        let project = root.join("-home-u-dev-app");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(project.join("s1").join("subagents")).unwrap();
        let path = project.join("s1.jsonl");
        std::fs::write(&path, format!("{MODE}\n{USER}\n{SIDE}\n{ASSISTANT}\n")).unwrap();
        std::fs::write(
            project.join("s1").join("subagents").join("agent-x.jsonl"),
            SIDE,
        )
        .unwrap();

        let mut p = AiSessionProvider::new(std::slice::from_ref(&root));
        let now = SystemTime::now();
        let got = p.scan(now);
        assert_eq!(got.len(), 1, "{got:?}");
        let e = &got[0];
        assert_eq!(e.kind, ActivityKind::AiSession);
        assert_eq!(e.ts, ts("2026-09-02T19:00:00Z"));
        assert_eq!(e.end_ts, Some(ts("2026-09-02T19:05:00Z")));
        assert_eq!((e.repo.as_str(), e.branch.as_str()), ("app", "ABC-1-x"));
        assert_eq!(e.ext_id.as_deref(), Some("s1"));
        assert_eq!(e.summary.as_deref(), Some("fix the flaky test"));
        assert_eq!(
            e.detail.as_deref(),
            Some(
                r#"{"last_assistant":"ok","paths":["src/lib.rs"],"prompt_minutes":[1788375600000],"prompts":["fix the flaky test"],"titles":[],"touches":[[1788375900000,0]],"writes":[1788375600000,1788375900000]}"#
            )
        );
        assert!(p.scan(now).is_empty(), "nothing moved");

        // Growth: only the new bytes are read; a partial line waits.
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        let later = ASSISTANT.replace("19:05:00", "19:30:00");
        write!(f, "{later}\n{{\"type\":\"assist").unwrap();
        let got = p.scan(now);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].end_ts, Some(ts("2026-09-02T19:30:00Z")));
        assert_eq!(got[0].ext_id.as_deref(), Some("s1"));
        writeln!(f, "ant\"}}").unwrap();
        assert!(p.scan(now).is_empty());

        // A pause over SESSION_GAP opens a second span under its own ext_id,
        // named by its first prompt and branch; the first span stands.
        let resumed = USER
            .replace("19:00:00", "20:15:00")
            .replace("fix the flaky test", "now the docs")
            .replace("ABC-1-x", "ABC-2-y");
        writeln!(f, "{resumed}").unwrap();
        let got = p.scan(now);
        assert_eq!(got.len(), 1, "{got:?}");
        assert_eq!(got[0].ts, ts("2026-09-02T20:15:00Z"));
        assert_eq!(got[0].end_ts, Some(ts("2026-09-02T20:15:00Z")));
        assert_eq!(got[0].ext_id.as_deref(), Some("s1#2"));
        assert_eq!(got[0].summary.as_deref(), Some("now the docs"));
        assert_eq!(got[0].branch, "ABC-2-y");
        writeln!(f, "{}", ASSISTANT.replace("19:05:00", "20:20:00")).unwrap();
        let got = p.scan(now);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].ext_id.as_deref(), Some("s1#2"));
        assert_eq!(got[0].end_ts, Some(ts("2026-09-02T20:20:00Z")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn first_sight_uses_head_and_tail_only() {
        let root = std::env::temp_dir().join(format!("chronicle-ai-big-{}", std::process::id()));
        let project = root.join("p");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join("s1.jsonl");
        let filler = ASSISTANT.replace("19:05:00", "19:10:00");
        let mut body = format!("{USER}\n");
        for _ in 0..2_000 {
            body.push_str(&filler);
            body.push('\n');
        }
        body.push_str(&ASSISTANT.replace("19:05:00", "21:00:00"));
        body.push('\n');
        std::fs::write(&path, &body).unwrap();
        assert!(body.len() as u64 > TAIL_BYTES);
        let state = first_sight(&path, body.len() as u64);
        // The unread middle bridges head and tail; the 19:10 → 21:00 pause
        // inside the tail is a real gap.
        assert_eq!(state.segments.len(), 2, "{:?}", state.segments);
        assert_eq!(state.segments[0].ts, ts("2026-09-02T19:00:00Z"));
        assert_eq!(state.segments[0].end, ts("2026-09-02T19:10:00Z"));
        assert_eq!(state.segments[1].ts, ts("2026-09-02T21:00:00Z"));
        assert_eq!(state.len, body.len() as u64);
        assert_eq!(state.ext_id(1).as_deref(), Some("s1#2"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn first_sight_bridges_the_skipped_middle() {
        let root = std::env::temp_dir().join(format!("chronicle-ai-bridge-{}", std::process::id()));
        let project = root.join("p");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&project).unwrap();
        let path = project.join("s1.jsonl");
        // Head at 19:00, a big unread middle, tail lines 22:00–22:05: without
        // the bridge the head→tail jump would look like a three-hour pause.
        let mut body = format!("{USER}\n");
        let filler = ASSISTANT.replace("19:05:00", "19:01:00");
        for _ in 0..HEAD_LINES + 1_500 {
            body.push_str(&filler);
            body.push('\n');
        }
        let tail_len = body.len();
        body.push_str(&ASSISTANT.replace("19:05:00", "22:00:00"));
        body.push('\n');
        body.push_str(&ASSISTANT.replace("19:05:00", "22:05:00"));
        body.push('\n');
        std::fs::write(&path, &body).unwrap();
        assert!(
            tail_len as u64 > TAIL_BYTES + 4096,
            "middle must exceed the tail window"
        );
        let state = first_sight(&path, body.len() as u64);
        assert_eq!(state.segments.len(), 2, "{:?}", state.segments);
        assert_eq!(state.segments[0].end, ts("2026-09-02T19:01:00Z"));
        assert_eq!(state.segments[1].ts, ts("2026-09-02T22:00:00Z"));
        assert_eq!(state.segments[1].end, ts("2026-09-02T22:05:00Z"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
