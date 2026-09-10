//! GitHub Copilot CLI: `~/.copilot/session-state/<id>/workspace.yaml`
//! (trivial `key: value` lines — `cwd`, `git_root`, `branch`) plus
//! `events.jsonl` (`user.message`, `tool.execution_start`, `session.start`).

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use super::{Rec, SessionFormat, Source, clip};

pub struct Copilot;

impl SessionFormat for Copilot {
    fn name(&self) -> &'static str {
        "copilot"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let dir = home.join(".copilot").join("session-state");
        if dir.is_dir() { vec![dir] } else { Vec::new() }
    }

    fn scan(&self, root: &Path) -> Vec<Source> {
        let mut out = Vec::new();
        let Ok(sessions) = std::fs::read_dir(root) else {
            return out;
        };
        for entry in sessions.flatten() {
            let dir = entry.path();
            let events = dir.join("events.jsonl");
            if !events.is_file() {
                continue;
            }
            let session_id = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let cwd_hint = workspace_cwd(&dir.join("workspace.yaml"));
            out.push(Source {
                path: events,
                session_id,
                cwd_hint,
            });
        }
        out
    }

    fn read(&self, source: &Source, cursor: u64) -> (Vec<Rec>, u64) {
        let (lines, next) = super::read_lines_from(&source.path, cursor);
        let mut out = Vec::new();
        for line in &lines {
            let Ok(v) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
                continue;
            };
            let Some(kind) = v.get("type").and_then(|t| t.as_str()) else {
                continue;
            };
            let Some(ts) = v
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(|t| t.parse::<Timestamp>().ok())
            else {
                continue;
            };
            let data = v.get("data");
            let rec = match kind {
                "session.start" => Rec {
                    ts,
                    cwd: None,
                    branch: data
                        .and_then(|d| d.get("context"))
                        .and_then(|c| c.get("branch"))
                        .and_then(|b| b.as_str())
                        .map(str::to_owned),
                    prompt: None,
                    paths: Vec::new(),
                    write: false,
                    title: None,
                    session_id: source.session_id.clone(),
                },
                "user.message" => {
                    let Some(text) = data.and_then(|d| d.get("content")).and_then(|c| c.as_str())
                    else {
                        continue;
                    };
                    let Some(prompt) = clip(text) else {
                        continue;
                    };
                    Rec {
                        ts,
                        cwd: None,
                        branch: None,
                        prompt: Some(prompt),
                        paths: Vec::new(),
                        write: false,
                        title: None,
                        session_id: source.session_id.clone(),
                    }
                }
                "tool.execution_start" => {
                    let args = data.and_then(|d| d.get("arguments"));
                    let paths = args.map(path_tokens).unwrap_or_default();
                    if paths.is_empty() {
                        continue;
                    }
                    Rec {
                        ts,
                        cwd: None,
                        branch: None,
                        prompt: None,
                        paths,
                        write: true,
                        title: None,
                        session_id: source.session_id.clone(),
                    }
                }
                _ => continue,
            };
            out.push(rec);
        }
        (out, next)
    }
}

/// `cwd:` from a trivial `key: value` yaml file; no parser, just lines.
fn workspace_cwd(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    for line in text.lines() {
        if let Some(rest) = line.trim_start().strip_prefix("cwd:") {
            let v = rest.trim().trim_matches('"').trim_matches('\'');
            if !v.is_empty() {
                return Some(v.to_owned());
            }
        }
    }
    None
}

/// Path-shaped tokens from `data.arguments`, whether it is a JSON object or
/// a raw string: any `/`-containing, whitespace-free token.
fn path_tokens(args: &serde_json::Value) -> Vec<String> {
    let raw = match args {
        serde_json::Value::String(s) => s.clone(),
        other => other.to_string(),
    };
    raw.split(|c: char| {
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
            "/../../fixtures/sessions/copilot"
        ))
    }

    #[test]
    fn scans_and_reads_a_session() {
        let root = fixture_dir();
        let sources = Copilot.scan(&root);
        assert_eq!(sources.len(), 1, "{sources:?}");
        assert_eq!(sources[0].session_id, "cp1");
        assert_eq!(sources[0].cwd_hint.as_deref(), Some("/home/u/dev/web"));

        let (recs, cursor) = Copilot.read(&sources[0], 0);
        assert!(cursor > 0);
        let start = recs.iter().find(|r| r.branch.is_some()).unwrap();
        assert_eq!(start.branch.as_deref(), Some("main"));

        let prompt = recs.iter().find(|r| r.prompt.is_some()).unwrap();
        assert_eq!(prompt.prompt.as_deref(), Some("upgrade the router"));
        assert_eq!(prompt.ts, "2026-09-05T11:00:00Z".parse().unwrap());
        assert_eq!(prompt.session_id, "cp1");

        let call = recs.iter().find(|r| !r.paths.is_empty()).unwrap();
        assert_eq!(call.paths, vec!["src/router.ts".to_string()]);
        assert!(call.write);
    }
}
