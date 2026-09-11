//! Browser history collector (m37 chunk 4): every visit in a Chromium- or
//! Firefox-family profile becomes one `browse` event. Read-only, like
//! `shell.rs`: each poll copies the live profile database (Firefox also
//! copies its `-wal` file, so very recent visits are not stuck in the WAL)
//! to a scratch path, opens that copy read-only, queries visits newer than
//! the profile's cursor, then deletes the copy. The browser's own
//! connection to the real file is never touched.

use std::path::{Path, PathBuf};
use std::time::Duration;

use chronicle_core::types::{ActivityEvent, ActivityKind, CaptureEvent, ms_to_ts};
use crossbeam_channel::Sender;
use rusqlite::{Connection, OpenFlags, params};

use crate::{BoxError, FocusProvider};

const POLL: Duration = Duration::from_secs(60);
/// Rows read per profile per poll; the cursor still advances to the last
/// row read, so a backlog drains over a few polls instead of one huge read.
const BATCH: i64 = 2000;
/// Chrome epoch (1601-01-01) to Unix epoch (1970-01-01), in milliseconds.
const WEBKIT_EPOCH_OFFSET_MS: i64 = 11_644_473_600_000;
/// URL path kept in `detail`, in chars.
const MAX_PATH_CHARS: usize = 200;

/// `(dir under ~/.config, browser id)` for the Chromium family.
const CHROMIUM_DIRS: &[(&str, &str)] = &[
    ("google-chrome", "chrome"),
    ("chromium", "chromium"),
    ("BraveSoftware/Brave-Browser", "brave"),
    ("microsoft-edge", "edge"),
    ("vivaldi", "vivaldi"),
];

/// `(dir under ~/Library/Application Support, browser id)` for the
/// Chromium family on macOS. Both this and [`CHROMIUM_DIRS`] are always
/// probed; a base that doesn't exist on the running OS is just skipped.
const CHROMIUM_DIRS_MACOS: &[(&str, &str)] = &[
    ("Google/Chrome", "chrome"),
    ("Chromium", "chromium"),
    ("BraveSoftware/Brave-Browser", "brave"),
    ("Microsoft Edge", "edge"),
    ("Vivaldi", "vivaldi"),
    ("Arc/User Data", "arc"),
];

/// `(dir under ~, browser id)` for the Firefox family. Snap/flatpak
/// installs (different data dirs) are out of scope. Includes the macOS
/// profile dir, which has the same `profiles.ini`/`*.default*` layout as
/// `.mozilla/firefox`.
const FIREFOX_DIRS: &[(&str, &str)] = &[
    (".mozilla/firefox", "firefox"),
    (".librewolf", "librewolf"),
    (".zen", "zen"),
    ("Library/Application Support/Firefox/Profiles", "firefox"),
];

