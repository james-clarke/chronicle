//! The day's intent: what the user said in the morning they would work on,
//! stored in `meta` as `intent:<YYYY-MM-DD>`. The evening side reads it back
//! as the digest's `## Plan` section, so a drafted standup can say where the
//! day actually went.

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::storage::{StorageError, get_meta, set_meta};

/// One day's intent. "Skip today" stores an empty one: the key exists, so
/// the morning picker stops asking, and no plan reaches the prompts.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Intent {
    /// Tasks ticked in the picker, in the order they were shown.
    #[serde(default)]
    pub task_ids: Vec<i64>,
    /// Free-text line ("finish the picker, then the soak").
    #[serde(default)]
    pub text: String,
}

impl Intent {
    pub fn is_empty(&self) -> bool {
        self.task_ids.is_empty() && self.text.trim().is_empty()
    }
}

fn meta_key(day: &str) -> String {
    format!("intent:{day}")
}

/// `None` = nothing set for that day (the picker is due). A stored value
/// that no longer parses reads as an empty intent rather than an error: the
/// picker asking twice is worse than losing one day's plan.
pub fn get(conn: &Connection, day: &str) -> Result<Option<Intent>, StorageError> {
    let Some(raw) = get_meta(conn, &meta_key(day))? else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_str(&raw).unwrap_or_default()))
}

pub fn set(conn: &Connection, day: &str, intent: &Intent) -> Result<(), StorageError> {
    let raw = serde_json::to_string(intent).unwrap_or_else(|_| "{}".to_owned());
    set_meta(conn, &meta_key(day), Some(&raw))
}

/// The day's plan as digest lines (`- task: …` / `- note: …`), or `None`
/// when no intent was set, it was skipped, or every task in it is gone.
/// Callers write the `## Plan` heading; the body is shared so the derive
/// digest and the standup digest say the plan the same way.
pub fn plan_body(conn: &Connection, day: &str) -> Result<Option<String>, StorageError> {
    let Some(intent) = get(conn, day)? else {
        return Ok(None);
    };
    let mut out = String::new();
    for id in &intent.task_ids {
        let label: Option<(String, Option<String>)> = conn
            .query_row("SELECT label, project FROM tasks WHERE id=?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .ok();
        if let Some((label, project)) = label {
            out.push_str("- task: ");
            out.push_str(&label);
            if let Some(p) = project.as_deref().filter(|p| !p.trim().is_empty()) {
                out.push_str(&format!(" [{p}]"));
            }
            out.push('\n');
        }
    }
    let text = intent.text.trim();
    if !text.is_empty() {
        out.push_str(&format!("- note: {text}\n"));
    }
    Ok((!out.is_empty()).then_some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::storage::test_migrate(&mut conn);
        conn
    }

    #[test]
    fn intent_json_round_trip() {
        let conn = db();
        assert_eq!(get(&conn, "2026-09-03").unwrap(), None);
        let intent = Intent {
            task_ids: vec![7, 9],
            text: "finish chunk 5".into(),
        };
        set(&conn, "2026-09-03", &intent).unwrap();
        assert_eq!(get(&conn, "2026-09-03").unwrap(), Some(intent));
        // Stored as the documented shape, under the documented key.
        let raw = get_meta(&conn, "intent:2026-09-03").unwrap().unwrap();
        assert_eq!(raw, r#"{"task_ids":[7,9],"text":"finish chunk 5"}"#);
        // "skip today": the key exists, the intent is empty.
        set(&conn, "2026-09-04", &Intent::default()).unwrap();
        let skipped = get(&conn, "2026-09-04").unwrap().unwrap();
        assert!(skipped.is_empty());
        assert_eq!(plan_body(&conn, "2026-09-04").unwrap(), None);
        // Garbage reads as empty, never as an error.
        set_meta(&conn, "intent:2026-09-05", Some("not json")).unwrap();
        assert_eq!(get(&conn, "2026-09-05").unwrap(), Some(Intent::default()));
    }

    #[test]
    fn plan_body_names_the_tasks_that_still_exist() {
        let conn = db();
        conn.execute_batch(
            "INSERT INTO tasks (id, label, project, status, source, created_ts)
                 VALUES (1, 'm26 chunk 5', 'chronicle', 'open', 'user', 100);
             INSERT INTO tasks (id, label, project, status, source, created_ts)
                 VALUES (2, 'ACME-14 triage', NULL, 'open', 'user', 100);",
        )
        .unwrap();
        set(
            &conn,
            "2026-09-03",
            &Intent {
                task_ids: vec![1, 2, 99],
                text: "  and the soak  ".into(),
            },
        )
        .unwrap();
        assert_eq!(
            plan_body(&conn, "2026-09-03").unwrap().unwrap(),
            "- task: m26 chunk 5 [chronicle]\n- task: ACME-14 triage\n- note: and the soak\n"
        );
    }
}
