//! Settings → Connections (m21): MCP servers with an on-demand probe, git
//! repos with resolve + last-seen status. Server edits write mcp.toml at
//! once — the daemon loads it per call, so they apply without a restart.
//! Git repos and local collector switches live in config.toml and apply on
//! the daemon's next start. m21.5: GitHub / Google Calendar / CalDAV presets
//! and `mcpServers` JSON import (config-only rows, no allowlisted calls).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc;

use chronicle_core::config::expand_home;
use chronicle_core::connectors::{self, ConnectKind, Platform};
use chronicle_core::health::{self, Health};
use chronicle_core::storage;
use chronicle_core::types::ActivityEvent;
use chronicle_core::usage::{self, Declared, Request, Tool};
use chronicle_mcp::{ActionCall, ContextCall, ImportEntry, McpConfig, ServerConfig, ServerProbe};
use eframe::egui;
use jiff::Timestamp;
use rusqlite::Connection;

use super::theme::{self, palette};
use super::timeline::ago;

/// Probe verdict cached in meta `mcp_probe:<name>` so a row keeps its
/// status across restarts. Holds no env values.
#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct ProbeRecord {
    ok: bool,
    ts_ms: i64,
    #[serde(default)]
    version: String,
    #[serde(default)]
    tools: usize,
    #[serde(default)]
    error: Option<String>,
}

enum Probe {
    Running(mpsc::Receiver<Result<ServerProbe, String>>),
    Done(ProbeRecord),
}

fn probe_key(name: &str) -> String {
    format!("mcp_probe:{name}")
}

/// Known servers the "add" menu pre-fills: everything but the secrets.
/// `hint` is the one line the user needs before "test" can pass (install,
/// auth step). Context-call args may use `{today}` / `{tomorrow}` / `{now}`
/// (local RFC 3339, expanded per call) for calendar windows.
struct Preset {
    label: &'static str,
    name: &'static str,
    command: &'static str,
    args: &'static [&'static str],
    env: &'static [&'static str],
    context_calls: &'static [(&'static str, &'static str)],
    fetch_calls: &'static [(&'static str, &'static str)],
    /// User-triggered writes (m26): tool, args template, button label.
    action_calls: &'static [(&'static str, &'static str, &'static str)],
    hint: &'static str,
}

const PRESETS: &[Preset] = &[
    Preset {
        label: "Jira (mcp-atlassian)",
        name: "jira",
        command: "uvx",
        args: &["mcp-atlassian"],
        env: &["JIRA_URL", "JIRA_USERNAME", "JIRA_API_TOKEN"],
        context_calls: &[(
            "jira_search",
            r#"{"jql": "assignee = currentUser() AND updated >= -3d ORDER BY updated DESC", "limit": 5, "fields": "key,summary,status"}"#,
        )],
        fetch_calls: &[(
            "jira_get_issue",
            r#"{"issue_key": "{ref}", "comment_limit": 10}"#,
        )],
        action_calls: &[(
            "jira_add_comment",
            r#"{"issue_key": "{key}", "comment": "{body}"}"#,
            "comment on {key}",
        )],
        hint: "API token from id.atlassian.com \u{b7} needs uvx on PATH",
    },
    Preset {
        label: "GitHub (github-mcp-server)",
        name: "github",
        command: "github-mcp-server",
        args: &[
            "stdio",
            "--toolsets",
            "pull_requests,context",
            "--read-only",
        ],
        env: &["GITHUB_PERSONAL_ACCESS_TOKEN"],
        context_calls: &[
            (
                "search_pull_requests",
                r#"{"query": "is:pr author:@me", "sort": "updated", "order": "desc", "perPage": 5, "fields": ["number", "title", "state", "updated_at", "html_url"]}"#,
            ),
            (
                "search_pull_requests",
                r#"{"query": "is:pr reviewed-by:@me", "sort": "updated", "order": "desc", "perPage": 5, "fields": ["number", "title", "state", "updated_at", "html_url"]}"#,
            ),
        ],
        fetch_calls: &[],
        action_calls: &[],
        hint: "binary from github.com/github/github-mcp-server releases on PATH \u{b7} fine-grained PAT with pull request read",
    },
    Preset {
        label: "Google Calendar (google-calendar-mcp)",
        name: "gcal",
        command: "npx",
        args: &["-y", "@cocal/google-calendar-mcp"],
        env: &["GOOGLE_OAUTH_CREDENTIALS"],
        context_calls: &[(
            "list-events",
            r#"{"calendarId": "primary", "timeMin": "{today}", "timeMax": "{tomorrow}"}"#,
        )],
        fetch_calls: &[],
        action_calls: &[],
        hint: "value = path to an OAuth desktop-client JSON; run `npx @cocal/google-calendar-mcp auth` once \u{b7} consent screen must be In production or the token dies in 7 days",
    },
    Preset {
        label: "CalDAV (caldav-mcp)",
        name: "caldav",
        command: "npx",
        args: &["-y", "caldav-mcp"],
        env: &["CALDAV_BASE_URL", "CALDAV_USERNAME", "CALDAV_PASSWORD"],
        context_calls: &[(
            "list-events",
            r#"{"start": "{today}", "end": "{tomorrow}"}"#,
        )],
        fetch_calls: &[],
        action_calls: &[],
        hint: "iCloud / Fastmail / Nextcloud with an app password \u{b7} list-events may need a calendarUrl (see list-calendars)",
    },
];

fn preset_calls(spec: &[(&str, &str)]) -> Vec<ContextCall> {
    spec.iter()
        .map(|(tool, args)| ContextCall {
            server: String::new(),
            tool: (*tool).to_owned(),
            args_json: Some((*args).to_owned()),
        })
        .collect()
}

fn preset_actions(spec: &[(&str, &str, &str)]) -> Vec<ActionCall> {
    spec.iter()
        .map(|(tool, args, label)| ActionCall {
            server: String::new(),
            tool: (*tool).to_owned(),
            args_json: Some((*args).to_owned()),
            label: (*label).to_owned(),
        })
        .collect()
}

fn is_secret(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    ["TOKEN", "SECRET", "PASSWORD", "KEY"]
        .iter()
        .any(|s| k.contains(s))
}

/// "as sam@…": the first env value whose key looks like an account.
fn account_hint(server: &ServerConfig) -> Option<&str> {
    server
        .env
        .iter()
        .find(|(k, v)| {
            let k = k.to_ascii_uppercase();
            (k.contains("USER") || k.contains("EMAIL")) && !v.is_empty()
        })
        .map(|(_, v)| v.as_str())
}

/// Directory basename — how the poller and `vcs_events` identify a repo.
fn repo_name(path: &Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

struct ServerForm {
    /// Name of the server being edited; None = adding.
    original: Option<String>,
    name: String,
    command: String,
    /// One per line.
    args: String,
    env: Vec<(String, String)>,
    enabled: bool,
    /// Allowlist entries a preset brings along (context, fetch, action);
    /// `server` is set at submit.
    preset_calls: Option<(Vec<ContextCall>, Vec<ContextCall>, Vec<ActionCall>)>,
    hint: Option<&'static str>,
    error: Option<String>,
    /// Remote servers: the shell line that prints the bearer token
    /// (`gh auth token`).
    bearer_command: String,
}

impl ServerForm {
    fn blank() -> Self {
        Self {
            original: None,
            name: String::new(),
            command: String::new(),
            args: String::new(),
            env: Vec::new(),
            enabled: true,
            preset_calls: None,
            hint: None,
            error: None,
            bearer_command: String::new(),
        }
    }

    fn from_preset(p: &Preset) -> Self {
        Self {
            original: None,
            name: p.name.to_owned(),
            command: chronicle_core::config::resolve_command(p.command),
            args: p.args.join("\n"),
            env: p
                .env
                .iter()
                .map(|k| ((*k).to_owned(), String::new()))
                .collect(),
            enabled: true,
            preset_calls: Some((
                preset_calls(p.context_calls),
                preset_calls(p.fetch_calls),
                preset_actions(p.action_calls),
            )),
            hint: Some(p.hint),
            error: None,
            bearer_command: String::new(),
        }
    }

    fn from_server(s: &ServerConfig) -> Self {
        Self {
            original: Some(s.name.clone()),
            name: s.name.clone(),
            command: s.url.clone().unwrap_or_else(|| s.command.clone()),
            bearer_command: s.bearer_command.clone().unwrap_or_default(),
            args: s.args.join("\n"),
            env: s.env.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            enabled: s.enabled,
            preset_calls: None,
            hint: None,
            error: None,
        }
    }

    fn to_server(&self) -> Result<ServerConfig, String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("name is required".into());
        }
        let command = self.command.trim();
        if command.is_empty() {
            return Err("command or https:// url is required".into());
        }
        // A URL in the command field is a remote server (m37 chunk 5).
        let (command, url) = if command.starts_with("http://") || command.starts_with("https://") {
            ("", Some(command.to_owned()))
        } else {
            (command, None)
        };
        let bearer = self.bearer_command.trim();
        let env = self
            .env
            .iter()
            .filter(|(k, _)| !k.trim().is_empty())
            .map(|(k, v)| (k.trim().to_owned(), v.clone()))
            .collect();
        Ok(ServerConfig {
            name: name.to_owned(),
            command: command.to_owned(),
            bearer_command: (url.is_some() && !bearer.is_empty()).then(|| bearer.to_owned()),
            bearer_env: None,
            url,
            args: self
                .args
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .map(String::from)
                .collect(),
            enabled: self.enabled,
            env,
        })
    }
}