/// Schemes with nothing worth remembering: extension pages, inline data,
/// bookmarklets, the browser's own internal pages. Everything else that
/// isn't `http(s):`/`file:` is dropped too — those two (plus `localhost`
/// under http) are the point of a history collector.
const DROPPED_SCHEMES: &[&str] = &[
    "about",
    "chrome",
    "edge",
    "moz-extension",
    "chrome-extension",
    "data",
    "blob",
    "javascript",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Family {
    Chromium,
    Firefox,
}

/// One profile database being polled.
struct Profile {
    /// "chrome", "chromium", "brave", "edge", "vivaldi", "firefox",
    /// "librewolf", "zen".
    browser: &'static str,
    /// Profile directory name; folded into the ext_id when not default.
    label: String,
    is_default: bool,
    family: Family,
    db: PathBuf,
    /// Last visit id / visit time seen, in the source's own units (WebKit
    /// microseconds for Chromium, microseconds since the epoch for
    /// Firefox).
    cursor: i64,
    last_err: Option<String>,
}

impl Profile {
    fn new(
        browser: &'static str,
        label: String,
        is_default: bool,
        family: Family,
        db: PathBuf,
    ) -> Self {
        let start_ms = jiff::Timestamp::now().as_millisecond() - 24 * 60 * 60 * 1000;
        let cursor = match family {
            Family::Chromium => chromium_us_from_ms(start_ms),
            Family::Firefox => firefox_us_from_ms(start_ms),
        };
        Self {
            browser,
            label,
            is_default,
            family,
            db,
            cursor,
            last_err: None,
        }
    }

    /// The `<browser>` or `<browser>/<profile dir>` half of the ext_id.
    fn browser_id(&self) -> String {
        if self.is_default {
            self.browser.to_owned()
        } else {
            format!("{}/{}", self.browser, self.label)
        }
    }

    fn poll(&mut self) -> Vec<ActivityEvent> {
        match self.poll_inner() {
            Ok(events) => {
                self.last_err = None;
                events
            }
            Err(e) => {
                let err = e.to_string();
                if self.last_err.as_deref() != Some(err.as_str()) {
                    tracing::warn!(
                        "browser history {} ({}): {err}",
                        self.browser_id(),
                        self.db.display()
                    );
                    self.last_err = Some(err);
                }
                Vec::new()
            }
        }
    }

    fn poll_inner(&mut self) -> Result<Vec<ActivityEvent>, BoxError> {
        let copy = TempCopy::make(&self.db, self.family)?;
        let conn = Connection::open_with_flags(
            copy.db_path(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.pragma_update(None, "query_only", true)?;
        let rows = query_visits(&conn, self.family, self.cursor)?;
        if let Some(last) = rows.last() {
            self.cursor = last.1;
        }
        Ok(rows
            .into_iter()
            .filter_map(|row| self.to_event(row))
            .collect())
    }

    fn to_event(&self, row: (i64, i64, String, String)) -> Option<ActivityEvent> {
        let (visit_id, raw_time, url, title) = row;
        let clean = clean_url(&url)?;
        let ms = match self.family {
            Family::Chromium => ms_from_chromium_us(raw_time),
            Family::Firefox => ms_from_firefox_us(raw_time),
        };
        Some(ActivityEvent {
            ts: ms_to_ts(ms),
            end_ts: None,
            repo: String::new(),
            branch: String::new(),
            kind: ActivityKind::Browse,
            ext_id: Some(format!("{}:{visit_id}", self.browser_id())),
            summary: {
                let title = title.trim();
                (!title.is_empty()).then(|| title.to_owned())
            },
            detail: Some(serde_json::json!({ "url": clean }).to_string()),
        })
    }
}

fn chromium_us_from_ms(ms: i64) -> i64 {
    (ms + WEBKIT_EPOCH_OFFSET_MS) * 1000
}

fn ms_from_chromium_us(us: i64) -> i64 {
    us / 1000 - WEBKIT_EPOCH_OFFSET_MS
}

fn firefox_us_from_ms(ms: i64) -> i64 {
    ms * 1000
}

fn ms_from_firefox_us(us: i64) -> i64 {
    us / 1000
}

fn query_visits(
    conn: &Connection,
    family: Family,
    cursor: i64,
) -> rusqlite::Result<Vec<(i64, i64, String, String)>> {
    let sql = match family {
        Family::Chromium => {
            "SELECT v.id, v.visit_time, u.url, u.title FROM visits v \
             JOIN urls u ON u.id = v.url \
             WHERE v.visit_time > ?1 ORDER BY v.visit_time LIMIT ?2"
        }
        Family::Firefox => {
            "SELECT v.id, v.visit_date, p.url, p.title FROM moz_historyvisits v \
             JOIN moz_places p ON p.id = v.place_id \
             WHERE v.visit_date > ?1 ORDER BY v.visit_date LIMIT ?2"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![cursor, BATCH], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, i64>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?.unwrap_or_default(),
        ))
    })?;
    rows.collect()
}

/// A profile database copied to a scratch path (plus its `-wal` sibling for
/// Firefox, when one exists), removed on drop along with the `-shm`
/// SQLite creates beside it.
struct TempCopy {
    paths: Vec<PathBuf>,
}

impl TempCopy {
    fn make(src: &Path, family: Family) -> std::io::Result<Self> {
        let unique = format!(
            "{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        );
        let dst = std::env::temp_dir().join(format!("chronicle-hist-{unique}"));
        std::fs::copy(src, &dst)?;
        let mut paths = vec![dst.clone()];
        if family == Family::Firefox {
            let wal_src = with_suffix(src, "-wal");
            if wal_src.is_file() {
                let wal_dst = with_suffix(&dst, "-wal");
                if std::fs::copy(&wal_src, &wal_dst).is_ok() {
                    paths.push(wal_dst);
                }
            }
        }
        Ok(Self { paths })
    }

    fn db_path(&self) -> &Path {
        &self.paths[0]
    }
}

impl Drop for TempCopy {
    fn drop(&mut self) {
        for p in &self.paths {
            let _ = std::fs::remove_file(p);
        }
        // SQLite creates a `-shm` beside a WAL-mode copy on open; it is not
        // in `paths`, so remove it explicitly.
        let _ = std::fs::remove_file(with_suffix(self.db_path(), "-shm"));
    }
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// `scheme://host[:port]/path`: the query string, fragment and any
/// userinfo (`user:pass@`) are dropped, and the path is capped at
/// [`MAX_PATH_CHARS`]. `None` for [`DROPPED_SCHEMES`] and anything that
/// isn't `http(s):` or `file:` — those two (localhost included) are the
/// only schemes a history collector cares about.
pub fn clean_url(url: &str) -> Option<String> {
    let url = url.trim();
    let (scheme, rest) = url.split_once(':')?;
    let scheme = scheme.to_ascii_lowercase();
    if DROPPED_SCHEMES.contains(&scheme.as_str()) {
        return None;
    }
    if scheme != "http" && scheme != "https" && scheme != "file" {
        return None;
    }
    let rest = rest.strip_prefix("//").unwrap_or(rest);
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let (authority, mut path) = match rest.split_once('/') {
        Some((a, p)) => (a, format!("/{p}")),
        None => (rest, String::new()),
    };
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    if path.chars().count() > MAX_PATH_CHARS {
        path = path.chars().take(MAX_PATH_CHARS).collect();
    }
    Some(format!("{scheme}://{host}{path}"))
}

/// Chromium and Firefox profile databases found under `home`, for the
/// Settings page.
pub fn profiles(home: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();
    for &(dir, browser) in CHROMIUM_DIRS {
        let base = home.join(".config").join(dir);
        for profile_dir in chromium_profile_dirs(&base) {
            let db = base.join(&profile_dir).join("History");
            if db.is_file() {
                out.push((browser.to_owned(), db));
            }
        }
    }
    for &(dir, browser) in CHROMIUM_DIRS_MACOS {
        let base = home.join("Library/Application Support").join(dir);
        for profile_dir in chromium_profile_dirs(&base) {
            let db = base.join(&profile_dir).join("History");
            if db.is_file() {
                out.push((browser.to_owned(), db));
            }
        }
    }
    for &(dir, browser) in FIREFOX_DIRS {
        let base = home.join(dir);
        let Ok(entries) = std::fs::read_dir(&base) else {
            continue;
        };
        for entry in entries.flatten() {
            let db = entry.path().join("places.sqlite");
            if db.is_file() {
                out.push((browser.to_owned(), db));
            }
        }
    }
    out
}

/// Chromium profile directory names directly under a browser's `.config`
/// dir: `Default`, or `Profile N`.
fn chromium_profile_dirs(base: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(base) else {
        return out;
    };
    for entry in entries.flatten() {
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let is_numbered_profile = name
            .strip_prefix("Profile ")
            .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()));
        if name == "Default" || is_numbered_profile {
            out.push(name);
        }
    }
    out.sort();
    out
}

pub struct BrowserProvider {
    profiles: Vec<Profile>,
}

impl BrowserProvider {
    pub fn new(home: &Path) -> Self {
        let mut profiles = Vec::new();
        for &(dir, browser) in CHROMIUM_DIRS {
            let base = home.join(".config").join(dir);
            for profile_dir in chromium_profile_dirs(&base) {
                let db = base.join(&profile_dir).join("History");
                if db.is_file() {
                    let is_default = profile_dir == "Default";
                    profiles.push(Profile::new(
                        browser,
                        profile_dir,
                        is_default,
                        Family::Chromium,
                        db,
                    ));
                }
            }
        }
        for &(dir, browser) in CHROMIUM_DIRS_MACOS {
            let base = home.join("Library/Application Support").join(dir);
            for profile_dir in chromium_profile_dirs(&base) {
                let db = base.join(&profile_dir).join("History");
                if db.is_file() {
                    let is_default = profile_dir == "Default";
                    profiles.push(Profile::new(
                        browser,
                        profile_dir,
                        is_default,
                        Family::Chromium,
                        db,
                    ));
                }
            }
        }
        for &(dir, browser) in FIREFOX_DIRS {
            let base = home.join(dir);
            let mut found: Vec<(String, PathBuf)> = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&base) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    let db = path.join("places.sqlite");
                    if db.is_file() {
                        let label = path
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        found.push((label, db));
                    }
                }
            }
            // No "Default" naming convention for Firefox profile dirs: the
            // common single-profile case is the implicit default, so its
            // ext_id stays bare (`firefox:123`); a multi-profile machine
            // tags every one of them.
            let is_default = found.len() == 1;
            for (label, db) in found {
                profiles.push(Profile::new(
                    browser,
                    label,
                    is_default,
                    Family::Firefox,
                    db,
                ));
            }
        }
        Self { profiles }
    }

    fn poll(&mut self) -> Vec<ActivityEvent> {
        self.profiles.iter_mut().flat_map(Profile::poll).collect()
    }
}

