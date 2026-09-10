//! Cursor composer chats: `~/.config/Cursor/User/globalStorage/state.vscdb`,
//! a `cursorDiskKV(key, value)` table. `composerData:<id>` names the
//! conversation and lists its bubble ids; `bubbleId:<id>:<bubble>` is one
//! turn. Cursor's schema churns fastest of any format here, so this stays
//! deliberately small: the first user bubble's text and files per
//! composer, a best-effort cwd from `workspaceStorage`, everything else
//! tolerated as absent rather than treated as an error.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rusqlite::Connection;

use super::{Rec, SessionFormat, Source, clip};

pub struct Cursor;

impl SessionFormat for Cursor {
    fn name(&self) -> &'static str {
        "cursor"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let dir = home.join(".config/Cursor/User/globalStorage");
        if dir.join("state.vscdb").is_file() {
            vec![dir]
        } else {
            Vec::new()
        }
    }

    fn scan(&self, root: &Path) -> Vec<Source> {
        let db = root.join("state.vscdb");
        if db.is_file() {
            vec![Source {
                path: db,
                session_id: "cursor".into(),
                cwd_hint: None,
            }]
        } else {
            Vec::new()
        }
    }

    fn read(&self, source: &Source, cursor: u64) -> (Vec<Rec>, u64) {
        let Some(tmp) = super::copy_db(&source.path, "cursor") else {
            return (Vec::new(), cursor);
        };
        let folders = workspace_folders(&source.path);
        let out = read_composers(&tmp, cursor, &folders);
        let _ = std::fs::remove_file(&tmp);
        out.unwrap_or((Vec::new(), cursor))
    }
}

fn read_composers(
    tmp: &Path,
    cursor: u64,
    folders: &HashMap<String, String>,
) -> Option<(Vec<Rec>, u64)> {
    let conn = Connection::open(tmp).ok()?;
    let mut stmt = conn
        .prepare("SELECT key, value FROM cursorDiskKV WHERE key LIKE 'composerData:%'")
        .ok()?;
    let mut rows = stmt.query([]).ok()?;
    let mut out = Vec::new();
    let mut max_seen = cursor;
    while let Ok(Some(row)) = rows.next() {
        let key: String = row.get(0).ok()?;
        let value: String = row.get(1).ok()?;
        let Some(id) = key.strip_prefix("composerData:") else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&value) else {
            continue;
        };
        let created = v.get("createdAt").and_then(|c| c.as_i64()).unwrap_or(0);
        if (created.max(0) as u64) <= cursor {
            continue;
        }
        max_seen = max_seen.max(created.max(0) as u64);
        let title = v.get("name").and_then(|n| n.as_str()).map(str::to_owned);
        let headers = v
            .get("fullConversationHeadersOnly")
            .and_then(|h| h.as_array())
            .cloned()
            .unwrap_or_default();

        let mut prompt = None;
        let mut paths = Vec::new();
        let mut ts_ms = created;
        for h in &headers {
            let Some(bubble_id) = h.get("bubbleId").and_then(|b| b.as_str()) else {
                continue;
            };
            let bubble_key = format!("bubbleId:{id}:{bubble_id}");
            let Some(bubble) = conn
                .query_row(
                    "SELECT value FROM cursorDiskKV WHERE key = ?1",
                    [&bubble_key],
                    |r| r.get::<_, String>(0),
                )
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            else {
                continue;
            };
            if bubble.get("type").and_then(|t| t.as_i64()) != Some(1) {
                continue; // not a user turn
            }
            if prompt.is_none() {
                prompt = bubble.get("text").and_then(|t| t.as_str()).and_then(clip);
                if let Some(start) = bubble
                    .get("timingInfo")
                    .and_then(|t| t.get("clientStartTime"))
                    .and_then(|s| s.as_i64())
                {
                    ts_ms = start;
                }
            }
            if let Some(files) = bubble.get("relevantFiles").and_then(|f| f.as_array()) {
                for f in files {
                    if let Some(p) = f.as_str() {
                        paths.push(p.to_owned());
                    }
                }
            }
        }
        if prompt.is_none() && paths.is_empty() {
            continue;
        }
        let Some(ts) = Timestamp::from_millisecond(ts_ms).ok() else {
            continue;
        };
        out.push(Rec {
            ts,
            cwd: folders.get(id).cloned(),
            branch: None,
            prompt,
            write: !paths.is_empty(),
            paths,
            title,
            session_id: id.to_owned(),
        });
    }
    Some((out, max_seen))
}