enum FormAct {
    None,
    Submit,
    Cancel,
}

fn form_ui(ui: &mut egui::Ui, form: &mut ServerForm) -> FormAct {
    let mut act = FormAct::None;
    theme::card().show(ui, |ui| {
        let title = if form.original.is_some() {
            "edit server"
        } else {
            "new server"
        };
        ui.label(
            egui::RichText::new(title)
                .family(egui::FontFamily::Name(theme::MEDIUM.into()))
                .color(palette::TEXT),
        );
        if let Some(hint) = form.hint {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(hint)
                        .text_style(theme::caption())
                        .weak(),
                )
                .wrap(),
            );
        }
        ui.add_space(theme::SPACE_XS);
        ui.label("name");
        ui.add(egui::TextEdit::singleline(&mut form.name).desired_width(f32::INFINITY));
        ui.label("command, or the URL of a remote server");
        ui.add(
            egui::TextEdit::singleline(&mut form.command)
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace),
        );
        if form.command.trim_start().starts_with("http") {
            ui.label("bearer token command (your CLI's login, e.g. gh auth token)");
            ui.add(
                egui::TextEdit::singleline(&mut form.bearer_command)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("gh auth token"),
            );
        }
        ui.label("args (one per line)");
        ui.add(
            egui::TextEdit::multiline(&mut form.args)
                .desired_rows(1)
                .desired_width(f32::INFINITY)
                .font(egui::TextStyle::Monospace),
        );
        ui.label("environment");
        let mut remove: Option<usize> = None;
        let w = ui.available_width();
        for (i, (k, v)) in form.env.iter_mut().enumerate() {
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(k)
                        .desired_width((w * 0.38).min(160.0))
                        .font(egui::TextStyle::Monospace)
                        .hint_text("KEY"),
                );
                let secret = is_secret(k);
                ui.add(
                    egui::TextEdit::singleline(v)
                        .password(secret)
                        .desired_width(ui.available_width() - 32.0)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("value"),
                );
                if theme::ghost_button(ui, "\u{d7}").clicked() {
                    remove = Some(i);
                }
            });
        }
        if let Some(i) = remove {
            form.env.remove(i);
        }
        if theme::ghost_button(ui, "+ variable").clicked() {
            form.env.push((String::new(), String::new()));
        }
        ui.checkbox(&mut form.enabled, "enabled");
        match (&form.preset_calls, &form.original) {
            (Some((c, f, a)), _) => {
                ui.weak(format!(
                    "adds {} context call(s), {} fetch call(s) and {} action(s) to the allowlist",
                    c.len(),
                    f.len(),
                    a.len()
                ));
            }
            (None, Some(_)) => {
                ui.weak("allowlisted calls: edit mcp.toml");
            }
            (None, None) => {}
        }
        if let Some(e) = &form.error {
            ui.colored_label(palette::RED, e);
        }
        ui.add_space(theme::SPACE_XS);
        ui.horizontal(|ui| {
            if theme::primary_button(ui, "save").clicked() {
                act = FormAct::Submit;
            }
            if theme::ghost_button(ui, "cancel").clicked() {
                act = FormAct::Cancel;
            }
        });
    });
    act
}

pub(super) fn subhead(ui: &mut egui::Ui, title: &str, trailing: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new(title)
                .family(egui::FontFamily::Name(theme::MEDIUM.into()))
                .color(palette::TEXT),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), trailing);
    });
}

/// Secret shown as a hint, not a value: last four characters only.
fn masked(key: &str) -> String {
    let tail: String = key
        .chars()
        .skip(key.chars().count().saturating_sub(4))
        .collect();
    format!("{}{tail}", "\u{2022}".repeat(8))
}

fn caption(ui: &mut egui::Ui, text: String, color: Option<egui::Color32>) -> egui::Response {
    let mut rich = egui::RichText::new(text).text_style(theme::caption());
    rich = match color {
        Some(c) => rich.color(c),
        None => rich.weak(),
    };
    ui.add(egui::Label::new(rich).truncate())
}

/// Caption whose hover shows `hover` (a full error) instead of egui's own
/// elided-text tooltip — one tooltip, not two.
fn caption_hover(ui: &mut egui::Ui, text: String, color: egui::Color32, hover: String) {
    let rich = egui::RichText::new(text)
        .text_style(theme::caption())
        .color(color);
    ui.add(
        egui::Label::new(rich)
            .truncate()
            .show_tooltip_when_elided(false),
    )
    .on_hover_text(hover);
}

/// Status dot + chip for a server row.
fn row_status(
    server: &ServerConfig,
    probe: Option<&Probe>,
) -> (egui::Color32, String, egui::Color32) {
    if !server.enabled {
        return (palette::AMBER, "disabled".into(), palette::AMBER);
    }
    match probe {
        Some(Probe::Running(_)) => (
            palette::TEXT_DIM,
            "testing\u{2026}".into(),
            palette::TEXT_DIM,
        ),
        Some(Probe::Done(rec)) if rec.ok => (
            palette::GREEN,
            format!("{} tools", rec.tools),
            palette::GREEN,
        ),
        Some(Probe::Done(_)) => (palette::RED, "failed".into(), palette::RED),
        None => (palette::AMBER, "not tested".into(), palette::AMBER),
    }
}

/// Connectors whose rows are drawn by a section of their own: the repo
/// list above, and the MCP server list. The registry still carries them —
/// `chronicle connections` and the site want one table — but the panel
/// would draw them twice.
const DRAWN_ELSEWHERE: &[&str] = &["git_repos", "git_hooks"];

/// The mined list's window and length (m41 chunk 3).
const MINED_DAYS: i64 = 30;
const MINED_ROWS: usize = 12;
const REQUEST_HOVER: &str =
    "open a prefilled GitHub issue in your browser \u{2014} Chronicle itself sends nothing";

/// The compose box behind every "request" button (m41 chunk 4): the
/// request being edited, each evidence line as a chip the person can drop,
/// and what happened to it. Nothing here leaves the machine until the
/// person's own browser opens the issue.
struct RequestForm {
    req: Request,
    /// Evidence lines (redacted) and whether they stay in the body.
    chips: Vec<(String, bool)>,
    status: Option<Result<String, String>>,
    /// The composed counter was bumped for this form.
    counted: bool,
}

impl RequestForm {
    fn new(mut req: Request) -> Self {
        // Paths, hostnames, query strings and anything token-shaped come
        // out before the person sees the line; what they see is what goes.
        let chips = req
            .evidence
            .drain(..)
            .map(|l| (chronicle_derive::redact::redact(&l).text, true))
            .collect();
        Self {
            req,
            chips,
            status: None,
            counted: false,
        }
    }

    /// The request as it stands: kept chips only.
    fn current(&self) -> Request {
        let mut r = self.req.clone();
        r.evidence = self
            .chips
            .iter()
            .filter(|(_, keep)| *keep)
            .map(|(l, _)| l.clone())
            .collect();
        r
    }
}

/// Every `config.toml` switch a descriptor can name, so a row's toggle can
/// borrow one list instead of a different field of `LocalSources` each
/// time. `ai_session_dirs` is here as a bool: it is a list in the config,
/// and switching it back on restores the default dir.
const SWITCHES: &[&str] = &[
    "ai_session_dirs",
    "github_prs",
    "gitlab_mrs",
    "mic_capture",
    "google_calendar",
    "shell_history",
    "editor_heartbeats",
    "shell_hook",
    "discover_repos",
    "browser_history",
];