impl FocusProvider for BrowserProvider {
    fn run(mut self, tx: Sender<CaptureEvent>) -> Result<(), BoxError> {
        crate::poll_loop(&tx, POLL, move || self.poll())
    }
}

#[cfg(test)]
mod tests {
    use jiff::Timestamp;
    use rusqlite::{Connection, params};

    use super::*;

    #[test]
    fn chromium_visit_time_converts_to_unix_ms() {
        // 2024-01-01T00:00:00Z in WebKit microseconds since 1601-01-01.
        assert_eq!(
            ms_from_chromium_us(13_348_540_800_000_000),
            1_704_067_200_000
        );
        assert_eq!(
            "2024-01-01T00:00:00Z"
                .parse::<Timestamp>()
                .unwrap()
                .as_millisecond(),
            1_704_067_200_000
        );
        assert_eq!(
            chromium_us_from_ms(1_704_067_200_000),
            13_348_540_800_000_000
        );
    }

    #[test]
    fn clean_url_drops_query_fragment_userinfo_and_caps_path() {
        let long_name = "a".repeat(300);
        let url = format!("https://user:pass@example.com:8443/{long_name}?x=1&y=2#frag");
        let cleaned = clean_url(&url).unwrap();
        assert!(
            cleaned.starts_with("https://example.com:8443/"),
            "{cleaned}"
        );
        assert!(
            !cleaned.contains('?') && !cleaned.contains('#'),
            "{cleaned}"
        );
        assert!(!cleaned.contains("user:pass"), "{cleaned}");
        assert!(cleaned.len() <= "https://example.com:8443/".len() + MAX_PATH_CHARS);

        assert_eq!(clean_url("about:blank"), None);
        assert_eq!(clean_url("chrome://settings"), None);
        assert_eq!(clean_url("chrome-extension://abc/page.html"), None);
        assert_eq!(clean_url("moz-extension://abc/page.html"), None);
        assert_eq!(clean_url("javascript:alert(1)"), None);
        assert_eq!(clean_url("data:text/plain;base64,eA=="), None);
        assert_eq!(clean_url("blob:https://x/uuid"), None);
        assert_eq!(
            clean_url("file:///home/x/notes.txt"),
            Some("file:///home/x/notes.txt".to_owned())
        );
        assert_eq!(
            clean_url("http://localhost:3000/app?y=1"),
            Some("http://localhost:3000/app".to_owned())
        );
    }

