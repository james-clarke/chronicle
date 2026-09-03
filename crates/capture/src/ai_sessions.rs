//! AI coding session watcher (m22): tails Claude Code transcripts under
//! `~/.claude/projects/<project>/<session>.jsonl` and emits `ai_session`
//! spans, refreshed as lines land. A transcript is one span until its lines
//! pause for longer than `SESSION_GAP`; the next line then opens a new span
//! (`<session>#2`, `#3`, …) so a chat left open all day does not read as a
//! nine-hour session. Read-only, incremental (bytes past the last seen
//! length), never recurses (subagent transcripts live under
//! `<session>/subagents/`), and only ever keeps each span's first prompt
//! clipped — never the conversation.

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
/// A pause between transcript lines longer than this ends the span.
const SESSION_GAP: Duration = Duration::from_secs(30 * 60);

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
    /// `end` as of the last emitted event.
    sent: Option<Timestamp>,
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
                for line in lines.iter().filter_map(|l| parse_line(l)) {
                    absorb(state, &line);
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
                });
            }
        }
        out
    }
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
        if let Some(line) = parse_line(&buf) {
            absorb(&mut state, &line);
            if state.segments.first().is_some_and(|s| s.prompt.is_some()) {
                break;
            }
        }
    }
    let tail_from = len.saturating_sub(TAIL_BYTES);
    state.bridge = tail_from > 0;
    let (lines, consumed) = read_lines_from(path, tail_from);
    for line in lines.iter().filter_map(|l| parse_line(l)) {
        absorb(&mut state, &line);
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

fn absorb(state: &mut FileState, line: &Line) {
    if state.ident.is_none() {
        state.ident = Some(Ident {
            repo: Path::new(&line.cwd)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
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
        }
        _ => state.segments.push(Segment {
            ts: line.ts,
            end: line.ts,
            branch: line.branch.clone(),
            prompt: line.prompt.clone(),
            sent: None,
        }),
    }
}

/// `user`/`assistant` lines with a timestamp; sidechain (subagent) lines and
/// bookkeeping lines (`mode`, `attachment`, …) carry no session evidence.
fn parse_line(raw: &str) -> Option<Line> {
    let v: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    let kind = v.get("type")?.as_str()?;
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
    let prompt = (kind == "user")
        .then(|| prompt_text(v.get("message")?.get("content")?))
        .flatten();
    Some(Line {
        ts,
        cwd,
        branch,
        session_id,
        prompt,
    })
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const USER: &str = r#"{"type":"user","cwd":"/home/u/dev/app","gitBranch":"ABC-1-x","sessionId":"s1","timestamp":"2026-09-02T19:00:00.000Z","isSidechain":false,"message":{"role":"user","content":"fix the flaky test"}}"#;
    const MODE: &str = r#"{"type":"mode","sessionId":"s1"}"#;
    const SIDE: &str = r#"{"type":"user","cwd":"/home/u/dev/app","gitBranch":"ABC-1-x","sessionId":"s1","timestamp":"2026-09-02T19:30:00.000Z","isSidechain":true,"message":{"role":"user","content":"sub"}}"#;
    const ASSISTANT: &str = r#"{"type":"assistant","cwd":"/home/u/dev/app","gitBranch":"ABC-1-x","sessionId":"s1","timestamp":"2026-09-02T19:05:00.000Z","message":{"role":"assistant","content":[{"type":"text","text":"ok"}]}}"#;

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