/// The bool behind one switch name. `ai_session_dirs` has none — it is a
/// list, handled where the rows are written back.
fn switch<'a>(src: &'a mut LocalSources, field: &str) -> Option<&'a mut bool> {
    Some(match field {
        "github_prs" => &mut src.github_prs,
        "gitlab_mrs" => &mut src.gitlab_mrs,
        "mic_capture" => &mut src.mic_capture,
        "google_calendar" => &mut src.google_calendar,
        "shell_history" => &mut src.shell_history,
        "editor_heartbeats" => &mut src.editor_heartbeats,
        "shell_hook" => &mut src.shell_hook,
        "discover_repos" => &mut src.discover_repos,
        "browser_history" => &mut src.browser_history,
        _ => return None,
    })
}

/// Local collector switches (m22), mirrored from config.toml like
/// `git_repos`. Sessions are on iff `ai_session_dirs` is non-empty; the
/// toggle restores the default dir when switching back on.
pub(super) struct LocalSources {
    pub ai_session_dirs: Vec<String>,
    pub github_prs: bool,
    pub gitlab_mrs: bool,
    pub mic_capture: bool,
    pub shell_history: bool,
    pub shell_hook: bool,
    pub google_calendar: bool,
    pub editor_heartbeats: bool,
    pub browser_history: bool,
    pub discover_repos: bool,
    pub calendars: Vec<String>,
}

impl LocalSources {
    pub(super) fn from_config(c: &chronicle_core::config::Config) -> Self {
        Self {
            ai_session_dirs: c.ai_session_dirs.clone(),
            github_prs: c.github_prs,
            gitlab_mrs: c.gitlab_mrs,
            mic_capture: c.mic_capture,
            shell_history: c.shell_history,
            shell_hook: c.shell_hook,
            google_calendar: c.google_calendar,
            editor_heartbeats: c.editor_heartbeats,
            browser_history: c.browser_history,
            discover_repos: c.discover_repos,
            calendars: c.calendars.clone(),
        }
    }

    pub(super) fn apply(&self, c: &mut chronicle_core::config::Config) {
        c.ai_session_dirs = self.ai_session_dirs.clone();
        c.github_prs = self.github_prs;
        c.gitlab_mrs = self.gitlab_mrs;
        c.mic_capture = self.mic_capture;
        c.shell_history = self.shell_history;
        c.shell_hook = self.shell_hook;
        c.google_calendar = self.google_calendar;
        c.editor_heartbeats = self.editor_heartbeats;
        c.browser_history = self.browser_history;
        c.discover_repos = self.discover_repos;
        c.calendars = self.calendars.clone();
    }
}

/// `resolve_git_dir(repo).is_some()` per repo, index aligned with `repos`.
fn repo_status(repos: &[String]) -> Vec<bool> {
    repos
        .iter()
        .map(|p| chronicle_capture::git::resolve_git_dir(&expand_home(p)).is_some())
        .collect()
}

/// First `ai_session_dirs` entry that doesn't exist, if any.
fn missing_session_dir(dirs: &[String]) -> Option<String> {
    dirs.iter().find(|d| !expand_home(d).is_dir()).cloned()
}

/// `mcpServers` JSON import: a path, the entries it parsed, a tick per row.
struct ImportState {
    path: String,
    entries: Vec<(ImportEntry, bool)>,
    error: Option<String>,
}

impl ImportState {
    fn new() -> Self {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let candidates = [
            "~/.claude.json",
            "~/.config/Claude/claude_desktop_config.json",
            "~/.cursor/mcp.json",
            ".mcp.json",
        ];
        let path = candidates
            .iter()
            .find(|c| {
                let p = expand_home(c);
                home.is_some() && p.is_file()
            })
            .map_or_else(|| candidates[0].to_owned(), |c| (*c).to_owned());
        Self {
            path,
            entries: Vec::new(),
            error: None,
        }
    }

    /// Parse the file; rows already configured or unsupported start unticked.
    fn load(&mut self, existing: &[ServerConfig]) {
        self.entries.clear();
        let path = expand_home(self.path.trim());
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                self.error = Some(format!("{}: {e}", path.display()));
                return;
            }
        };
        match chronicle_mcp::parse_mcp_servers_json(&text) {
            Ok(entries) => {
                self.error = None;
                for mut e in entries {
                    if e.skip.is_none() && existing.iter().any(|s| s.name == e.server.name) {
                        e.skip = Some("already configured".into());
                    }
                    if !e.server.command.is_empty() {
                        e.server.command =
                            chronicle_core::config::resolve_command(&e.server.command);
                    }
                    let tick = e.skip.is_none();
                    self.entries.push((e, tick));
                }
                if self.entries.is_empty() {
                    self.error = Some("no servers in that file".into());
                }
            }
            Err(e) => self.error = Some(e),
        }
    }
}

enum RowAct {
    Test(String),
    Edit(String),
    Arm(String),
    Remove(String),
}

pub(super) struct Connections {
    mcp_path: PathBuf,
    mcp: McpConfig,
    /// mcp.toml failed to load: shown in place of the list and edits are
    /// off — a save would clobber the hand-written file.
    mcp_error: Option<String>,
    probes: BTreeMap<String, Probe>,
    form: Option<ServerForm>,
    import: Option<ImportState>,
    arm_remove: Option<String>,
    mcp_status: Option<Result<String, String>>,
    /// Newest `fetch_context` job: (status, created, error).
    last_fetch: Option<(String, Timestamp, Option<String>)>,
    /// Newest vcs event per repo basename.
    repo_last: BTreeMap<String, ActivityEvent>,
    /// What each connector is doing on this machine (m41 chunk 1): probes
    /// and row counts, read at load rather than per frame.
    health: BTreeMap<&'static str, Health>,
    /// Key WakaTime plugins authenticate with (meta `wakapi_api_key`).
    wakapi_key: Option<String>,
    repo_add: String,
    repo_error: Option<String>,
    repo_arm_remove: Option<usize>,
    /// `google.toml` as found at load: `None` = not signed in, the string is
    /// the account (empty when the login could not read it).
    google_account: Option<String>,
    /// `resolve_git_dir(repo).is_some()` per `git_repos` entry (index
    /// aligned); refreshed at load and after add/remove, not per frame.
    repo_resolved: Vec<bool>,
    /// Session formats with a directory on this machine (m37 chunk 0).
    session_formats: Vec<&'static str>,
    /// Browser profiles with a history DB (m37 chunk 4), by browser name.
    browser_profiles: Vec<String>,
    /// Chronicle git hooks installed per repo path (m37 chunk 2).
    repo_hooks: BTreeMap<String, Vec<&'static str>>,
    /// Discovered projects the matcher added (m37 chunk 2).
    discovered: Vec<String>,
    /// Link rows and editor workspaces stored (m37 chunks 2–3).
    link_rows: Vec<chronicle_core::storage::RepoLinkRow>,
    workspace_rows: Vec<chronicle_core::storage::WorkspaceRow>,
    /// First `ai_session_dirs` entry that doesn't exist, if any; refreshed
    /// at load and whenever the sessions toggle changes the dirs.
    session_dir_missing: Option<String>,
    /// Apps and domains in focus over the last 30 days (m41 chunk 3),
    /// ranked by minutes, read at load.
    mined: Vec<Tool>,
    /// What the person ticked as "I use this" (meta `declared_tools`).
    declared: Declared,
    declare_open: bool,
    declare_filter: String,
    /// Some = the request compose box is open (m41 chunk 4).
    request: Option<RequestForm>,
    /// `{data_dir}/requests`, where "save to file" writes.
    requests_dir: PathBuf,
}

