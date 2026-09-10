//! Codex CLI rollouts: `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. The
//! first line is a `session_meta` record carrying the session id, cwd and
//! branch; after that, `response_item` lines carry user messages and tool
//! (`function_call`) invocations.

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use super::{Rec, SessionFormat, Source, clip};

pub struct Codex;

impl SessionFormat for Codex {
    fn name(&self) -> &'static str {
        "codex"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let dir = home.join(".codex").join("sessions");
        if dir.is_dir() { vec![dir] } else { Vec::new() }
    }

    fn scan(&self, root: &Path) -> Vec<Source> {
        super::walk_files(root, 4, |p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
        })
        .into_iter()
        .map(|path| {
            let session_id = session_meta_id(&path).unwrap_or_else(|| {
                path.file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            });
            Source {
                path,
                session_id,
                cwd_hint: None,
            }
        })
        .collect()
    }

    fn read(&self, source: &Source, cursor: u64) -> (Vec<Rec>, u64) {
        let (lines, next) = super::read_lines_from(&source.path, cursor);
        let mut out = Vec::new();
        for line in &lines {
            let Some(rec) = parse_line(line, &source.session_id) else {
                continue;
            };
            out.push(rec);
        }
        (out, next)
    }
}

/// The `id` a rollout's first `session_meta` line names, if the file
/// starts with one; used as the fallback session id when scanning.
fn session_meta_id(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let first = text.lines().next()?;
    let v: serde_json::Value = serde_json::from_str(first.trim()).ok()?;
    v.get("payload")?.get("id")?.as_str().map(str::to_owned)
}

fn parse_line(raw: &str, session_id: &str) -> Option<Rec> {
    let v: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
    let ts: Timestamp = v.get("timestamp")?.as_str()?.parse().ok()?;
    let kind = v.get("type")?.as_str()?;
    match kind {
        "session_meta" => {
            let payload = v.get("payload")?;
            let cwd = payload
                .get("cwd")
                .and_then(|c| c.as_str())
                .map(str::to_owned);
            let branch = payload
                .get("git")
                .and_then(|g| g.get("branch"))
                .and_then(|b| b.as_str())
                .map(str::to_owned);
            Some(Rec {
                ts,
                cwd,
                branch,
                prompt: None,
                paths: Vec::new(),
                write: false,
                title: None,
                session_id: session_id.to_owned(),
            })
        }
        "response_item" => {
            let payload = v.get("payload")?;
            let ptype = payload.get("type").and_then(|t| t.as_str())?;
            if ptype == "message" && payload.get("role").and_then(|r| r.as_str()) == Some("user") {
                let text = payload
                    .get("content")?
                    .as_array()?
                    .iter()
                    .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("input_text"))?
                    .get("text")?
                    .as_str()?;
                let prompt = clip(text)?;
                Some(Rec {
                    ts,
                    cwd: None,
                    branch: None,
                    prompt: Some(prompt),
                    paths: Vec::new(),
                    write: false,
                    title: None,
                    session_id: session_id.to_owned(),
                })
            } else if ptype == "function_call" {
                let args = payload.get("arguments").and_then(|a| a.as_str())?;
                let paths = path_tokens(args);
                if paths.is_empty() {
                    return None;
                }
                Some(Rec {
                    ts,
                    cwd: None,
                    branch: None,
                    prompt: None,
                    paths,
                    write: true,
                    title: None,
                    session_id: session_id.to_owned(),
                })
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Path-shaped tokens from a `function_call`'s raw `arguments` JSON string,
/// cheaply: any whitespace/punctuation-delimited token containing `/`.
fn path_tokens(args: &str) -> Vec<String> {
    args.split(|c: char| {
        c == '"'
            || c == ':'
            || c == ','
            || c == '{'
            || c == '}'
            || c == '['
            || c == ']'
            || c.is_whitespace()
    })
    .filter(|t| !t.is_empty() && t.contains('/'))
    .map(str::to_owned)
    .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/sessions/codex"
        ))
    }

    #[test]
    fn scans_and_reads_a_rollout() {
        let root = fixture_dir();
        let sources = Codex.scan(&root);
        assert_eq!(sources.len(), 1, "{sources:?}");
        assert_eq!(sources[0].session_id, "c1");

        let (recs, cursor) = Codex.read(&sources[0], 0);
        assert!(cursor > 0);
        let meta = recs.iter().find(|r| r.cwd.is_some()).unwrap();
        assert_eq!(meta.cwd.as_deref(), Some("/home/u/dev/api"));
        assert_eq!(meta.branch.as_deref(), Some("main"));

        let prompt = recs.iter().find(|r| r.prompt.is_some()).unwrap();
        assert_eq!(prompt.prompt.as_deref(), Some("add a health endpoint"));
        assert_eq!(prompt.ts, "2026-09-03T09:00:00Z".parse().unwrap());
        assert_eq!(prompt.session_id, "c1");

        let call = recs.iter().find(|r| !r.paths.is_empty()).unwrap();
        assert_eq!(call.paths, vec!["src/health.rs".to_string()]);
        assert!(call.write);
    }
}
