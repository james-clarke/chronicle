//! Editor workspace reader (m37 chunk 3): recently-opened folders and
//! projects read straight off each editor's own on-disk state — VS Code
//! family (`state.vscdb` + `workspaceStorage/*/workspace.json`), JetBrains
//! (`recentProjects.xml`) and Zed (`db.sqlite`). No editor needs to be
//! running. Feeds discovery candidates (chunk 2) and the title resolver in
//! `extract.rs` (chunk 4).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use jiff::Timestamp;
use rusqlite::{Connection, OpenFlags, params};
use serde::Deserialize;

/// One recently-opened folder or project, from one editor's own history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Workspace {
    /// "vscode" | "cursor" | "windsurf" | "vscodium" | "jetbrains" | "zed".
    pub editor: &'static str,
    /// Local absolute path, or the remote-side path for remote URIs.
    pub path: String,
    /// e.g. "ssh-remote+devbox", "codespaces+name", "wsl+Ubuntu",
    /// "dev-container+…"; `None` for a local workspace.
    pub remote: Option<String>,
    pub last_ts_ms: Option<i64>,
}

const VSCODE_ROOTS: &[(&str, &str)] = &[
    ("Code", "vscode"),
    ("Code - OSS", "vscode"),
    ("VSCodium", "vscodium"),
    ("Cursor", "cursor"),
    ("Windsurf", "windsurf"),
];

/// Scans the known roots under `home` for every editor's recent-workspace
/// state. Never panics on malformed data; a file or row that doesn't parse
/// is skipped.
pub fn read_all(home: &Path) -> Vec<Workspace> {
    let mut out = Vec::new();
    for (dir, editor) in VSCODE_ROOTS {
        let user_root = home.join(".config").join(dir).join("User");
        out.extend(read_vscdb(&user_root, editor));
        out.extend(read_workspace_storage(&user_root, editor));
    }
    out.extend(read_jetbrains(home));
    out.extend(read_zed(home));
    dedupe(out)
}

/// The distinct local (non-remote) paths that exist on disk and contain a
/// `.git` — the discovery list the project page shows.
pub fn candidates(ws: &[Workspace]) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for w in ws {
        if w.remote.is_some() {
            continue;
        }
        let path = PathBuf::from(&w.path);
        if !seen.insert(path.clone()) {
            continue;
        }
        if path.join(".git").exists() {
            out.push(path);
        }
    }
    out
}