impl Connections {
    pub(super) fn load(
        mcp_path: PathBuf,
        data_dir: &Path,
        conn: Option<&Connection>,
        git_repos: &[String],
        ai_session_dirs: &[String],
    ) -> Self {
        let (mcp, mcp_error) = match McpConfig::load(&mcp_path) {
            Ok(cfg) => (cfg, None),
            Err(e) => (McpConfig::default(), Some(e.to_string())),
        };
        let mut probes = BTreeMap::new();
        if let Some(conn) = conn {
            for s in &mcp.servers {
                if let Ok(Some(json)) = storage::get_meta(conn, &probe_key(&s.name))
                    && let Ok(rec) = serde_json::from_str::<ProbeRecord>(&json)
                {
                    probes.insert(s.name.clone(), Probe::Done(rec));
                }
            }
        }
        let last_fetch =
            conn.and_then(|c| storage::latest_ai_job(c, "fetch_context").ok().flatten());
        let repo_last = conn
            .and_then(|c| storage::latest_vcs_event_per_repo(c).ok())
            .unwrap_or_default()
            .into_iter()
            .map(|e| (e.repo.clone(), e))
            .collect();
        let wakapi_key = conn.and_then(|c| storage::get_meta(c, "wakapi_api_key").ok().flatten());
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let config =
            chronicle_core::config::Config::load(&data_dir.join("config.toml")).unwrap_or_default();
        let discovered: Vec<String> = chronicle_core::project::Matcher::from_config(&config)
            .projects
            .iter()
            .filter(|p| p.discovered)
            .map(|p| p.name.clone())
            .collect();
        // `daemon_up` is false because health does not read endpoint
        // probes: whether the daemon answers is one fact about the whole
        // product, reported on Home, not a per-row verdict.
        let env = health::Env::host(false);
        let now_ms = Timestamp::now().as_millisecond();
        let health = connectors::for_platform(Platform::HOST)
            .map(|c| (c.id, health::health_of(conn, &config, &env, c, now_ms)))
            .collect();
        Self {
            mcp_path,
            mcp,
            mcp_error,
            probes,
            form: None,
            import: None,
            arm_remove: None,
            mcp_status: None,
            last_fetch,
            repo_last,
            health,
            wakapi_key,
            repo_add: String::new(),
            repo_error: None,
            repo_arm_remove: None,
            google_account: chronicle_capture::gcal::Tokens::load(
                &chronicle_capture::gcal::token_path(data_dir),
            )
            .ok()
            .map(|t| t.email),
            repo_resolved: repo_status(git_repos),
            session_formats: home
                .as_deref()
                .map(|h| {
                    chronicle_capture::ai_sessions::AiSessionProvider::detected(
                        h,
                        &chronicle_capture::sessions::all(),
                    )
                })
                .unwrap_or_default(),
            browser_profiles: home
                .as_deref()
                .map(|h| {
                    let mut v: Vec<String> = chronicle_capture::browser::profiles(h)
                        .into_iter()
                        .map(|(b, _)| b)
                        .collect();
                    v.dedup();
                    v
                })
                .unwrap_or_default(),
            repo_hooks: git_repos
                .iter()
                .map(|r| {
                    let st = chronicle_capture::hooks::status(&expand_home(r));
                    (r.clone(), st.installed)
                })
                .collect(),
            discovered,
            link_rows: conn
                .and_then(|c| storage::repo_links(c).ok())
                .unwrap_or_default(),
            workspace_rows: conn
                .and_then(|c| storage::editor_workspaces(c).ok())
                .unwrap_or_default(),
            session_dir_missing: missing_session_dir(ai_session_dirs),
            mined: conn
                .and_then(|c| {
                    usage::mined(
                        c,
                        &config,
                        &jiff::tz::TimeZone::system(),
                        now_ms,
                        MINED_DAYS,
                    )
                    .ok()
                })
                .unwrap_or_default(),
            declared: conn.map(Declared::load).unwrap_or_default(),
            declare_open: false,
            declare_filter: String::new(),
            request: None,
            requests_dir: data_dir.join("requests"),
        }
    }

    /// Re-derives `repo_resolved` after an add/remove; O(repos), not per
    /// frame.
    fn refresh_repo_status(&mut self, git_repos: &[String]) {
        self.repo_resolved = repo_status(git_repos);
    }

    pub(super) fn ui(
        &mut self,
        ui: &mut egui::Ui,
        conn: Option<&Connection>,
        git_repos: &mut Vec<String>,
        sources: &mut LocalSources,
    ) {
        self.poll_probes(conn);
        self.servers_ui(ui, conn);
        self.actions_ui(ui);
        ui.add_space(theme::SPACE_SM);
        self.repos_ui(ui, git_repos);
        ui.add_space(theme::SPACE_SM);
        self.sources_ui(ui, sources);
        self.worklist_ui(ui, conn);
        self.request_ui(ui, conn);
        caption(
            ui,
            "repo and source changes apply on the daemon's next start".to_owned(),
            None,
        );
    }

    fn poll_probes(&mut self, conn: Option<&Connection>) {
        for (name, probe) in self.probes.iter_mut() {
            let Probe::Running(rx) = probe else {
                continue;
            };
            let Ok(result) = rx.try_recv() else {
                continue;
            };
            let ts_ms = Timestamp::now().as_millisecond();
            let rec = match result {
                Ok(p) => ProbeRecord {
                    ok: true,
                    ts_ms,
                    version: format!("{} {}", p.server_name, p.server_version)
                        .trim()
                        .to_owned(),
                    tools: p.tools.len(),
                    error: None,
                },
                Err(e) => ProbeRecord {
                    ok: false,
                    ts_ms,
                    version: String::new(),
                    tools: 0,
                    error: Some(e),
                },
            };
            if let (Some(conn), Ok(json)) = (conn, serde_json::to_string(&rec)) {
                let _ = storage::set_meta(conn, &probe_key(name), Some(&json));
            }
            *probe = Probe::Done(rec);
        }
    }

    fn start_probe(&mut self, ctx: &egui::Context, server: ServerConfig) {
        let (tx, rx) = mpsc::channel();
        let name = server.name.clone();
        let ctx = ctx.clone();
        std::thread::Builder::new()
            .name("mcp-probe".into())
            .spawn(move || {
                let result = chronicle_mcp::probe_server(&server).map_err(|e| format!("{e:#}"));
                let _ = tx.send(result);
                ctx.request_repaint();
            })
            .expect("spawn mcp-probe thread");
        self.probes.insert(name, Probe::Running(rx));
    }

