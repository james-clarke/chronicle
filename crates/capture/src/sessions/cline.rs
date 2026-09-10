//! Cline (`saoudrizwan.claude-dev`), a VS Code / Cursor / VSCodium
//! extension: `globalStorage/saoudrizwan.claude-dev/state/taskHistory.json`
//! names each task's cwd and id; `tasks/<id>/ui_messages.json` carries its
//! turns; `tasks/<id>/task_metadata.json` the files it touched.

use std::path::{Path, PathBuf};

use jiff::Timestamp;

use super::{Rec, SessionFormat, Source, clip};

pub struct Cline;

/// Editors that host the extension under their own `globalStorage`.
const EDITORS: [&str; 3] = ["Code", "Cursor", "VSCodium"];

impl SessionFormat for Cline {
    fn name(&self) -> &'static str {
        "cline"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        EDITORS
            .iter()
            .map(|e| {
                home.join(".config")
                    .join(e)
                    .join("User/globalStorage/saoudrizwan.claude-dev")
            })
            .filter(|d| d.is_dir())
            .collect()
    }

    fn scan(&self, root: &Path) -> Vec<Source> {
        let Ok(text) = std::fs::read_to_string(root.join("state").join("taskHistory.json")) else {
            return Vec::new();
        };
        let Ok(entries) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else {
            return Vec::new();
        };
        entries
            .iter()
            .filter_map(|e| {
                let id = e.get("id")?.as_str()?.to_owned();
                let cwd_hint = e
                    .get("cwdOnTaskInitialization")
                    .and_then(|c| c.as_str())
                    .map(str::to_owned);
                let path = root.join("tasks").join(&id).join("ui_messages.json");
                if !path.is_file() {
                    return None;
                }
                Some(Source {
                    path,
                    session_id: id,
                    cwd_hint,
                })
            })
            .collect()
    }

    fn read(&self, source: &Source, cursor: u64) -> (Vec<Rec>, u64) {
        let Ok(text) = std::fs::read_to_string(&source.path) else {
            return (Vec::new(), cursor);
        };
        let Ok(messages) = serde_json::from_str::<Vec<serde_json::Value>>(&text) else {
            return (Vec::new(), cursor);
        };
        let mut out = Vec::new();
        for m in messages.iter().skip(cursor as usize) {
            if m.get("type").and_then(|t| t.as_str()) != Some("say") {
                continue;
            }
            let say = m.get("say").and_then(|s| s.as_str()).unwrap_or_default();
            if say != "task" && say != "user_feedback" {
                continue;
            }
            let Some(ts_ms) = m.get("ts").and_then(|t| t.as_i64()) else {
                continue;
            };
            let Some(ts) = Timestamp::from_millisecond(ts_ms).ok() else {
                continue;
            };
            let Some(prompt) = m.get("text").and_then(|t| t.as_str()).and_then(clip) else {
                continue;
            };
            out.push(Rec {
                ts,
                cwd: None,
                branch: None,
                prompt: Some(prompt),
                paths: Vec::new(),
                write: false,
                title: None,
                session_id: source.session_id.clone(),
            });
        }
        // The files a task touched are static per task, not per message;
        // fold them in once, on the first read.
        if cursor == 0 {
            if let Some(paths) = task_paths(&source.path) {
                if !paths.is_empty() {
                    let ts = out.last().map(|r| r.ts).unwrap_or(Timestamp::UNIX_EPOCH);
                    out.push(Rec {
                        ts,
                        cwd: None,
                        branch: None,
                        prompt: None,
                        paths,
                        write: true,
                        title: None,
                        session_id: source.session_id.clone(),
                    });
                }
            }
        }
        (out, messages.len() as u64)
    }
}

fn task_paths(ui_messages_path: &Path) -> Option<Vec<String>> {
    let metadata_path = ui_messages_path.parent()?.join("task_metadata.json");
    let text = std::fs::read_to_string(metadata_path).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    Some(
        v.get("files_in_context")?
            .as_array()?
            .iter()
            .filter_map(|f| f.get("path").and_then(|p| p.as_str()).map(str::to_owned))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_dir() -> PathBuf {
        PathBuf::from(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixtures/sessions/cline"
        ))
    }

    #[test]
    fn scans_and_reads_a_task() {
        let root = fixture_dir();
        let sources = Cline.scan(&root);
        assert_eq!(sources.len(), 1, "{sources:?}");
        assert_eq!(sources[0].session_id, "t1");
        assert_eq!(sources[0].cwd_hint.as_deref(), Some("/home/u/dev/cli"));

        let (recs, cursor) = Cline.read(&sources[0], 0);
        assert!(cursor > 0);
        let first = recs.iter().find(|r| r.prompt.is_some()).unwrap();
        assert_eq!(first.prompt.as_deref(), Some("write a changelog entry"));
        assert_eq!(
            first.ts,
            Timestamp::from_millisecond(1788609600000).unwrap()
        );
        assert_eq!(first.session_id, "t1");

        let files = recs.iter().find(|r| !r.paths.is_empty()).unwrap();
        assert_eq!(files.paths, vec!["CHANGELOG.md".to_string()]);
        assert!(files.write);

        let (more, _) = Cline.read(&sources[0], cursor);
        assert!(more.is_empty());
    }
}
