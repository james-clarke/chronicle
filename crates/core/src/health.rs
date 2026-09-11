//! What a connector is doing on *this* machine (m41 chunk 1).
//!
//! `connectors.rs` is data: what a connector is, and what to look at to
//! decide whether its tool is here. This module does the looking. Probes
//! evaluate against an [`Env`] rather than the ambient process, so the
//! `{config}`/`{data}` expansion is the same code on Linux, macOS and
//! Windows — and so a fixture can point every probe at a temp dir.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::config::Config;
use crate::connectors::{self, Connector, Platform, Probe, ProbeKind, Support};

/// The machine a probe is evaluated against. `daemon_up` stands in for the
/// local endpoint: core does no HTTP, and the caller already knows whether
/// the daemon answers.
#[derive(Debug, Clone)]
pub struct Env {
    pub platform: Platform,
    pub home: PathBuf,
    /// `~/.config` · `~/Library/Application Support` · `%APPDATA%`
    pub config_dir: PathBuf,
    /// `~/.local/share` · `~/Library/Application Support` · `%LOCALAPPDATA%`
    pub data_dir: PathBuf,
    /// `$PATH` plus the user bin dirs the daemon's minimal systemd PATH
    /// misses, the same list `config::resolve_command` walks.
    pub bin_dirs: Vec<PathBuf>,
    pub daemon_up: bool,
}

impl Env {
    /// This machine.
    pub fn host(daemon_up: bool) -> Env {
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map_or_else(|| PathBuf::from("/"), PathBuf::from);
        let dirs = directories::BaseDirs::new();
        let mut bin_dirs: Vec<PathBuf> = std::env::var_os("PATH")
            .map(|p| std::env::split_paths(&p).collect())
            .unwrap_or_default();
        bin_dirs.push(home.join(".local/bin"));
        bin_dirs.push(home.join(".cargo/bin"));
        Env {
            platform: Platform::HOST,
            config_dir: dirs
                .as_ref()
                .map_or_else(|| home.join(".config"), |d| d.config_dir().to_path_buf()),
            data_dir: dirs.as_ref().map_or_else(
                || home.join(".local/share"),
                |d| d.data_local_dir().to_path_buf(),
            ),
            home,
            bin_dirs,
            daemon_up,
        }
    }

    /// A machine of the given shape rooted at `root`: the fixture behind
    /// every probe test, and the only way to assert macOS and Windows path
    /// expansion from a Linux box.
    pub fn fake(platform: Platform, root: &Path) -> Env {
        let (config_dir, data_dir) = match platform {
            Platform::Linux => (root.join(".config"), root.join(".local/share")),
            Platform::MacOs => (
                root.join("Library/Application Support"),
                root.join("Library/Application Support"),
            ),
            Platform::Windows => (root.join("AppData/Roaming"), root.join("AppData/Local")),
        };
        Env {
            platform,
            home: root.to_path_buf(),
            config_dir,
            data_dir,
            bin_dirs: vec![root.join("bin")],
            daemon_up: false,
        }
    }
}

/// A probe's path pattern against this machine: `~`, `{config}` and
/// `{data}` expand, everything else is taken as written. A relative
/// pattern stays relative — it names a file inside a watched repo, not a
/// file at the root.
pub fn expand(env: &Env, pattern: &str) -> PathBuf {
    for (token, base) in [
        ("{config}", &env.config_dir),
        ("{data}", &env.data_dir),
        ("~", &env.home),
    ] {
        if pattern == token {
            return base.clone();
        }
        if let Some(rest) = pattern.strip_prefix(&format!("{token}/")) {
            return base.join(rest);
        }
    }
    PathBuf::from(pattern)
}

/// Bare command resolved against this machine's bin dirs.
pub fn which(env: &Env, cmd: &str) -> Option<PathBuf> {
    let mut names = vec![cmd.to_owned()];
    if env.platform == Platform::Windows {
        names.push(format!("{cmd}.exe"));
    }
    env.bin_dirs
        .iter()
        .flat_map(|d| names.iter().map(move |n| d.join(n)))
        .find(|p| p.is_file())
}

/// Whether a probe is worth evaluating here: an empty `platforms` means
/// every platform the connector claims.
fn applies(env: &Env, p: &Probe) -> bool {
    p.platforms.is_empty() || p.platforms.contains(&env.platform)
}

