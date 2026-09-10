//! Claude Code transcripts (`~/.claude/projects/<project>/<session>.jsonl`).
//!
//! `AiSessionProvider` reads these through its own richer, unchanged
//! pipeline (prompt minutes vs. write minutes, titles, per-file touches,
//! the last reply) so `detail` JSON and `ext_id`s for Claude sessions never
//! move (m22, m32 chunk 2). This `SessionFormat` impl is the thin,
//! format-generic view: detection (`AiSessionProvider::detected`) and
//! anything that wants Claude in the same shape as every other tool.

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use super::{Rec, SessionFormat, Source};
use crate::ai_sessions::{prompt_text, tool_paths};

pub struct Claude;

impl SessionFormat for Claude {
    fn name(&self) -> &'static str {
        "claude"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let dir = home.join(".claude").join("projects");
        if dir.is_dir() { vec![dir] } else { Vec::new() }
    }

    /// `<root>/<project>/*.jsonl`, one level down only (subagent
    /// transcripts live under `<session>/subagents/` and are never
    /// recursed into).
    fn scan(&self, root: &Path) -> Vec<Source> {
        let mut out = Vec::new();
        let Ok(projects) = std::fs::read_dir(root) else {
            return out;
        };
        for project in projects.flatten() {
            let Ok(files) = std::fs::read_dir(project.path()) else {
                continue;
            };
            for f in files.flatten() {
                let path = f.path();
                if path.extension().is_some_and(|e| e == "jsonl") && path.is_file() {
                    let session_id = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    out.push(Source {
                        path,
                        session_id,
                        cwd_hint: None,
                    });
                }
            }
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
            if kind != "user" && kind != "assistant" {
                continue;
            }
            if v.get("isSidechain").and_then(|s| s.as_bool()) == Some(true) {
                continue;
            }
            let Some(ts) = v
                .get("timestamp")
                .and_then(|t| t.as_str())
                .and_then(|t| t.parse::<Timestamp>().ok())
            else {
                continue;
            };
            let cwd = v.get("cwd").and_then(|c| c.as_str()).map(str::to_owned);
            let branch = v
                .get("gitBranch")
                .and_then(|b| b.as_str())
                .map(str::to_owned);
            let content = v.get("message").and_then(|m| m.get("content"));
            let prompt = (kind == "user")
                .then(|| content.and_then(prompt_text))
                .flatten();
            let paths = if kind == "assistant" {
                content
                    .map(|c| tool_paths(c, cwd.as_deref().unwrap_or("")))
                    .unwrap_or_default()
            } else {
                Vec::new()
            };
            out.push(Rec {
                ts,
                cwd,
                branch,
                prompt,
                write: !paths.is_empty(),
                paths,
                title: None,
                session_id: source.session_id.clone(),
            });
        }
        (out, next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/sessions/claude"
        ))
    }

    #[test]
    fn scans_and_reads_a_transcript() {
        let root = fixture_dir();
        let sources = Claude.scan(&root);
        assert_eq!(sources.len(), 1, "{sources:?}");
        assert_eq!(sources[0].session_id, "s1");

        let (recs, cursor) = Claude.read(&sources[0], 0);
        assert!(cursor > 0);
        let first = recs
            .iter()
            .find(|r| r.prompt.is_some())
            .expect("a prompt record");
        assert_eq!(first.cwd.as_deref(), Some("/home/u/dev/app"));
        assert_eq!(first.session_id, "s1");
        assert_eq!(first.prompt.as_deref(), Some("fix the flaky test"));
        assert_eq!(first.ts, "2026-09-02T19:00:00Z".parse().unwrap());

        let with_paths = recs
            .iter()
            .find(|r| !r.paths.is_empty())
            .expect("a paths record");
        assert_eq!(with_paths.paths, vec!["src/lib.rs".to_string()]);
        assert!(with_paths.write);

        // A second read from the returned cursor sees nothing new.
        let (more, _) = Claude.read(&sources[0], cursor);
        assert!(more.is_empty());
    }
}