/// Best-effort composer id -> workspace folder map from
/// `workspaceStorage/<hash>/{workspace.json,state.vscdb}`; any failure
/// along the way just leaves the composers it touched unmapped.
fn workspace_folders(global_db: &Path) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let Some(user_dir) = global_db.parent().and_then(|p| p.parent()) else {
        return out;
    };
    let Ok(entries) = std::fs::read_dir(user_dir.join("workspaceStorage")) else {
        return out;
    };
    for entry in entries.flatten() {
        let dir = entry.path();
        let Some(folder) = std::fs::read_to_string(dir.join("workspace.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("folder").and_then(|f| f.as_str()).map(str::to_owned))
        else {
            continue;
        };
        let folder = folder.trim_start_matches("file://").to_owned();
        let Some(tmp) = super::copy_db(&dir.join("state.vscdb"), "cursor-ws") else {
            continue;
        };
        for id in composer_ids(&tmp) {
            out.insert(id, folder.clone());
        }
        let _ = std::fs::remove_file(&tmp);
    }
    out
}

fn composer_ids(tmp: &Path) -> Vec<String> {
    let Ok(conn) = Connection::open(tmp) else {
        return Vec::new();
    };
    let Ok(value) = conn.query_row(
        "SELECT value FROM ItemTable WHERE key = 'composer.composerData'",
        [],
        |r| r.get::<_, String>(0),
    ) else {
        return Vec::new();
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&value) else {
        return Vec::new();
    };
    v.get("allComposers")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| {
                    c.get("composerId")
                        .and_then(|i| i.as_str())
                        .map(str::to_owned)
                })
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "chronicle-{tag}-{}-{}",
            std::process::id(),
            Timestamp::now().as_microsecond()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn reads_the_first_user_bubble_and_files_per_composer() {
        // `<user_dir>/globalStorage/state.vscdb` and a sibling
        // `workspaceStorage/<hash>/{workspace.json,state.vscdb}` so the
        // cwd match has something real to find.
        let user_dir = scratch_dir("cursor-user");
        let global_dir = user_dir.join("globalStorage");
        std::fs::create_dir_all(&global_dir).unwrap();
        let global_db = global_dir.join("state.vscdb");
        let conn = Connection::open(&global_db).unwrap();
        conn.execute_batch("CREATE TABLE cursorDiskKV (key TEXT PRIMARY KEY, value TEXT);")
            .unwrap();
        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES ('composerData:c1', ?1)",
            [serde_json::json!({
                "name": "Refactor auth",
                "createdAt": 1788800000000i64,
                "fullConversationHeadersOnly": [{"bubbleId": "b1", "type": 1}],
            })
            .to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO cursorDiskKV (key, value) VALUES ('bubbleId:c1:b1', ?1)",
            [serde_json::json!({
                "text": "split the auth module",
                "type": 1,
                "timingInfo": {"clientStartTime": 1788800005000i64},
                "relevantFiles": ["src/auth.rs"],
            })
            .to_string()],
        )
        .unwrap();
        drop(conn);

        let ws_dir = user_dir.join("workspaceStorage").join("hash1");
        std::fs::create_dir_all(&ws_dir).unwrap();
        std::fs::write(
            ws_dir.join("workspace.json"),
            r#"{"folder":"file:///home/u/dev/auth"}"#,
        )
        .unwrap();
        let ws_db = ws_dir.join("state.vscdb");
        let conn = Connection::open(&ws_db).unwrap();
        conn.execute_batch("CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT);")
            .unwrap();
        conn.execute(
            "INSERT INTO ItemTable (key, value) VALUES ('composer.composerData', ?1)",
            [serde_json::json!({"allComposers": [{"composerId": "c1"}]}).to_string()],
        )
        .unwrap();
        drop(conn);

        let source = Source {
            path: global_db,
            session_id: "cursor".into(),
            cwd_hint: None,
        };
        let (recs, cursor) = Cursor.read(&source, 0);
        assert_eq!(recs.len(), 1, "{recs:?}");
        assert_eq!(recs[0].session_id, "c1");
        assert_eq!(recs[0].title.as_deref(), Some("Refactor auth"));
        assert_eq!(recs[0].prompt.as_deref(), Some("split the auth module"));
        assert_eq!(recs[0].paths, vec!["src/auth.rs".to_string()]);
        assert_eq!(recs[0].cwd.as_deref(), Some("/home/u/dev/auth"));
        assert_eq!(
            recs[0].ts,
            Timestamp::from_millisecond(1788800005000).unwrap()
        );
        assert_eq!(cursor, 1788800000000);

        let (more, _) = Cursor.read(&source, cursor);
        assert!(more.is_empty());

        let _ = std::fs::remove_dir_all(&user_dir);
    }
}