/// Does this probe pass?
pub fn probe_passes(env: &Env, cfg: &Config, p: &Probe) -> bool {
    match p.kind {
        ProbeKind::Command { cmd } => which(env, cmd).is_some(),
        ProbeKind::Path { path } => {
            let path = expand(env, path);
            if path.is_absolute() {
                path.exists()
            } else {
                // A relative pattern (`.sentryclirc`) is a file inside a
                // watched repo.
                cfg.git_repos
                    .iter()
                    .any(|r| expand(env, r).join(&path).exists())
            }
        }
        ProbeKind::Endpoint { .. } => env.daemon_up,
        ProbeKind::Config { field } => connectors::config_on(cfg, field).unwrap_or(false),
    }
}

/// Why a probe did not pass, for a `Broken` row.
fn describe(env: &Env, p: &Probe) -> String {
    match p.kind {
        ProbeKind::Command { cmd } => format!("{cmd} is not on PATH"),
        ProbeKind::Path { path } => format!("{} is missing", expand(env, path).display()),
        ProbeKind::Endpoint { route } => format!("the daemon is not serving {route}"),
        ProbeKind::Config { field } => format!("{field} is not set"),
    }
}

/// A connector's state on this machine, from "the tool is not here" to
/// "rows are arriving".
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "health", rename_all = "snake_case")]
pub enum Health {
    /// Nothing to look at: the tool is not installed, or there is nothing
    /// Chronicle could read if it were.
    Absent,
    /// The tool is on this machine and Chronicle is not reading it —
    /// switched off, or a `Planned` connector with no collector yet. This
    /// is the row that earns an integration request.
    Found,
    /// Switched on and pointed at something real, but no rows yet.
    Connected,
    Working {
        last_seen_ms: i64,
        rows_7d: i64,
    },
    /// Switched on and pointed at nothing: the session dir was deleted, the
    /// CLI was uninstalled, the daemon is down.
    Broken {
        reason: String,
    },
}

impl Health {
    /// Sort key: the rows that need attention first, `Absent` last.
    pub fn rank(&self) -> u8 {
        match self {
            Health::Broken { .. } => 0,
            Health::Working { .. } => 1,
            Health::Connected => 2,
            Health::Found => 3,
            Health::Absent => 4,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Health::Absent => "absent",
            Health::Found => "found",
            Health::Connected => "connected",
            Health::Working { .. } => "working",
            Health::Broken { .. } => "broken",
        }
    }
}

/// The word for what Chronicle is doing with this tool, for the first
/// column of `chronicle connections` and `chronicle status`.
///
/// A `Planned` connector never reports a health word: "found" would read
/// as a source that is working, when what it means is "you have this and
/// Chronicle cannot read it yet" — which is the row that earns an
/// integration request.
pub fn label(c: &Connector, h: &Health) -> &'static str {
    match c.state {
        Support::Planned if *h == Health::Absent => "planned",
        Support::Planned => "installed",
        Support::WontDo { .. } => "not planned",
        _ => h.label(),
    }
}

/// What this connector's evidence is counted in, for the line that reports
/// it: most connectors write activity rows, a few write something else.
pub fn unit(c: &Connector) -> &'static str {
    match c.id {
        "repo_discovery" => "project",
        "editor_workspaces" => "workspace",
        "link_files" => "link",
        "mcp_servers" => "server",
        _ => "row",
    }
}

/// Rows this connector put in the database: how many in the last seven
/// days, and when the newest one landed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Evidence {
    pub rows_7d: i64,
    pub last_ts: Option<i64>,
}

impl Evidence {
    fn from_row(rows_7d: Option<i64>, last_ts: Option<i64>) -> Evidence {
        Evidence {
            rows_7d: rows_7d.unwrap_or(0),
            last_ts,
        }
    }
}

/// One aggregate over a table with a "when" column: rows inside the week
/// and the newest timestamp.
fn count_since(
    conn: &Connection,
    sql: &str,
    params: &[&dyn rusqlite::ToSql],
) -> rusqlite::Result<Evidence> {
    conn.query_row(sql, params, |r| {
        Ok(Evidence::from_row(r.get(0)?, r.get(1)?))
    })
}