    fn chromium_db(path: &Path, url: &str, title: &str, visit_id: i64, visit_time: i64) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(
            "CREATE TABLE urls(id INTEGER PRIMARY KEY, url TEXT, title TEXT);
             CREATE TABLE visits(id INTEGER PRIMARY KEY, url INTEGER, visit_time INTEGER);",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO urls (id, url, title) VALUES (1, ?1, ?2)",
            params![url, title],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO visits (id, url, visit_time) VALUES (?1, 1, ?2)",
            params![visit_id, visit_time],
        )
        .unwrap();
    }

    #[test]
    fn chromium_and_firefox_profiles_emit_events_and_cursor_advances() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let now_ms = Timestamp::now().as_millisecond();

        let chrome_dir = home.join(".config/google-chrome/Default");
        std::fs::create_dir_all(&chrome_dir).unwrap();
        let chrome_db = chrome_dir.join("History");
        {
            let conn = Connection::open(&chrome_db).unwrap();
            conn.execute_batch(
                "CREATE TABLE urls(id INTEGER PRIMARY KEY, url TEXT, title TEXT);
                 CREATE TABLE visits(id INTEGER PRIMARY KEY, url INTEGER, visit_time INTEGER);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO urls VALUES (1, 'https://example.com/a?x=1#y', 'Example A')",
                [],
            )
            .unwrap();
            conn.execute("INSERT INTO urls VALUES (2, 'about:blank', '')", [])
                .unwrap();
            let t1 = chromium_us_from_ms(now_ms - 60_000);
            let t2 = chromium_us_from_ms(now_ms - 30_000);
            conn.execute("INSERT INTO visits VALUES (10, 1, ?1)", params![t1])
                .unwrap();
            conn.execute("INSERT INTO visits VALUES (11, 2, ?1)", params![t2])
                .unwrap();
        }

        let ff_dir = home.join(".mozilla/firefox/abc123.default-release");
        std::fs::create_dir_all(&ff_dir).unwrap();
        let ff_db = ff_dir.join("places.sqlite");
        {
            let conn = Connection::open(&ff_db).unwrap();
            conn.execute_batch(
                "CREATE TABLE moz_places(id INTEGER PRIMARY KEY, url TEXT, title TEXT);
                 CREATE TABLE moz_historyvisits(id INTEGER PRIMARY KEY, place_id INTEGER, visit_date INTEGER);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO moz_places VALUES (1, 'https://example.org/b', 'Example B')",
                [],
            )
            .unwrap();
            let t = firefox_us_from_ms(now_ms - 45_000);
            conn.execute(
                "INSERT INTO moz_historyvisits VALUES (5, 1, ?1)",
                params![t],
            )
            .unwrap();
        }

        let mut provider = BrowserProvider::new(home);
        assert_eq!(
            provider.profiles.len(),
            2,
            "one chromium, one firefox profile"
        );

        let events = provider.poll();
        assert_eq!(events.len(), 2, "about:blank is dropped: {events:?}");

        let chrome_ev = events
            .iter()
            .find(|e| e.ext_id.as_deref() == Some("chrome:10"))
            .expect("chrome visit");
        assert_eq!(chrome_ev.kind, ActivityKind::Browse);
        assert_eq!(chrome_ev.summary.as_deref(), Some("Example A"));
        assert_eq!(chrome_ev.repo, "");
        assert_eq!(chrome_ev.end_ts, None);
        let d: serde_json::Value =
            serde_json::from_str(chrome_ev.detail.as_deref().unwrap()).unwrap();
        assert_eq!(d["url"], "https://example.com/a");

        let ff_ev = events
            .iter()
            .find(|e| e.ext_id.as_deref() == Some("firefox:5"))
            .expect("firefox visit");
        assert_eq!(ff_ev.summary.as_deref(), Some("Example B"));
        let d: serde_json::Value = serde_json::from_str(ff_ev.detail.as_deref().unwrap()).unwrap();
        assert_eq!(d["url"], "https://example.org/b");

        assert!(
            !events
                .iter()
                .any(|e| e.ext_id.as_deref() == Some("chrome:11"))
        );

        // Cursor advanced past both visits (including the dropped one): a
        // second poll is silent.
        assert!(provider.poll().is_empty());
    }

    #[test]
    fn multi_profile_chromium_carries_the_profile_dir_in_the_ext_id() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let now_ms = Timestamp::now().as_millisecond();
        for profile in ["Default", "Profile 2"] {
            let p = home.join(".config/google-chrome").join(profile);
            std::fs::create_dir_all(&p).unwrap();
            let t = chromium_us_from_ms(now_ms - 10_000);
            chromium_db(&p.join("History"), "https://example.com/x", "X", 1, t);
        }
        let mut provider = BrowserProvider::new(home);
        let events = provider.poll();
        let ids: Vec<&str> = events.iter().filter_map(|e| e.ext_id.as_deref()).collect();
        assert!(ids.contains(&"chrome:1"), "{ids:?}");
        assert!(ids.contains(&"chrome/Profile 2:1"), "{ids:?}");
    }

    #[test]
    fn macos_chrome_and_firefox_profile_dirs_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let now_ms = Timestamp::now().as_millisecond();

        let chrome_dir = home.join("Library/Application Support/Google/Chrome/Default");
        std::fs::create_dir_all(&chrome_dir).unwrap();
        let t = chromium_us_from_ms(now_ms - 10_000);
        chromium_db(
            &chrome_dir.join("History"),
            "https://example.com/mac",
            "Mac Chrome",
            1,
            t,
        );

        let ff_dir =
            home.join("Library/Application Support/Firefox/Profiles/abc123.default-release");
        std::fs::create_dir_all(&ff_dir).unwrap();
        let ff_db = ff_dir.join("places.sqlite");
        {
            let conn = Connection::open(&ff_db).unwrap();
            conn.execute_batch(
                "CREATE TABLE moz_places(id INTEGER PRIMARY KEY, url TEXT, title TEXT);
                 CREATE TABLE moz_historyvisits(id INTEGER PRIMARY KEY, place_id INTEGER, visit_date INTEGER);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO moz_places VALUES (1, 'https://example.org/mac', 'Mac Firefox')",
                [],
            )
            .unwrap();
            let t = firefox_us_from_ms(now_ms - 5_000);
            conn.execute(
                "INSERT INTO moz_historyvisits VALUES (7, 1, ?1)",
                params![t],
            )
            .unwrap();
        }

        let found = profiles(home);
        assert!(
            found
                .iter()
                .any(|(b, p)| b == "chrome" && *p == chrome_dir.join("History")),
            "{found:?}"
        );
        assert!(
            found.iter().any(|(b, p)| b == "firefox" && *p == ff_db),
            "{found:?}"
        );

        let mut provider = BrowserProvider::new(home);
        let events = provider.poll();
        let ids: Vec<&str> = events.iter().filter_map(|e| e.ext_id.as_deref()).collect();
        assert!(ids.contains(&"chrome:1"), "{ids:?}");
        assert!(ids.contains(&"firefox:7"), "{ids:?}");
    }
}