    fn servers_ui(&mut self, ui: &mut egui::Ui, conn: Option<&Connection>) {
        let mut open_form: Option<ServerForm> = None;
        let mut open_import = false;
        let editable = self.mcp_error.is_none();
        subhead(ui, "MCP servers", |ui| {
            if editable {
                ui.menu_button("add", |ui| {
                    for p in PRESETS {
                        if ui.button(p.label).clicked() {
                            open_form = Some(ServerForm::from_preset(p));
                            ui.close();
                        }
                    }
                    if ui.button("custom").clicked() {
                        open_form = Some(ServerForm::blank());
                        ui.close();
                    }
                    ui.separator();
                    if ui.button("import mcpServers JSON\u{2026}").clicked() {
                        open_import = true;
                        ui.close();
                    }
                });
            }
        });
        if let Some(f) = open_form {
            self.form = Some(f);
            self.import = None;
            self.arm_remove = None;
        }
        if open_import {
            self.form = None;
            self.import = Some(ImportState::new());
            self.arm_remove = None;
        }
        if let Some(err) = &self.mcp_error {
            ui.colored_label(palette::RED, format!("mcp.toml: {err}"));
            ui.weak("fix the file by hand; edits here are off until it loads");
            return;
        }
        if self.mcp.servers.is_empty() && self.form.is_none() {
            ui.weak("no servers \u{2014} add one to pull ticket context into task workspaces");
        }
        let width = ui.available_width();
        let mut act: Option<RowAct> = None;
        for server in &self.mcp.servers {
            let probe = self.probes.get(&server.name);
            let armed = self.arm_remove.as_deref() == Some(server.name.as_str());
            let running = matches!(probe, Some(Probe::Running(_)));
            let (dot, chip, chip_color) = row_status(server, probe);
            let name = &server.name;
            theme::ListRow::new(name)
                .emphasis()
                .dot(dot)
                .chip(chip, chip_color)
                .show(ui, width, |ui| {
                    let label = if armed { "delete?" } else { "\u{d7}" };
                    if theme::ghost_button(ui, label).clicked() {
                        act = Some(if armed {
                            RowAct::Remove(name.clone())
                        } else {
                            RowAct::Arm(name.clone())
                        });
                    }
                    if theme::ghost_button(ui, "edit").clicked() {
                        act = Some(RowAct::Edit(name.clone()));
                    }
                    if running {
                        ui.spinner();
                    } else if theme::ghost_button(ui, "test").clicked() {
                        act = Some(RowAct::Test(name.clone()));
                    }
                });
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.vertical(|ui| {
                    let mut line = server.url.clone().unwrap_or_else(|| server.command.clone());
                    for a in &server.args {
                        line.push(' ');
                        line.push_str(a);
                    }
                    if let Some(acct) = account_hint(server) {
                        line.push_str(&format!("  \u{b7}  as {acct}"));
                    }
                    caption(ui, line, None);
                    match probe {
                        Some(Probe::Done(rec)) if rec.ok => {
                            let mut text = String::from("connected");
                            if !rec.version.is_empty() {
                                text.push_str(&format!(" \u{b7} {}", rec.version));
                            }
                            text.push_str(&format!(" \u{b7} checked {}", ago(rec.ts_ms)));
                            caption(ui, text, None);
                        }
                        Some(Probe::Done(rec)) => {
                            let err = rec.error.clone().unwrap_or_default();
                            let first = err.lines().next().unwrap_or("").to_owned();
                            caption_hover(
                                ui,
                                format!("failed {}: {first}", ago(rec.ts_ms)),
                                palette::RED,
                                err,
                            );
                        }
                        _ => {}
                    }
                });
            });
        }
        match act {
            Some(RowAct::Arm(n)) => self.arm_remove = Some(n),
            Some(RowAct::Remove(n)) => {
                self.arm_remove = None;
                self.mcp_status = Some(
                    self.remove_server(&n, conn)
                        .map(|()| format!("removed {n}")),
                );
            }
            Some(RowAct::Edit(n)) => {
                self.arm_remove = None;
                if let Some(s) = self.mcp.servers.iter().find(|s| s.name == n) {
                    self.form = Some(ServerForm::from_server(s));
                }
            }
            Some(RowAct::Test(n)) => {
                let ctx = ui.ctx().clone();
                if let Some(s) = self.mcp.servers.iter().find(|s| s.name == n).cloned() {
                    self.start_probe(&ctx, s);
                }
            }
            None => {}
        }
        // Passive signal: the newest per-task context fetch, whichever
        // server's allowlist ran it.
        if !self.mcp.servers.is_empty() {
            match &self.last_fetch {
                Some((status, ts, err)) if status == "failed" => {
                    let err = err.clone().unwrap_or_default();
                    let first = err.lines().next().unwrap_or("").to_owned();
                    caption_hover(
                        ui,
                        format!(
                            "last context fetch failed {}: {first}",
                            ago(ts.as_millisecond())
                        ),
                        palette::RED,
                        err,
                    );
                }
                Some((status, ts, _)) => {
                    caption(
                        ui,
                        format!(
                            "last context fetch {status} \u{b7} {}",
                            ago(ts.as_millisecond())
                        ),
                        None,
                    );
                }
                None => {
                    caption(
                        ui,
                        "no context fetched yet \u{2014} runs when a task gets a ticket key".into(),
                        None,
                    );
                }
            }
        }
        match &self.mcp_status {
            Some(Ok(msg)) => {
                ui.weak(msg.as_str());
            }
            Some(Err(msg)) => {
                ui.colored_label(palette::RED, msg);
            }
            None => {}
        }
        // The add/edit form sits under the list.
        let form_act = match &mut self.form {
            Some(form) => {
                ui.add_space(theme::SPACE_XS);
                form_ui(ui, form)
            }
            None => FormAct::None,
        };
        match form_act {
            FormAct::Submit => match self.apply_form() {
                Ok(name) => self.mcp_status = Some(Ok(format!("saved {name}"))),
                Err(e) => {
                    if let Some(form) = &mut self.form {
                        form.error = Some(e);
                    }
                }
            },
            FormAct::Cancel => self.form = None,
            FormAct::None => {}
        }
        self.import_ui(ui);
    }

    /// Import panel under the list: path + load, one tick per parsed server,
    /// import writes the ticked ones as config-only rows (no allowlisted
    /// calls — those stay a per-server decision in mcp.toml or a preset).
    fn import_ui(&mut self, ui: &mut egui::Ui) {
        let Some(state) = &mut self.import else {
            return;
        };
        let mut close = false;
        let mut import = false;
        ui.add_space(theme::SPACE_XS);
        theme::card().show(ui, |ui| {
            ui.label(
                egui::RichText::new("import mcpServers JSON")
                    .family(egui::FontFamily::Name(theme::MEDIUM.into()))
                    .color(palette::TEXT),
            );
            ui.horizontal(|ui| {
                let w = ui.available_width();
                ui.add(
                    egui::TextEdit::singleline(&mut state.path)
                        .desired_width(w - 56.0)
                        .font(egui::TextStyle::Monospace)
                        .hint_text("~/.claude.json"),
                );
                if theme::secondary_button(ui, "load").clicked() {
                    state.load(&self.mcp.servers);
                }
            });
            if let Some(e) = &state.error {
                ui.colored_label(palette::RED, e);
            }
            let width = ui.available_width();
            for (entry, tick) in state.entries.iter_mut() {
                let name = entry.server.name.clone();
                let (dot, chip, color) = match &entry.skip {
                    Some(why) => (palette::AMBER, why.clone(), palette::AMBER),
                    None => (palette::GREEN, "stdio".to_owned(), palette::TEXT_DIM),
                };
                theme::ListRow::new(&name)
                    .emphasis()
                    .dot(dot)
                    .chip(chip, color)
                    .show(ui, width, |ui| {
                        ui.add_enabled(entry.skip.is_none(), egui::Checkbox::without_text(tick));
                    });
                let mut line = entry
                    .server
                    .url
                    .clone()
                    .unwrap_or_else(|| entry.server.command.clone());
                for a in &entry.server.args {
                    line.push(' ');
                    line.push_str(a);
                }
                if !entry.server.env.is_empty() {
                    line.push_str(&format!("  \u{b7}  {} env", entry.server.env.len()));
                }
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    caption(ui, line, None);
                });
            }
            let n = state.entries.iter().filter(|(_, t)| *t).count();
            ui.add_space(theme::SPACE_XS);
            ui.horizontal(|ui| {
                if theme::primary_button_enabled(ui, n > 0, &format!("import {n}")).clicked() {
                    import = true;
                }
                if theme::ghost_button(ui, "cancel").clicked() {
                    close = true;
                }
            });
        });
        if import {
            let picked: Vec<ServerConfig> = state
                .entries
                .iter()
                .filter(|(e, t)| *t && e.skip.is_none())
                .map(|(e, _)| e.server.clone())
                .collect();
            let mut next = self.mcp.clone();
            next.servers.extend(picked.iter().cloned());
            self.mcp_status = Some(match next.save(&self.mcp_path) {
                Ok(()) => {
                    self.mcp = next;
                    close = true;
                    Ok(format!(
                        "imported {} \u{2014} add allowlisted calls in mcp.toml to use them",
                        picked.len()
                    ))
                }
                Err(e) => Err(e.to_string()),
            });
        }
        if close {
            self.import = None;
        }
    }

    /// Add or replace a server (renaming rewrites its calls) and write the
    /// file. On failure the form comes back with the user's edits intact.
    /// Add or replace the form's server (renaming rewrites its calls) and
    /// write the file; the form closes on success and keeps the user's
    /// edits on failure.
    fn apply_form(&mut self) -> Result<String, String> {
        let Some(form) = &self.form else {
            return Err("no form open".into());
        };
        let server = form.to_server()?;
        let mut next = self.mcp.clone();
        let name = server.name.clone();
        match &form.original {
            Some(old) => {
                let Some(idx) = next.servers.iter().position(|s| &s.name == old) else {
                    return Err("server no longer exists".into());
                };
                if old != &name && next.servers.iter().any(|s| s.name == name) {
                    return Err(format!("a server named {name} already exists"));
                }
                if old != &name {
                    for c in next
                        .context_calls
                        .iter_mut()
                        .chain(next.fetch_calls.iter_mut())
                    {
                        if &c.server == old {
                            c.server = name.clone();
                        }
                    }
                    for a in next.action_calls.iter_mut() {
                        if &a.server == old {
                            a.server = name.clone();
                        }
                    }
                    self.probes.remove(old);
                }
                next.servers[idx] = server;
            }
            None => {
                if next.servers.iter().any(|s| s.name == name) {
                    return Err(format!("a server named {name} already exists"));
                }
                next.servers.push(server);
                if let Some((ctx_calls, fetch_calls, action_calls)) = &form.preset_calls {
                    let with_server = |c: &ContextCall| ContextCall {
                        server: name.clone(),
                        ..c.clone()
                    };
                    next.context_calls.extend(ctx_calls.iter().map(with_server));
                    next.fetch_calls.extend(fetch_calls.iter().map(with_server));
                    next.action_calls
                        .extend(action_calls.iter().map(|a| ActionCall {
                            server: name.clone(),
                            ..a.clone()
                        }));
                }
            }
        }
        next.save(&self.mcp_path).map_err(|e| e.to_string())?;
        self.probes.remove(&name);
        self.mcp = next;
        self.form = None;
        Ok(name)
    }

    fn remove_server(&mut self, name: &str, conn: Option<&Connection>) -> Result<(), String> {
        let mut next = self.mcp.clone();
        next.servers.retain(|s| s.name != name);
        next.context_calls.retain(|c| c.server != name);
        next.fetch_calls.retain(|c| c.server != name);
        next.action_calls.retain(|a| a.server != name);
        next.save(&self.mcp_path).map_err(|e| e.to_string())?;
        self.mcp = next;
        self.probes.remove(name);
        if let Some(conn) = conn {
            let _ = storage::set_meta(conn, &probe_key(name), None);
        }
        Ok(())
    }

    /// Every allowlisted write in mcp.toml, listed apart from the read
    /// calls: these are the only things Chronicle can put back into a
    /// tracker, and only a click in the task pane runs one.
    fn actions_ui(&mut self, ui: &mut egui::Ui) {
        if self.mcp.action_calls.is_empty() {
            return;
        }
        ui.add_space(theme::SPACE_SM);
        subhead(ui, "Actions", |_| {});
        let width = ui.available_width();
        for a in &self.mcp.action_calls {
            let title = format!("{}.{}", a.server, a.tool);
            theme::ListRow::new(&title)
                .emphasis()
                .dot(palette::TEXT_DIM)
                .chip("manual".to_owned(), palette::TEXT_DIM)
                .show(ui, width, |_| {});
            if !a.label.is_empty() {
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    caption(ui, a.label.clone(), None);
                });
            }
        }
        caption(ui, "posts only when you click".to_owned(), None);
    }

    fn repos_ui(&mut self, ui: &mut egui::Ui, git_repos: &mut Vec<String>) {
        subhead(ui, "Git repos", |_| {});
        if git_repos.is_empty() {
            ui.weak("no repos watched \u{2014} commits and branch switches become task evidence");
        }
        let width = ui.available_width();
        let mut remove: Option<usize> = None;
        let mut arm: Option<usize> = None;
        let mut hook_toggle: Option<(usize, bool)> = None;
        for (i, path) in git_repos.iter().enumerate() {
            let expanded = expand_home(path);
            let name = repo_name(&expanded);
            let resolved = self.repo_resolved.get(i).copied().unwrap_or(false);
            // Chip stays short (kind + age) so the title survives at zoom
            // 1.15; the branch rides the path line, where it can truncate.
            let last = self.repo_last.get(&name);
            let (dot, chip, color) = match (resolved, last) {
                (false, _) => (palette::RED, "not a git repo".to_owned(), palette::RED),
                (true, Some(ev)) => (
                    palette::GREEN,
                    format!("{} {}", ev.kind.as_str(), ago(ev.ts.as_millisecond())),
                    palette::TEXT_DIM,
                ),
                (true, None) => (palette::AMBER, "no activity yet".to_owned(), palette::AMBER),
            };
            let detail = match last {
                Some(ev) => format!("{path} \u{b7} {}", ev.branch),
                None => path.clone(),
            };
            let hooks = self.repo_hooks.get(path).cloned().unwrap_or_default();
            let armed = self.repo_arm_remove == Some(i);
            theme::ListRow::new(&name)
                .emphasis()
                .dot(dot)
                .chip(chip, color)
                .show(ui, width, |ui| {
                    let label = if armed { "delete?" } else { "\u{d7}" };
                    if theme::ghost_button(ui, label).clicked() {
                        if armed {
                            remove = Some(i);
                        } else {
                            arm = Some(i);
                        }
                    }
                    // Chronicle's git hooks (m37 chunk 2): exact-second
                    // checkouts and commits, appended after any existing
                    // hook; opt-in per repo.
                    let (label, hover) = if hooks.is_empty() {
                        ("hooks", "install post-checkout, post-commit and post-rewrite hooks (appended after any existing hook)")
                    } else {
                        ("hooks \u{2713}", "remove chronicle's git hooks")
                    };
                    if resolved && theme::ghost_button(ui, label).on_hover_text(hover).clicked() {
                        hook_toggle = Some((i, hooks.is_empty()));
                    }
                });
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                caption(ui, detail, None);
            });
        }
        if let Some(i) = arm {
            self.repo_arm_remove = Some(i);
        }
        if let Some((i, install)) = hook_toggle
            && let Some(path) = git_repos.get(i)
        {
            let repo = expand_home(path);
            let result = if install {
                std::env::current_exe()
                    .and_then(|exe| chronicle_capture::hooks::install(&repo, &exe))
            } else {
                chronicle_capture::hooks::remove(&repo)
            };
            match result {
                Ok(_) => {
                    let st = chronicle_capture::hooks::status(&repo);
                    self.repo_hooks.insert(path.clone(), st.installed);
                }
                Err(e) => self.repo_error = Some(format!("hooks: {e}")),
            }
        }
        if let Some(i) = remove {
            git_repos.remove(i);
            self.repo_arm_remove = None;
            self.refresh_repo_status(git_repos);
        }
        ui.horizontal(|ui| {
            let w = ui.available_width();
            ui.add(
                egui::TextEdit::singleline(&mut self.repo_add)
                    .desired_width(w - 56.0)
                    .font(egui::TextStyle::Monospace)
                    .hint_text("~/dev/project"),
            );
            if theme::secondary_button(ui, "add").clicked() {
                self.repo_error = self.add_repo(git_repos).err();
                if self.repo_error.is_none() {
                    self.refresh_repo_status(git_repos);
                }
            }
        });
        if let Some(e) = &self.repo_error {
            ui.colored_label(palette::RED, e);
        }
    }

    /// One row per local collector: a switch, a status chip from the newest
    /// stored event, and the tool gate the daemon applies at spawn (`gh`,
    /// `pw-dump` on PATH) surfaced here instead of only in the log.
    /// The local sources section (m41 chunk 1): one row per connector the
    /// registry claims for this platform, in the registry's own grouping,
    /// with the health the probes and the event tables report. Git repos
    /// and MCP servers keep the sections above — their descriptors are
    /// skipped here rather than drawn twice.
    fn sources_ui(&mut self, ui: &mut egui::Ui, src: &mut LocalSources) {
        let width = ui.available_width();
        let mut sessions_on = !src.ai_session_dirs.is_empty();
        let missing_dir = self.session_dir_missing.clone();

        // What this machine has to say about a row, where the registry's
        // one-line blurb would be the poorer answer: the profiles found,
        // the projects discovered, the workspaces read.
        let mut details: BTreeMap<&'static str, String> = BTreeMap::new();
        details.insert(
            "claude_code_sessions",
            if sessions_on {
                src.ai_session_dirs.join(", ")
            } else {
                "Claude Code transcripts under ~/.claude/projects".to_owned()
            },
        );
        let others: Vec<&str> = self
            .session_formats
            .iter()
            .copied()
            .filter(|f| *f != "claude")
            .collect();
        details.insert(
            "agent_sessions",
            if others.is_empty() {
                "none found \u{b7} looks for codex, gemini, copilot, aider, cline, opencode, cursor"
                    .to_owned()
            } else {
                format!(
                    "found: {} \u{b7} first prompt, cwd and paths only",
                    others.join(", ")
                )
            },
        );
        details.insert(
            "browser_history",
            if self.browser_profiles.is_empty() {
                "no Chrome, Chromium, Brave, Edge, Firefox, LibreWolf or Zen profile found"
                    .to_owned()
            } else {
                format!(
                    "{} \u{b7} copied and read every 60 s, query strings dropped, private windows absent",
                    self.browser_profiles.join(", ")
                )
            },
        );
        details.insert(
            "repo_discovery",
            if self.discovered.is_empty() {
                "git repos beside the ones above become projects without a config edit".to_owned()
            } else {
                format!("discovered: {}", self.discovered.join(", "))
            },
        );
        if !self.link_rows.is_empty() {
            let mut names: Vec<String> = self
                .link_rows
                .iter()
                .filter(|l| l.kind != "build")
                .map(|l| format!("{} {}", l.kind, l.name))
                .collect();
            names.dedup();
            let mut builds: Vec<&str> = self
                .link_rows
                .iter()
                .filter(|l| l.kind == "build")
                .map(|l| l.name.as_str())
                .collect();
            let mut d = names.join(", ");
            if !builds.is_empty() {
                if !d.is_empty() {
                    d.push_str(" \u{b7} ");
                }
                d.push_str("build: ");
                builds.sort_unstable();
                builds.dedup();
                d.push_str(&builds.join(", "));
            }
            details.insert("link_files", d);
        }
        if !self.workspace_rows.is_empty() {
            let mut editors: Vec<&str> = self
                .workspace_rows
                .iter()
                .map(|w| w.editor.as_str())
                .collect();
            editors.sort_unstable();
            editors.dedup();
            let remote = self
                .workspace_rows
                .iter()
                .filter(|w| !w.remote.is_empty())
                .count();
            details.insert(
                "editor_workspaces",
                format!(
                    "{} workspaces from {}{}",
                    self.workspace_rows.len(),
                    editors.join(", "),
                    if remote > 0 {
                        format!(" \u{b7} {remote} remote")
                    } else {
                        String::new()
                    }
                ),
            );
        }
        if !src.calendars.is_empty() {
            details.insert(
                "calendars_ics",
                format!("{} feeds every 15 min, \u{b1}7 days", src.calendars.len()),
            );
        }
        if src.shell_hook {
            details.insert(
                "shell_hook",
                "eval \"$(chronicle shell-init zsh)\" in your rc file \u{b7} cwd, program name and duration per command, never the command line"
                    .to_owned(),
            );
        }
        details.insert(
            "google_calendar",
            match self.google_account.as_deref() {
                Some("") => "primary calendar every 5 min".to_owned(),
                Some(email) => format!("primary calendar every 5 min \u{b7} {email}"),
                None => "primary calendar every 5 min \u{b7} read-only, events scope".to_owned(),
            },
        );

        // The switches, copied out: a row's toggle borrows this list rather
        // than a different field of `src` each time.
        let mut request: Option<&'static connectors::Connector> = None;
        let mut switches: Vec<(&'static str, bool)> = SWITCHES
            .iter()
            .map(|f| match *f {
                "ai_session_dirs" => (*f, sessions_on),
                field => (*f, switch(src, field).is_some_and(|b| *b)),
            })
            .collect();

        for kind in [
            ConnectKind::Files,
            ConnectKind::LocalServer,
            ConnectKind::Cli,
            ConnectKind::Account,
        ] {
            let mut rows: Vec<&'static connectors::Connector> =
                connectors::for_platform(Platform::HOST)
                    .filter(|c| c.kind == kind && !DRAWN_ELSEWHERE.contains(&c.id))
                    .collect();
            if rows.is_empty() {
                continue;
            }
            // What the person said they use sorts to the top of its group,
            // planned rows included (m41 chunk 3); stable, so the registry
            // order holds within each half.
            rows.sort_by_key(|c| !self.declared.uses(c.id));
            if kind != ConnectKind::Files {
                ui.add_space(theme::SPACE_SM);
            }
            subhead(ui, kind.label(), |_| {});
            for c in rows {
                // The two blockers a probe cannot see: a configured session
                // directory that is gone, and an account that is not signed
                // in.
                let blocker = match c.id {
                    "claude_code_sessions" => missing_dir
                        .clone()
                        .map(|d| format!("no such directory: {d}")),
                    "google_calendar" if self.google_account.is_none() => {
                        Some("not signed in: run `chronicle gcal-login`".to_owned())
                    }
                    _ => None,
                };
                self.source_row(
                    ui,
                    width,
                    c,
                    &mut switches,
                    &details,
                    blocker.as_deref(),
                    &mut request,
                );
                // The key the WakaTime plugins authenticate with belongs to
                // the row above it, not to the end of the group.
                if c.id == "editor_heartbeats"
                    && let Some(key) = self.wakapi_key.clone()
                {
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        caption(ui, format!("api key {}", masked(&key)), None);
                        if theme::ghost_button(ui, theme::glyph(theme::icon::COPY))
                            .on_hover_text("copy the api key for ~/.wakatime.cfg")
                            .clicked()
                        {
                            ui.ctx().copy_text(key.clone());
                        }
                    });
                }
            }
        }

        if let Some(c) = request {
            self.request = Some(RequestForm::new(Request::for_connector(c)));
        }
        for (field, on) in switches {
            if field == "ai_session_dirs" {
                sessions_on = on;
                continue;
            }
            if let Some(slot) = switch(src, field) {
                *slot = on;
            }
        }
        if sessions_on != !src.ai_session_dirs.is_empty() {
            src.ai_session_dirs = if sessions_on {
                chronicle_core::config::Config::default().ai_session_dirs
            } else {
                Vec::new()
            };
            self.session_dir_missing = missing_session_dir(&src.ai_session_dirs);
        }
    }

    /// One source row: the registry's name and blurb, this machine's health
    /// as the dot and chip, and a toggle only where a descriptor names a
    /// config field to flip.
    fn source_row(
        &self,
        ui: &mut egui::Ui,
        width: f32,
        c: &'static connectors::Connector,
        switches: &mut [(&'static str, bool)],
        details: &BTreeMap<&'static str, String>,
        blocker: Option<&str>,
        request: &mut Option<&'static connectors::Connector>,
    ) {
        let stored = self.health.get(c.id).cloned().unwrap_or(Health::Absent);
        let toggle = c.setup.iter().find_map(|s| match s {
            connectors::SetupStep::Toggle { field } => Some(*field),
            _ => None,
        });
        // Health is read at load, but a switch flips in this frame: carry
        // the toggle's live value so a row answers the click that just
        // happened instead of waiting for Settings to be reopened. A row
        // with no switch has nothing to carry and keeps what it was given.
        let present = matches!(
            stored,
            Health::Found | Health::Connected | Health::Working { .. }
        );
        let state = match toggle.map(|field| {
            switches
                .iter()
                .find(|(f, _)| *f == field)
                .is_some_and(|(_, v)| *v)
        }) {
            None | Some(true) if present => stored,
            Some(false) if present => Health::Found,
            None => stored,
            Some(true) => Health::Broken {
                reason: match stored {
                    Health::Broken { reason } => reason,
                    // Switched on this frame, so health has no reason yet:
                    // name the first thing the descriptor looks for.
                    _ => c
                        .probes
                        .iter()
                        .find_map(|p| match p.kind {
                            connectors::ProbeKind::Command { cmd } => {
                                Some(format!("{cmd} not on PATH"))
                            }
                            connectors::ProbeKind::Path { path } => {
                                Some(format!("{path} is missing"))
                            }
                            _ => None,
                        })
                        .unwrap_or_else(|| "not on this machine".to_owned()),
                },
            },
            Some(false) => Health::Absent,
        };
        let (dot, chip, color) = match (&state, blocker) {
            (_, Some(why)) => (palette::RED, why.to_owned(), palette::RED),
            (Health::Broken { reason }, _) => (palette::RED, reason.clone(), palette::RED),
            (Health::Working { last_seen_ms, .. }, _) => {
                (palette::GREEN, ago(*last_seen_ms), palette::TEXT_DIM)
            }
            (Health::Connected, _) => (
                palette::AMBER,
                "nothing seen yet".to_owned(),
                palette::AMBER,
            ),
            (Health::Found | Health::Absent, _) => (
                palette::TEXT_DIM,
                health::label(c, &state).to_owned(),
                palette::TEXT_DIM,
            ),
        };
        theme::ListRow::new(c.name)
            .emphasis()
            .dot(dot)
            .chip(chip, color)
            .show(ui, width, |ui| {
                if let Some(field) = toggle
                    && let Some(sw) = switches.iter_mut().find(|(f, _)| *f == field)
                {
                    theme::toggle(ui, &mut sw.1);
                }
                // A row the person uses that Chronicle cannot read yet is
                // the one that earns a request (m41 chunk 3).
                if self.declared.uses(c.id)
                    && !matches!(
                        c.state,
                        connectors::Support::Supported | connectors::Support::Partial { .. }
                    )
                    && theme::ghost_button(ui, "request")
                        .on_hover_text(REQUEST_HOVER)
                        .clicked()
                {
                    *request = Some(c);
                }
            });
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            caption(
                ui,
                details
                    .get(c.id)
                    .cloned()
                    .unwrap_or_else(|| c.blurb.to_owned()),
                None,
            );
        });
    }

    /// What you work with (m41 chunk 3): the month's apps and sites no
    /// connector reads, ranked by minutes, each with a request button; then
    /// the registry as a tick list of what the person says they use, and a
    /// free line for anything without a row. Ticks live in `meta` and sort
    /// the rows above.
    fn worklist_ui(&mut self, ui: &mut egui::Ui, conn: Option<&Connection>) {
        ui.add_space(theme::SPACE_SM);
        subhead(ui, "What you work with", |ui| {
            caption(ui, format!("last {MINED_DAYS} days"), None);
        });
        let width = ui.available_width();
        let mut compose: Option<usize> = None;
        let unread: Vec<usize> = (0..self.mined.len())
            .filter(|&i| self.mined[i].unread())
            .take(MINED_ROWS)
            .collect();
        if unread.is_empty() {
            caption(
                ui,
                if self.mined.is_empty() {
                    "nothing in focus yet".to_owned()
                } else {
                    "everything you spent time in this month has a connector".to_owned()
                },
                None,
            );
        }
        for i in unread {
            let t = &self.mined[i];
            let label = t.label();
            let (state, color) = match t.claimed_by {
                Some(_) => ("planned", palette::AMBER),
                None => (t.category(), palette::TEXT_DIM),
            };
            theme::ListRow::new(&label)
                .chip(
                    format!("{} \u{b7} {} d", usage::minutes_label(t.minutes), t.days),
                    palette::TEXT_DIM,
                )
                .chip(state, color)
                .show(ui, width, |ui| {
                    if theme::ghost_button(ui, "request")
                        .on_hover_text(REQUEST_HOVER)
                        .clicked()
                    {
                        compose = Some(i);
                    }
                });
        }
        if let Some(i) = compose {
            let t = &self.mined[i];
            let since = Timestamp::now().as_millisecond() - MINED_DAYS * 86_400_000;
            let sample = match t.seen {
                usage::Seen::App => conn.and_then(|c| usage::sample_title(c, &t.name, since)),
                usage::Seen::Domain => None,
            };
            self.request = Some(RequestForm::new(Request::for_tool(t, sample, MINED_DAYS)));
        }

        ui.add_space(theme::SPACE_XS);
        theme::disclosure_header(
            ui,
            &mut self.declare_open,
            "what I use",
            Some(self.declared.ids.len()).filter(|n| *n > 0),
        );
        let open = self.declare_open;
        let declared = &mut self.declared;
        let filter = &mut self.declare_filter;
        let health = &self.health;
        let mut dirty = false;
        theme::fade_body(ui, "declare", open, |ui| {
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                caption(
                    ui,
                    "tick what you use; it sorts to the top and, where Chronicle cannot read it yet, earns a request button"
                        .to_owned(),
                    None,
                );
            });
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.add(
                    egui::TextEdit::singleline(filter)
                        .hint_text("filter\u{2026}")
                        .desired_width(160.0),
                );
            });
            let q = filter.trim().to_lowercase();
            for c in connectors::for_platform(Platform::HOST) {
                if !q.is_empty() && !c.name.to_lowercase().contains(&q) && !c.id.contains(&q) {
                    continue;
                }
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    let mut on = declared.uses(c.id);
                    if theme::toggle(ui, &mut on).changed() {
                        if on {
                            declared.ids.insert(c.id.to_owned());
                        } else {
                            declared.ids.remove(c.id);
                        }
                        dirty = true;
                    }
                    ui.label(c.name);
                    let h = health.get(c.id).cloned().unwrap_or(Health::Absent);
                    theme::badge(ui, health::label(c, &h), palette::TEXT_DIM);
                });
            }
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut declared.other)
                            .hint_text("anything else you use, in a few words")
                            .desired_width(width - 16.0 - 8.0),
                    )
                    .changed()
                {
                    dirty = true;
                }
            });
        });
        if dirty && let Some(conn) = conn {
            let _ = self.declared.save(conn);
        }
    }

    /// The compose box (m41 chunk 4): tool, category and platform prefilled,
    /// the "why" field, the evidence chips, and the issue body exactly as it
    /// will appear. Three ways out: the browser (a prefilled `issues/new`
    /// URL under the person's own account), the clipboard, or a file under
    /// the data dir for people without a GitHub account.
    fn request_ui(&mut self, ui: &mut egui::Ui, conn: Option<&Connection>) {
        let requests_dir = self.requests_dir.clone();
        let Some(form) = &mut self.request else {
            return;
        };
        if !form.counted {
            form.counted = true;
            if let Some(c) = conn {
                usage::bump(c, usage::REQUESTS_COMPOSED);
            }
        }
        let mut close = false;
        let width = (ui.ctx().content_rect().width() - 48.0).clamp(240.0, 440.0);
        let resp = egui::Modal::new(egui::Id::new("integration_request")).show(ui.ctx(), |ui| {
            ui.set_width(width);
            ui.label(
                egui::RichText::new("Request an integration")
                    .family(egui::FontFamily::Name(theme::MEDIUM.into()))
                    .color(palette::TEXT),
            );
            caption(
                ui,
                "opens a prefilled GitHub issue in your browser, under your account \u{2014} \
                 Chronicle sends nothing"
                    .to_owned(),
                None,
            );
            ui.add_space(theme::SPACE_XS);
            ui.label("tool");
            ui.add(egui::TextEdit::singleline(&mut form.req.tool).desired_width(f32::INFINITY));
            ui.horizontal(|ui| {
                ui.label("category");
                ui.add(egui::TextEdit::singleline(&mut form.req.category).desired_width(120.0));
                ui.label("platform");
                ui.add(egui::TextEdit::singleline(&mut form.req.platform).desired_width(90.0));
            });
            ui.label("what you'd want out of it");
            ui.add(
                egui::TextEdit::multiline(&mut form.req.why)
                    .desired_rows(3)
                    .desired_width(f32::INFINITY),
            );
            if !form.chips.is_empty() {
                ui.label("evidence \u{2014} drop any line you would rather keep");
                for (line, keep) in form.chips.iter_mut() {
                    ui.horizontal(|ui| {
                        if theme::ghost_button(ui, "\u{d7}")
                            .on_hover_text("drop this line")
                            .clicked()
                        {
                            *keep = false;
                        }
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(line.as_str()).text_style(theme::caption()),
                            )
                            .truncate(),
                        );
                    });
                }
                form.chips.retain(|(_, keep)| *keep);
            }
            ui.add_space(theme::SPACE_XS);
            let current = form.current();
            caption(ui, "the issue, exactly as it will appear".to_owned(), None);
            egui::ScrollArea::vertical()
                .id_salt("request_preview")
                .max_height(150.0)
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(current.body())
                                .text_style(egui::TextStyle::Monospace)
                                .size(11.0)
                                .color(palette::TEXT_DIM),
                        )
                        .selectable(true)
                        .wrap(),
                    );
                });
            ui.add_space(theme::SPACE_XS);
            match &form.status {
                Some(Ok(msg)) => {
                    ui.colored_label(palette::GREEN, msg.as_str());
                }
                Some(Err(msg)) => {
                    ui.colored_label(palette::RED, msg.as_str());
                }
                None => {}
            }
            ui.horizontal(|ui| {
                let ready = !current.tool.trim().is_empty();
                if theme::primary_button_enabled(ui, ready, "open GitHub issue").clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(current.url()));
                    if let Some(c) = conn {
                        usage::bump(c, usage::REQUESTS_FILED);
                    }
                    form.status = Some(Ok("opened in your browser".into()));
                }
                if theme::secondary_button(ui, "copy").clicked() {
                    ui.ctx().copy_text(current.markdown());
                    form.status = Some(Ok("copied".into()));
                }
                if theme::secondary_button(ui, "save to file").clicked() {
                    let name = format!("{}-{}.md", current.slug(), jiff::Zoned::now().date());
                    let path = requests_dir.join(name);
                    form.status = Some(
                        std::fs::create_dir_all(&requests_dir)
                            .and_then(|()| std::fs::write(&path, current.markdown()))
                            .map(|()| format!("saved {}", path.display()))
                            .map_err(|e| e.to_string()),
                    );
                }
                if theme::ghost_button(ui, "close").clicked() {
                    close = true;
                }
            });
        });
        if close || resp.should_close() {
            self.request = None;
        }
    }

    fn add_repo(&mut self, git_repos: &mut Vec<String>) -> Result<(), String> {
        let raw = self.repo_add.trim().trim_end_matches('/').to_owned();
        if raw.is_empty() {
            return Err("enter a path".into());
        }
        let expanded = expand_home(&raw);
        if chronicle_capture::git::resolve_git_dir(&expanded).is_none() {
            return Err(format!("not a git repo: {}", expanded.display()));
        }
        if git_repos.iter().any(|p| expand_home(p) == expanded) {
            return Err("already watched".into());
        }
        let name = repo_name(&expanded);
        if let Some(other) = git_repos
            .iter()
            .find(|p| repo_name(&expand_home(p)) == name)
        {
            return Err(format!(
                "{other} is already named {name} \u{2014} repos are keyed by directory name"
            ));
        }
        git_repos.push(raw);
        self.repo_add.clear();
        Ok(())
    }
}