/// What this connector has written, keyed by connector rather than by
/// activity kind.
///
/// Most connectors are read through the `ActivityKind`s they produce — the
/// same aggregate `chronicle status` used to print per kind. Where a kind
/// has several collectors behind it, an `ext_id` predicate tells them
/// apart; where a connector's trace is not in `activity_events` at all
/// (editor workspaces, link files, the browser extension, MCP servers,
/// repo discovery) it gets its own query.
///
/// The one thing left conflated is `git_repos`: a commit the hook posted
/// and the same commit the poller saw 20 s later are one row, deduped by
/// `(kind, ext_id)` (`010_activity_events.sql`), so a repo's counts include
/// work the hook did. `git_hooks` still reads true, because a hook-stamped
/// checkout carries its own `ext_id`.
pub fn evidence(conn: Option<&Connection>, cfg: &Config, c: &Connector, now_ms: i64) -> Evidence {
    if c.id == "repo_discovery" {
        // A live filesystem scan, not a table: the discovered projects the
        // matcher would file time into right now.
        let n = crate::project::Matcher::from_config(cfg)
            .projects
            .iter()
            .filter(|p| p.discovered)
            .count() as i64;
        return Evidence {
            rows_7d: n,
            last_ts: (n > 0).then_some(now_ms),
        };
    }
    let Some(conn) = conn else {
        return Evidence::default();
    };
    let week_lo = now_ms - 7 * 86_400_000;
    let out = match c.id {
        // The extension posts page changes into `events` as `url` rows, not
        // into `activity_events` (`crates/server/src/lib.rs:365`), so it is
        // the one Browse producer the kind query would miss entirely.
        "browser_extension" => count_since(
            conn,
            "SELECT SUM(CASE WHEN ts >= ?1 THEN 1 ELSE 0 END), MAX(ts)
             FROM events WHERE kind = 'url'",
            &[&week_lo],
        ),
        "mcp_servers" => return mcp_evidence(conn, week_lo),
        "editor_workspaces" => count_since(
            conn,
            "SELECT SUM(CASE WHEN seen_ts >= ?1 THEN 1 ELSE 0 END), MAX(seen_ts)
             FROM editor_workspaces",
            &[&week_lo],
        ),
        "link_files" => count_since(
            conn,
            "SELECT SUM(CASE WHEN seen_ts >= ?1 THEN 1 ELSE 0 END), MAX(seen_ts)
             FROM repo_links",
            &[&week_lo],
        ),
        _ => {
            if c.produces.is_empty() {
                return Evidence::default();
            }
            let kinds = c
                .produces
                .iter()
                .map(|k| format!("'{}'", k.as_str()))
                .collect::<Vec<_>>()
                .join(",");
            let extra = ext_id_predicate(c.id).unwrap_or_default();
            count_since(
                conn,
                &format!(
                    "SELECT SUM(CASE WHEN COALESCE(end_ts, ts) >= ?1 THEN 1 ELSE 0 END),
                            MAX(COALESCE(end_ts, ts))
                     FROM activity_events WHERE kind IN ({kinds}){extra}"
                ),
                &[&week_lo],
            )
        }
    };
    out.unwrap_or_default()
}

/// The MCP servers that answered a test, from the probe records the panel
/// writes (`mcp_probe:<name>` in `meta`). A server Chronicle never called
/// has no record, which is `Connected`, not `Working`.
fn mcp_evidence(conn: &Connection, week_lo: i64) -> Evidence {
    let Ok(records) = crate::storage::meta_with_prefix(conn, "mcp_probe:") else {
        return Evidence::default();
    };
    let mut out = Evidence::default();
    for (_, value) in records {
        let Ok(rec) = serde_json::from_str::<serde_json::Value>(&value) else {
            continue;
        };
        if rec["ok"] != serde_json::Value::Bool(true) {
            continue;
        }
        let ts = rec["ts_ms"].as_i64().unwrap_or(0);
        if ts >= week_lo {
            out.rows_7d += 1;
        }
        out.last_ts = out.last_ts.max(Some(ts));
    }
    out
}

