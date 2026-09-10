//! OpenCode: `~/.local/share/opencode/opencode.db`, a `session` table
//! (`id`, `project_id`, `directory`, `title`, `time_created`,
//! `time_updated`) and a `session_message` table (`id`, `session_id`,
//! `type`, `time_created`, `data`) whose `data` is `{"parts":[{"type":
//! "text","text"}]}` for a `type = 'user'` row. Read read-only from a
//! throwaway copy of the (possibly live) db; tolerates a `role` column in
//! place of `type`, since the schema isn't ours to pin.

use std::path::{Path, PathBuf};

use jiff::Timestamp;
use rusqlite::Connection;

use super::{Rec, SessionFormat, Source, clip};

pub struct OpenCode;

impl SessionFormat for OpenCode {
    fn name(&self) -> &'static str {
        "opencode"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let dir = home.join(".local/share/opencode");
        if dir.join("opencode.db").is_file() {
            vec![dir]
        } else {
            Vec::new()
        }
    }

    fn scan(&self, root: &Path) -> Vec<Source> {
        let db = root.join("opencode.db");
        if db.is_file() {
            vec![Source {
                path: db,
                session_id: "opencode".into(),
                cwd_hint: None,
            }]
        } else {
            Vec::new()
        }
    }

    fn read(&self, source: &Source, cursor: u64) -> (Vec<Rec>, u64) {
        let Some(tmp) = super::copy_db(&source.path, "opencode") else {
            return (Vec::new(), cursor);
        };
        let out = read_db(&tmp, cursor);
        let _ = std::fs::remove_file(&tmp);
        out.unwrap_or((Vec::new(), cursor))
    }
}

fn read_db(path: &Path, cursor: u64) -> Option<(Vec<Rec>, u64)> {
    let conn = Connection::open(path).ok()?;
    let col = if has_column(&conn, "session_message", "type") {
        "type"
    } else {
        "role"
    };
    let sql = format!(
        "SELECT sm.session_id, sm.{col}, sm.time_created, sm.data, s.directory, s.title \
         FROM session_message sm JOIN session s ON s.id = sm.session_id \
         WHERE sm.time_created > ?1 ORDER BY sm.time_created ASC"
    );
    let mut stmt = conn.prepare(&sql).ok()?;
    let mut rows = stmt.query(rusqlite::params![cursor as i64]).ok()?;
    let mut out = Vec::new();
    let mut max_seen = cursor;
    while let Ok(Some(row)) = rows.next() {
        let session_id: String = row.get(0).ok()?;
        let kind: String = row.get(1).ok()?;
        let time_created: i64 = row.get(2).ok()?;
        let data: Option<String> = row.get(3).ok()?;
        let directory: Option<String> = row.get(4).ok()?;
        let title: Option<String> = row.get(5).ok()?;
        max_seen = max_seen.max(time_created.max(0) as u64);
        if kind != "user" {
            continue;
        }
        let Some(ts) = Timestamp::from_millisecond(time_created).ok() else {
            continue;
        };
        let Some(prompt) = data
            .as_deref()
            .and_then(extract_text)
            .and_then(|t| clip(&t))
        else {
            continue;
        };
        out.push(Rec {
            ts,
            cwd: directory,
            branch: None,
            prompt: Some(prompt),
            paths: Vec::new(),
            write: false,
            title,
            session_id,
        });
    }
    Some((out, max_seen))
}

fn extract_text(data: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(data).ok()?;
    v.get("parts")?
        .as_array()?
        .iter()
        .find(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"))?
        .get("text")?
        .as_str()
        .map(str::to_owned)
}

fn has_column(conn: &Connection, table: &str, col: &str) -> bool {
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return false;
    };
    let Ok(mut rows) = stmt.query([]) else {
        return false;
    };
    while let Ok(Some(row)) = rows.next() {
        if row.get::<_, String>(1).ok().as_deref() == Some(col) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_db(dir: &Path) -> PathBuf {
        let path = dir.join("opencode.db");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY, project_id TEXT, directory TEXT, \
             title TEXT, time_created INTEGER, time_updated INTEGER);
             CREATE TABLE session_message (id TEXT PRIMARY KEY, session_id TEXT, type TEXT, \
             time_created INTEGER, data TEXT);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session (id, project_id, directory, title, time_created, time_updated) \
             VALUES ('s1', 'p1', '/home/u/dev/oc', 'Fix the parser', 1788700000000, 1788700100000)",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_message (id, session_id, type, time_created, data) VALUES \
             ('m1', 's1', 'user', 1788700000000, ?1)",
            [r#"{"parts":[{"type":"text","text":"fix the null check"}]}"#],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO session_message (id, session_id, type, time_created, data) VALUES \
             ('m2', 's1', 'assistant', 1788700050000, ?1)",
            [r#"{"parts":[{"type":"text","text":"done"}]}"#],
        )
        .unwrap();
        path
    }

    #[test]
    fn reads_user_messages_from_a_real_db() {
        let dir = std::env::temp_dir().join(format!(
            "chronicle-opencode-test-{}-{}",
            std::process::id(),
            Timestamp::now().as_microsecond()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db_path = make_db(&dir);

        let source = Source {
            path: db_path,
            session_id: "opencode".into(),
            cwd_hint: None,
        };
        let (recs, cursor) = OpenCode.read(&source, 0);
        assert_eq!(recs.len(), 1, "{recs:?}");
        assert_eq!(recs[0].session_id, "s1");
        assert_eq!(recs[0].cwd.as_deref(), Some("/home/u/dev/oc"));
        assert_eq!(recs[0].prompt.as_deref(), Some("fix the null check"));
        assert_eq!(recs[0].title.as_deref(), Some("Fix the parser"));
        assert_eq!(
            recs[0].ts,
            Timestamp::from_millisecond(1788700000000).unwrap()
        );
        assert_eq!(cursor, 1788700050000);

        let (more, _) = OpenCode.read(&source, cursor);
        assert!(more.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