fn dedupe(list: Vec<Workspace>) -> Vec<Workspace> {
    let mut map: HashMap<(&'static str, String, Option<String>), Option<i64>> = HashMap::new();
    for w in list {
        let key = (w.editor, w.path, w.remote);
        let slot = map.entry(key).or_insert(None);
        *slot = max_opt(*slot, w.last_ts_ms);
    }
    let mut out: Vec<Workspace> = map
        .into_iter()
        .map(|((editor, path, remote), last_ts_ms)| Workspace {
            editor,
            path,
            remote,
            last_ts_ms,
        })
        .collect();
    out.sort_by_key(|w| std::cmp::Reverse(w.last_ts_ms));
    out
}

fn max_opt(a: Option<i64>, b: Option<i64>) -> Option<i64> {
    match (a, b) {
        (Some(x), Some(y)) => Some(x.max(y)),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

// ---------------------------------------------------------------------
// VS Code family
// ---------------------------------------------------------------------

#[derive(Deserialize)]
struct RecentList {
    #[serde(default)]
    entries: Vec<RecentEntry>,
}

#[derive(Deserialize)]
struct RecentEntry {
    #[serde(rename = "folderUri", default)]
    folder_uri: Option<String>,
    #[serde(default)]
    workspace: Option<RecentWorkspace>,
    // `fileUri` entries are read implicitly by not matching either field
    // above, and are skipped.
}

#[derive(Deserialize)]
struct RecentWorkspace {
    #[serde(rename = "configPath")]
    config_path: String,
}

fn read_vscdb(user_root: &Path, editor: &'static str) -> Vec<Workspace> {
    let db_path = user_root.join("globalStorage").join("state.vscdb");
    if !db_path.is_file() {
        return Vec::new();
    }
    let last_ts_ms = mtime_ms(&db_path);
    let Some(entries) = with_temp_copy(&db_path, read_recent_list).flatten() else {
        return Vec::new();
    };
    entries
        .into_iter()
        .map(|(path, remote)| Workspace {
            editor,
            path,
            remote,
            last_ts_ms,
        })
        .collect()
}

fn read_recent_list(conn: &Connection) -> Option<Vec<(String, Option<String>)>> {
    // The column is declared BLOB, but VS Code's own driver (and our test
    // fixture) writes the JSON as TEXT storage class; accept either.
    let text: String = conn
        .query_row(
            "SELECT value FROM ItemTable WHERE key = ?1",
            params!["history.recentlyOpenedPathsList"],
            |r| {
                Ok(match r.get_ref(0)? {
                    rusqlite::types::ValueRef::Text(t) => String::from_utf8_lossy(t).into_owned(),
                    rusqlite::types::ValueRef::Blob(b) => String::from_utf8_lossy(b).into_owned(),
                    _ => String::new(),
                })
            },
        )
        .ok()?;
    let list: RecentList = serde_json::from_str(&text).ok()?;
    let mut out = Vec::new();
    for entry in list.entries {
        if let Some(uri) = entry.folder_uri {
            if let Some(parsed) = parse_uri(&uri) {
                out.push(parsed);
            }
        } else if let Some(ws) = entry.workspace
            && let Some((path, remote)) = parse_uri(&ws.config_path)
        {
            let parent = Path::new(&path)
                .parent()
                .map(|p| p.display().to_string())
                .unwrap_or(path);
            out.push((parent, remote));
        }
    }
    Some(out)
}

fn read_workspace_storage(user_root: &Path, editor: &'static str) -> Vec<Workspace> {
    let dir = user_root.join("workspaceStorage");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let ws_dir = entry.path();
        let json_path = ws_dir.join("workspace.json");
        let Ok(text) = std::fs::read_to_string(&json_path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        let uri = value
            .get("folder")
            .or_else(|| value.get("workspace"))
            .and_then(|v| v.as_str());
        let Some(uri) = uri else {
            continue;
        };
        let Some((path, remote)) = parse_uri(uri) else {
            continue;
        };
        out.push(Workspace {
            editor,
            path,
            remote,
            last_ts_ms: mtime_ms(&ws_dir),
        });
    }
    out
}

/// Parses a `scheme://authority/path` (or `file:///path`) URI into
/// (local path or remote-side path, remote label for a non-`file` scheme).
/// The authority (and path) are percent-decoded.
fn parse_uri(uri: &str) -> Option<(String, Option<String>)> {
    let (scheme, rest) = uri.split_once("://")?;
    if scheme == "file" {
        Some((percent_decode(rest), None))
    } else {
        let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
        let path = percent_decode(&format!("/{path}"));
        Some((path, Some(percent_decode(authority))))
    }
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 3 <= bytes.len()
            && let Ok(byte) = u8::from_str_radix(&s[i + 1..i + 3], 16)
        {
            out.push(byte);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------
// JetBrains
// ---------------------------------------------------------------------

fn read_jetbrains(home: &Path) -> Vec<Workspace> {
    let root = home.join(".config").join("JetBrains");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let xml_path = entry.path().join("options").join("recentProjects.xml");
        let Ok(text) = std::fs::read_to_string(&xml_path) else {
            continue;
        };
        out.extend(parse_jetbrains_xml(&text, home));
    }
    out
}

/// Hand-rolled: no XML crate in the workspace. Finds each `<entry key="…">`
/// block and, within it, `activationTimestamp` (falling back to
/// `projectOpenTimestamp`).
fn parse_jetbrains_xml(xml: &str, home: &Path) -> Vec<Workspace> {
    let mut out = Vec::new();
    let mut rest = xml;
    const ENTRY: &str = "<entry key=\"";
    while let Some(start) = rest.find(ENTRY) {
        rest = &rest[start + ENTRY.len()..];
        let Some(key_end) = rest.find('"') else {
            break;
        };
        let key = &rest[..key_end];
        let block_start = &rest[key_end..];
        let block_end = block_start.find(ENTRY).unwrap_or(block_start.len());
        let block = &block_start[..block_end];
        let ts = extract_xml_attr(block, "activationTimestamp")
            .or_else(|| extract_xml_attr(block, "projectOpenTimestamp"));
        let path = key.replace("$USER_HOME$", &home.display().to_string());
        out.push(Workspace {
            editor: "jetbrains",
            path,
            remote: None,
            last_ts_ms: ts.and_then(|s| s.parse::<i64>().ok()),
        });
        rest = block_start;
    }
    out
}

fn extract_xml_attr(block: &str, name: &str) -> Option<String> {
    let needle = format!("name=\"{name}\" value=\"");
    let start = block.find(&needle)? + needle.len();
    let end = block[start..].find('"')?;
    Some(block[start..start + end].to_owned())
}

// ---------------------------------------------------------------------
// Zed
// ---------------------------------------------------------------------

fn read_zed(home: &Path) -> Vec<Workspace> {
    let root = home.join(".local").join("share").join("zed").join("db");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let db_path = entry.path().join("db.sqlite");
        if !db_path.is_file() {
            continue;
        }
        if let Some(rows) = with_temp_copy(&db_path, read_zed_workspaces).flatten() {
            out.extend(rows);
        }
    }
    out
}

fn read_zed_workspaces(conn: &Connection) -> Option<Vec<Workspace>> {
    read_zed_column(conn, "paths").or_else(|| read_zed_column(conn, "workspace_location"))
}

fn read_zed_column(conn: &Connection, column: &str) -> Option<Vec<Workspace>> {
    let sql = format!("SELECT {column}, timestamp FROM workspaces");
    let mut stmt = conn.prepare(&sql).ok()?;
    let rows = stmt
        .query_map([], |r| {
            let paths: String = r.get(0)?;
            let ts: Option<String> = r.get(1)?;
            Ok((paths, ts))
        })
        .ok()?;
    let mut out = Vec::new();
    for row in rows.flatten() {
        let (paths, ts) = row;
        let last_ts_ms = ts.as_deref().and_then(parse_sqlite_datetime);
        for p in paths
            .split(['\n', '\0'])
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            out.push(Workspace {
                editor: "zed",
                path: p.to_owned(),
                remote: None,
                last_ts_ms,
            });
        }
    }
    Some(out)
}

/// SQLite `datetime()` text ("YYYY-MM-DD HH:MM:SS[.sss]", UTC) to epoch ms.
fn parse_sqlite_datetime(s: &str) -> Option<i64> {
    let s = s.trim();
    let iso = if s.contains('T') {
        s.to_owned()
    } else {
        s.replacen(' ', "T", 1)
    };
    let iso = if iso.ends_with('Z') || iso.contains('+') {
        iso
    } else {
        format!("{iso}Z")
    };
    iso.parse::<Timestamp>().ok().map(|t| t.as_millisecond())
}

// ---------------------------------------------------------------------
// Shared: read-only sqlite via a temp copy (the editor may hold a lock)
// ---------------------------------------------------------------------

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn with_temp_copy<T>(path: &Path, f: impl FnOnce(&Connection) -> T) -> Option<T> {
    let n = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = std::env::temp_dir().join(format!(
        "chronicle-workspaces-{}-{n}-{nanos}.sqlite",
        std::process::id()
    ));
    if std::fs::copy(path, &tmp).is_err() {
        return None;
    }
    let result = Connection::open_with_flags(
        &tmp,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()
    .map(|conn| f(&conn));
    let _ = std::fs::remove_file(&tmp);
    result
}

fn mtime_ms(path: &Path) -> Option<i64> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    let dur = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(dur.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;

    use super::*;

    #[test]
    fn reads_all_three_families_and_filters_candidates() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        // The one local folder that actually has a `.git` on disk.
        let repo = home.join("dev/foo");
        fs::create_dir_all(repo.join(".git")).unwrap();
        // A local folder without `.git`.
        let no_git = home.join("dev/bar");
        fs::create_dir_all(&no_git).unwrap();

        // --- VS Code: state.vscdb recent list ---
        let user_root = home.join(".config/Code/User");
        let vscdb = user_root.join("globalStorage/state.vscdb");
        fs::create_dir_all(vscdb.parent().unwrap()).unwrap();
        let recent = json!({
            "entries": [
                {"folderUri": format!("file://{}", repo.display())},
                {"folderUri": "vscode-remote://ssh-remote%2Bdevbox/home/u/app"},
                {"fileUri": format!("file://{}/README.md", no_git.display())},
            ]
        })
        .to_string();
        {
            let conn = Connection::open(&vscdb).unwrap();
            conn.execute_batch("CREATE TABLE ItemTable (key TEXT, value BLOB)")
                .unwrap();
            conn.execute(
                "INSERT INTO ItemTable (key, value) VALUES (?1, ?2)",
                params!["history.recentlyOpenedPathsList", recent],
            )
            .unwrap();
        }

        // --- VS Code: workspaceStorage/<hash>/workspace.json ---
        let ws_dir = user_root.join("workspaceStorage/abc123");
        fs::create_dir_all(&ws_dir).unwrap();
        fs::write(
            ws_dir.join("workspace.json"),
            json!({"folder": format!("file://{}", no_git.display())}).to_string(),
        )
        .unwrap();

        // --- JetBrains ---
        let jb_options = home.join(".config/JetBrains/IntelliJIdea2024.2/options");
        fs::create_dir_all(&jb_options).unwrap();
        fs::write(
            jb_options.join("recentProjects.xml"),
            r#"<component name="RecentProjectsManager">
  <option name="additionalInfo">
    <map>
      <entry key="$USER_HOME$/dev/jb1">
        <value><RecentProjectMetaInfo>
          <option name="activationTimestamp" value="1725900000000" />
          <option name="projectOpenTimestamp" value="1725800000000" />
        </RecentProjectMetaInfo></value>
      </entry>
      <entry key="$USER_HOME$/dev/jb2">
        <value><RecentProjectMetaInfo>
          <option name="projectOpenTimestamp" value="1725700000000" />
        </RecentProjectMetaInfo></value>
      </entry>
    </map>
  </option>
</component>"#,
        )
        .unwrap();

        // --- Zed ---
        let zed_dir = home.join(".local/share/zed/db/abcdef");
        fs::create_dir_all(&zed_dir).unwrap();
        let zed_db = zed_dir.join("db.sqlite");
        {
            let conn = Connection::open(&zed_db).unwrap();
            conn.execute_batch("CREATE TABLE workspaces (paths TEXT, timestamp TEXT)")
                .unwrap();
            conn.execute(
                "INSERT INTO workspaces (paths, timestamp) VALUES (?1, ?2)",
                params!["/home/u/dev/zed1\n/home/u/dev/zed2", "2026-09-01 12:00:00"],
            )
            .unwrap();
        }

        let ws = read_all(home);

        let repo_str = repo.display().to_string();
        let local = ws
            .iter()
            .find(|w| w.editor == "vscode" && w.path == repo_str)
            .expect("local folderUri entry");
        assert!(local.remote.is_none());
        assert!(
            local.last_ts_ms.is_some(),
            "falls back to the db file mtime"
        );

        let remote = ws
            .iter()
            .find(|w| w.editor == "vscode" && w.remote.is_some())
            .expect("ssh-remote folderUri entry");
        assert_eq!(remote.remote.as_deref(), Some("ssh-remote+devbox"));
        assert_eq!(remote.path, "/home/u/app");

        assert!(
            !ws.iter().any(|w| w.path.ends_with("README.md")),
            "fileUri entries are skipped"
        );

        let no_git_str = no_git.display().to_string();
        let from_storage = ws
            .iter()
            .find(|w| w.editor == "vscode" && w.path == no_git_str)
            .expect("workspace.json entry");
        assert!(from_storage.last_ts_ms.is_some(), "uses the dir mtime");

        let jb1_path = home.join("dev/jb1").display().to_string();
        let jb1 = ws
            .iter()
            .find(|w| w.editor == "jetbrains" && w.path == jb1_path)
            .expect("first jetbrains entry");
        assert_eq!(jb1.last_ts_ms, Some(1_725_900_000_000));
        let jb2_path = home.join("dev/jb2").display().to_string();
        let jb2 = ws
            .iter()
            .find(|w| w.editor == "jetbrains" && w.path == jb2_path)
            .expect("second jetbrains entry, falls back to projectOpenTimestamp");
        assert_eq!(jb2.last_ts_ms, Some(1_725_700_000_000));

        let expected_zed_ts = "2026-09-01T12:00:00Z"
            .parse::<Timestamp>()
            .unwrap()
            .as_millisecond();
        let zed1 = ws
            .iter()
            .find(|w| w.editor == "zed" && w.path == "/home/u/dev/zed1")
            .expect("first zed path of the row");
        assert_eq!(zed1.last_ts_ms, Some(expected_zed_ts));
        assert!(
            ws.iter()
                .any(|w| w.editor == "zed" && w.path == "/home/u/dev/zed2"),
            "second zed path of the same row"
        );

        let cands = candidates(&ws);
        assert_eq!(cands, vec![repo]);
    }

    #[test]
    fn parse_uri_handles_file_and_remote_schemes() {
        assert_eq!(
            parse_uri("file:///home/u/dev/foo"),
            Some(("/home/u/dev/foo".to_owned(), None))
        );
        assert_eq!(
            parse_uri("vscode-remote://wsl%2BUbuntu/home/u/app"),
            Some(("/home/u/app".to_owned(), Some("wsl+Ubuntu".to_owned())))
        );
        assert_eq!(parse_uri("not-a-uri"), None);
    }

    #[test]
    fn dedupe_keeps_the_max_timestamp() {
        let list = vec![
            Workspace {
                editor: "vscode",
                path: "/a".to_owned(),
                remote: None,
                last_ts_ms: Some(100),
            },
            Workspace {
                editor: "vscode",
                path: "/a".to_owned(),
                remote: None,
                last_ts_ms: Some(200),
            },
        ];
        let out = dedupe(list);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].last_ts_ms, Some(200));
    }
}