/// The `ext_id` shape that tells two collectors of the same kind apart, so
/// a row never claims another collector's work. Each string is appended to
/// the kind query's `WHERE`.
fn ext_id_predicate(id: &str) -> Option<String> {
    /// Every non-Claude session format, which stamps `<format>:<id>`
    /// (`crates/capture/src/ai_sessions.rs:373`); the Claude tracker writes
    /// the bare session id (:279).
    const AGENTS: &str = "(ext_id LIKE 'codex:%' OR ext_id LIKE 'gemini:%' \
         OR ext_id LIKE 'copilot:%' OR ext_id LIKE 'aider:%' \
         OR ext_id LIKE 'cline:%' OR ext_id LIKE 'opencode:%' \
         OR ext_id LIKE 'cursor:%')";
    Some(match id {
        "agent_sessions" => format!(" AND {AGENTS}"),
        "claude_code_sessions" => format!(" AND NOT {AGENTS}"),
        // Shell: atuin writes `<cwd>#<start_ms>`
        // (`crates/capture/src/shell.rs:169`), the hook prefixes its own
        // (`shell_hook.rs:69`).
        "shell_history_atuin" => " AND ext_id NOT LIKE 'shellhook:%'".to_owned(),
        "shell_hook" => " AND ext_id LIKE 'shellhook:%'".to_owned(),
        // Cwd: four collectors, one prefix each (`tmux.rs:78`,
        // `docker.rs:81`, `ports.rs:43`; the focus process tree's `cwd:`
        // rows belong to no connector).
        "tmux" => " AND ext_id LIKE 'tmux:%'".to_owned(),
        "docker_compose" => " AND ext_id LIKE 'docker:%'".to_owned(),
        "listening_ports" => " AND ext_id LIKE 'port:%'".to_owned(),
        // The hook stamps checkouts `hook:<sha>@<ts>` (`hooks.rs:254`); the
        // commits it posts are the row the poller would have written 20 s
        // later, so they stay with `git_repos`.
        "git_hooks" => " AND ext_id LIKE 'hook:%'".to_owned(),
        // Pull requests: the ext_id is the web URL, and GitLab's carries
        // `/-/merge_requests/` whatever host it is on
        // (`crates/capture/src/gitlab.rs:118`, `github.rs:94`).
        "gitlab_mrs" => " AND ext_id LIKE '%/-/merge_requests/%'".to_owned(),
        "github_prs" => " AND ext_id NOT LIKE '%/-/merge_requests/%'".to_owned(),
        // Meetings: an ICS event is `ics:<uid>` (`ics.rs:194`), a Google
        // one is the bare event id (`gcal.rs:445`).
        "calendars_ics" => " AND ext_id LIKE 'ics:%'".to_owned(),
        "google_calendar" => " AND ext_id NOT LIKE 'ics:%'".to_owned(),
        _ => return None,
    })
}

/// What this connector is doing on this machine.
pub fn health_of(
    conn: Option<&Connection>,
    cfg: &Config,
    env: &Env,
    c: &Connector,
    now_ms: i64,
) -> Health {
    if matches!(c.state, Support::WontDo { .. }) {
        return Health::Absent;
    }
    let switched = connectors::enabled(cfg, c);
    // What counts as "the tool is here" is the file and command probes.
    // Within a connector those are alternatives — five session dirs, two
    // browser profiles — so any one passing is enough.
    //
    // `Config` probes say "switched on", not "installed", and `Endpoint`
    // probes only ever fail because the daemon is stopped, which is one
    // fact about the whole product that `chronicle status` reports on its
    // first line — not a reason to call every endpoint source broken.
    let hard: Vec<&Probe> = c
        .probes
        .iter()
        .filter(|p| {
            applies(env, p) && matches!(p.kind, ProbeKind::Command { .. } | ProbeKind::Path { .. })
        })
        .collect();
    let present = if hard.is_empty() {
        // Nothing to look at: a connector with no probes is here whenever
        // Chronicle can read it at all.
        matches!(c.state, Support::Supported | Support::Partial { .. })
    } else {
        hard.iter().any(|p| probe_passes(env, cfg, p))
    };
    if !present {
        return if switched == Some(true) {
            Health::Broken {
                reason: hard
                    .iter()
                    .map(|p| describe(env, p))
                    .collect::<Vec<_>>()
                    .join(", "),
            }
        } else {
            Health::Absent
        };
    }
    if switched == Some(false) {
        return Health::Found;
    }
    // A tool that is here and has no collector: the row that earns a
    // request, never a row that claims to be reading anything.
    if c.state == Support::Planned {
        return Health::Found;
    }
    match evidence(conn, cfg, c, now_ms) {
        Evidence {
            last_ts: Some(ts),
            rows_7d,
        } => Health::Working {
            last_seen_ms: ts,
            rows_7d,
        },
        _ => Health::Connected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connectors::by_id;
    use crate::types::ActivityKind;

    fn db() -> (tempfile::TempDir, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let conn = crate::storage::open(&dir.path().join("chronicle.db")).unwrap();
        (dir, conn)
    }

    fn insert_shell(conn: &Connection, ext_id: &str, ts: i64) {
        conn.execute(
            "INSERT INTO activity_events (ts, end_ts, repo, branch, kind, ext_id)
             VALUES (?1, ?1, 'app', '', 'shell', ?2)",
            rusqlite::params![ts, ext_id],
        )
        .unwrap();
    }

    #[test]
    fn expands_config_and_data_per_platform() {
        let root = Path::new("/root");
        let linux = Env::fake(Platform::Linux, root);
        assert_eq!(
            expand(&linux, "{config}/Zed/db"),
            PathBuf::from("/root/.config/Zed/db")
        );
        assert_eq!(
            expand(&linux, "{data}/atuin"),
            PathBuf::from("/root/.local/share/atuin")
        );
        let mac = Env::fake(Platform::MacOs, root);
        assert_eq!(
            expand(&mac, "{config}/Zed/db"),
            PathBuf::from("/root/Library/Application Support/Zed/db")
        );
        let win = Env::fake(Platform::Windows, root);
        assert_eq!(
            expand(&win, "{data}/Google/Chrome/User Data"),
            PathBuf::from("/root/AppData/Local/Google/Chrome/User Data")
        );
        // `~` is the same everywhere, and anything else is taken as written.
        assert_eq!(expand(&win, "~/.codex"), PathBuf::from("/root/.codex"));
        assert_eq!(
            expand(&linux, "/proc/net/tcp"),
            PathBuf::from("/proc/net/tcp")
        );
        assert_eq!(
            expand(&linux, ".sentryclirc"),
            PathBuf::from(".sentryclirc")
        );
    }

    #[test]
    fn command_probe_walks_bin_dirs() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::fake(Platform::Linux, dir.path());
        let cfg = Config::default();
        let probe = Probe::cmd("tmux");
        assert!(!probe_passes(&env, &cfg, &probe));
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::write(dir.path().join("bin/tmux"), "").unwrap();
        assert!(probe_passes(&env, &cfg, &probe));
    }

    #[test]
    fn path_probe_relative_looks_inside_watched_repos() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::fake(Platform::Linux, dir.path());
        let mut cfg = Config::default();
        let probe = Probe::path(".sentryclirc");
        assert!(!probe_passes(&env, &cfg, &probe));
        std::fs::create_dir_all(dir.path().join("dev/app")).unwrap();
        cfg.git_repos = vec![dir.path().join("dev/app").display().to_string()];
        assert!(!probe_passes(&env, &cfg, &probe));
        std::fs::write(dir.path().join("dev/app/.sentryclirc"), "").unwrap();
        assert!(probe_passes(&env, &cfg, &probe));
    }

    #[test]
    fn endpoint_probe_is_the_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = Env::fake(Platform::Linux, dir.path());
        let cfg = Config::default();
        let probe = Probe::endpoint("/api/heartbeat");
        assert!(!probe_passes(&env, &cfg, &probe));
        env.daemon_up = true;
        assert!(probe_passes(&env, &cfg, &probe));
    }

    #[test]
    fn config_probe_reads_the_named_field() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::fake(Platform::Linux, dir.path());
        let mut cfg = Config::default();
        let probe = Probe::config("git_repos");
        assert!(!probe_passes(&env, &cfg, &probe));
        cfg.git_repos = vec!["~/dev/app".into()];
        assert!(probe_passes(&env, &cfg, &probe));
        // An unknown field is not a silent "on".
        assert!(!probe_passes(&env, &cfg, &Probe::config("no_such_field")));
    }

    /// A probe scoped to one platform is skipped on the others, which is
    /// what keeps `/proc/net/tcp` from deciding a macOS row.
    #[test]
    fn platform_scoped_probes_do_not_apply_elsewhere() {
        let dir = tempfile::tempdir().unwrap();
        let ports = by_id("listening_ports").unwrap();
        let linux_only = ports
            .probes
            .iter()
            .filter(|p| applies(&Env::fake(Platform::Linux, dir.path()), p))
            .count();
        let mac_only = ports
            .probes
            .iter()
            .filter(|p| applies(&Env::fake(Platform::MacOs, dir.path()), p))
            .count();
        assert_eq!((linux_only, mac_only), (1, 1));
    }

    /// The ladder one connector climbs as a machine gets set up.
    #[test]
    fn health_walks_absent_found_connected_working() {
        let (_tmp, conn) = db();
        let root = tempfile::tempdir().unwrap();
        let env = Env::fake(Platform::Linux, root.path());
        let mut cfg = Config::default();
        let atuin = by_id("shell_history_atuin").unwrap();
        let now = 1_000_000_000_000;
        let health = |cfg: &Config| health_of(Some(&conn), cfg, &env, atuin, now);

        // atuin is not installed and the switch is off.
        assert_eq!(health(&cfg), Health::Absent);

        // Installed, still off: the row that says "you could turn this on".
        let hist = root.path().join(".local/share/atuin/history.db");
        std::fs::create_dir_all(hist.parent().unwrap()).unwrap();
        std::fs::write(&hist, "").unwrap();
        assert_eq!(health(&cfg), Health::Found);

        // On, nothing read yet.
        cfg.shell_history = true;
        assert_eq!(health(&cfg), Health::Connected);

        // Rows arriving.
        insert_shell(&conn, "/home/j/dev/app#900", now - 3_600_000);
        assert_eq!(
            health(&cfg),
            Health::Working {
                last_seen_ms: now - 3_600_000,
                rows_7d: 1,
            }
        );

        // The history file goes away underneath a switch that is still on.
        std::fs::remove_file(&hist).unwrap();
        let Health::Broken { reason } = health(&cfg) else {
            panic!("a switched-on source pointing at nothing is broken");
        };
        assert!(reason.ends_with("atuin/history.db is missing"), "{reason}");
    }

    /// Two collectors behind one `ActivityKind` never report each other's
    /// rows (m41 chunk 1's whole point for `chronicle status`).
    #[test]
    fn collectors_sharing_a_kind_do_not_share_rows() {
        let (_tmp, conn) = db();
        let cfg = Config::default();
        let now = 1_000_000_000_000;
        insert_shell(&conn, "shellhook:app:900", now - 1000);
        insert_shell(&conn, "/home/j/dev/app#900", now - 2000);
        insert_shell(&conn, "/home/j/dev/other#901", now - 3000);

        let hook = evidence(Some(&conn), &cfg, by_id("shell_hook").unwrap(), now);
        let atuin = evidence(
            Some(&conn),
            &cfg,
            by_id("shell_history_atuin").unwrap(),
            now,
        );
        assert_eq!((hook.rows_7d, atuin.rows_7d), (1, 2));
        assert_eq!(hook.last_ts, Some(now - 1000));
        assert_eq!(atuin.last_ts, Some(now - 2000));
    }

    /// The same, for the two pairs whose rows are URLs and calendar ids
    /// rather than prefixed keys.
    #[test]
    fn pull_requests_and_meetings_split_by_their_own_shape() {
        let (_tmp, conn) = db();
        let cfg = Config::default();
        let now = 1_000_000_000_000;
        let insert = |kind: &str, ext_id: &str| {
            conn.execute(
                "INSERT INTO activity_events (ts, end_ts, repo, branch, kind, ext_id)
                 VALUES (?1, ?1, '', '', ?2, ?3)",
                rusqlite::params![now - 1000, kind, ext_id],
            )
            .unwrap();
        };
        insert("pr_authored", "https://github.com/org/app/pull/1");
        insert("pr_reviewed", "https://ghe.internal/org/app/pull/2");
        insert("pr_authored", "https://gitlab.com/g/app/-/merge_requests/3");
        insert("meeting", "ics:abcd@1234");
        insert("meeting", "0p9q8r7s6t");

        let rows = |id: &str| evidence(Some(&conn), &cfg, by_id(id).unwrap(), now).rows_7d;
        assert_eq!(rows("github_prs"), 2, "an enterprise host is still GitHub");
        assert_eq!(rows("gitlab_mrs"), 1);
        assert_eq!(rows("calendars_ics"), 1);
        assert_eq!(rows("google_calendar"), 1);
    }

    #[test]
    fn a_bare_token_expands_to_the_directory_itself() {
        let env = Env::fake(Platform::MacOs, Path::new("/root"));
        let base = PathBuf::from("/root/Library/Application Support");
        assert_eq!(expand(&env, "{config}"), base);
        assert_eq!(expand(&env, "{data}"), base);
        assert_eq!(expand(&env, "~"), PathBuf::from("/root"));
    }

    /// A `Planned` tool that is installed is `Found`, never `Connected` or
    /// `Working`: Chronicle is not reading it, whatever else is in the DB.
    #[test]
    fn planned_never_claims_to_be_reading() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::fake(Platform::Linux, dir.path());
        let cfg = Config::default();
        let ngrok = by_id("ngrok").unwrap();
        assert_eq!(ngrok.state, Support::Planned);
        assert_eq!(health_of(None, &cfg, &env, ngrok, 0), Health::Absent);
        std::fs::create_dir_all(dir.path().join("bin")).unwrap();
        std::fs::write(dir.path().join("bin/ngrok"), "").unwrap();
        let found = health_of(None, &cfg, &env, ngrok, 0);
        assert_eq!(found, Health::Found);
        // …and it says so in the word the CLI prints, which is neither
        // "found" (reads as working) nor "planned" (loses that you have it).
        assert_eq!(label(ngrok, &found), "installed");
        assert_eq!(label(ngrok, &Health::Absent), "planned");
    }

    /// A stopped daemon is one fact about the whole product, not a per-row
    /// failure: an endpoint source keeps reporting the rows it collected.
    #[test]
    fn a_stopped_daemon_does_not_break_every_endpoint_row() {
        let (_tmp, conn) = db();
        let root = tempfile::tempdir().unwrap();
        let env = Env::fake(Platform::Linux, root.path());
        assert!(!env.daemon_up);
        let cfg = Config::default();
        let hook = by_id("shell_hook").unwrap();
        assert!(cfg.shell_hook, "the fixture wants the switch on");

        // Nothing collected yet: the route is open, nobody has posted.
        assert_eq!(
            health_of(Some(&conn), &cfg, &env, hook, 0),
            Health::Connected
        );

        let now = 1_000_000_000_000;
        insert_shell(&conn, "shellhook:app:900", now - 1000);
        assert_eq!(
            health_of(Some(&conn), &cfg, &env, hook, now),
            Health::Working {
                last_seen_ms: now - 1000,
                rows_7d: 1,
            }
        );
    }

    /// A `WontDo` row is never a health state: there is nothing to connect.
    #[test]
    fn wont_do_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let env = Env::fake(Platform::Linux, dir.path());
        for c in connectors::REGISTRY
            .iter()
            .filter(|c| matches!(c.state, Support::WontDo { .. }))
        {
            assert_eq!(
                health_of(None, &Config::default(), &env, c, 0),
                Health::Absent
            );
        }
    }

    /// `chronicle status` prints connectors instead of kinds, so every kind
    /// that can reach the table has to belong to one.
    #[test]
    fn every_activity_kind_has_a_connector() {
        #[allow(dead_code)]
        fn exhaustive(k: ActivityKind) {
            // No wildcard: a new kind breaks the build here, and the list
            // below is what needs a descriptor.
            match k {
                ActivityKind::Checkout
                | ActivityKind::Commit
                | ActivityKind::AiSession
                | ActivityKind::PrAuthored
                | ActivityKind::PrReviewed
                | ActivityKind::Call
                | ActivityKind::Meeting
                | ActivityKind::Edit
                | ActivityKind::Shell
                | ActivityKind::Cwd
                | ActivityKind::Note
                | ActivityKind::Browse => {}
            }
        }
        for k in [
            ActivityKind::Checkout,
            ActivityKind::Commit,
            ActivityKind::AiSession,
            ActivityKind::PrAuthored,
            ActivityKind::PrReviewed,
            ActivityKind::Call,
            ActivityKind::Meeting,
            ActivityKind::Edit,
            ActivityKind::Shell,
            ActivityKind::Cwd,
            ActivityKind::Note,
            ActivityKind::Browse,
        ] {
            assert!(
                connectors::REGISTRY.iter().any(|c| c.produces.contains(&k)),
                "no connector produces {}",
                k.as_str()
            );
        }
    }
}
