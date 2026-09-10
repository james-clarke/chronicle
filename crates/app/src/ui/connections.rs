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
use chronicle_core::storage;
use chronicle_core::types::{ActivityEvent, ActivityKind};
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

fn subhead(ui: &mut egui::Ui, title: &str, trailing: impl FnOnce(&mut egui::Ui)) {
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

fn kind_label(k: ActivityKind) -> &'static str {
    match k {
        ActivityKind::AiSession => "session",
        ActivityKind::PrAuthored => "authored",
        ActivityKind::PrReviewed => "reviewed",
        ActivityKind::Call => "call",
        other => other.as_str(),
    }
}

/// One sources row: title, switch (`None` = detected, no switch), blocker
/// (tool/dir missing), the kinds whose newest event feeds the chip, detail
/// caption.
type SourceRow<'a> = (
    &'a str,
    Option<&'a mut bool>,
    Option<String>,
    &'a [ActivityKind],
    String,
);

/// Bare command resolved on PATH (plus the user bin dirs the daemon sees)?
fn on_path(cmd: &str) -> bool {
    chronicle_core::config::resolve_command(cmd).contains('/')
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
    /// Newest event per kind (local collector rows).
    kind_last: Vec<ActivityEvent>,
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
    /// `gh`/`pw-dump` on PATH and the atuin db present; filesystem/PATH
    /// checks that don't change within a settings session, so cached at
    /// load instead of probed every frame.
    gh_on_path: bool,
    glab_on_path: bool,
    tmux_on_path: bool,
    docker_on_path: bool,
    pw_dump_on_path: bool,
    atuin_present: bool,
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
        let kind_last = conn
            .and_then(|c| storage::latest_activity_per_kind(c).ok())
            .unwrap_or_default();
        let wakapi_key = conn.and_then(|c| storage::get_meta(c, "wakapi_api_key").ok().flatten());
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let discovered: Vec<String> =
            chronicle_core::config::Config::load(&data_dir.join("config.toml"))
                .map(|c| chronicle_core::project::Matcher::from_config(&c))
                .map(|m| {
                    m.projects
                        .iter()
                        .filter(|p| p.discovered)
                        .map(|p| p.name.clone())
                        .collect()
                })
                .unwrap_or_default();
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
            kind_last,
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
            gh_on_path: on_path("gh"),
            glab_on_path: on_path("glab"),
            tmux_on_path: on_path("tmux"),
            docker_on_path: on_path("docker"),
            pw_dump_on_path: on_path("pw-dump"),
            atuin_present: chronicle_capture::shell::default_db_path().is_file(),
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
    fn sources_ui(&mut self, ui: &mut egui::Ui, src: &mut LocalSources) {
        let width = ui.available_width();
        let mut sessions_on = !src.ai_session_dirs.is_empty();
        let missing_dir = self.session_dir_missing.clone();
        let session_detail = if sessions_on {
            src.ai_session_dirs.join(", ")
        } else {
            "Claude Code transcripts under ~/.claude/projects".to_owned()
        };
        let gcal_detail = match self.google_account.as_deref() {
            Some("") => "primary calendar every 5 min".to_owned(),
            Some(email) => format!("primary calendar every 5 min \u{b7} {email}"),
            None => "primary calendar every 5 min \u{b7} read-only, events scope".to_owned(),
        };
        let others: Vec<&str> = self
            .session_formats
            .iter()
            .copied()
            .filter(|f| *f != "claude")
            .collect();
        let formats_detail = if others.is_empty() {
            "none found \u{b7} looks for codex, gemini, copilot, aider, cline, opencode, cursor"
                .to_owned()
        } else {
            format!(
                "found: {} \u{b7} first prompt, cwd and paths only",
                others.join(", ")
            )
        };
        let mut formats_on = !others.is_empty() && sessions_on;
        let browser_detail = if self.browser_profiles.is_empty() {
            "no Chrome, Chromium, Brave, Edge, Firefox, LibreWolf or Zen profile found".to_owned()
        } else {
            format!(
                "{} \u{b7} copied and read every 60 s, query strings dropped, private windows absent",
                self.browser_profiles.join(", ")
            )
        };
        let discover_detail = if self.discovered.is_empty() {
            "git repos beside the ones above become projects without a config edit".to_owned()
        } else {
            format!("discovered: {}", self.discovered.join(", "))
        };
        let links_detail = if self.link_rows.is_empty() {
            "vercel, netlify, fly, render, wrangler, supabase, doppler, sentry, CI and the build system, read at each repo root"
                .to_owned()
        } else {
            let mut names: Vec<String> = self
                .link_rows
                .iter()
                .filter(|l| l.kind != "build")
                .map(|l| format!("{} {}", l.kind, l.name))
                .collect();
            names.dedup();
            let builds: Vec<&str> = self
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
                let mut b = builds;
                b.sort_unstable();
                b.dedup();
                d.push_str(&b.join(", "));
            }
            d
        };
        let workspaces_detail = if self.workspace_rows.is_empty() {
            "VS Code, Cursor, Windsurf, JetBrains and Zed recent lists: none found".to_owned()
        } else {
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
            format!(
                "{} workspaces from {}{}",
                self.workspace_rows.len(),
                editors.join(", "),
                if remote > 0 {
                    format!(" \u{b7} {remote} remote")
                } else {
                    String::new()
                }
            )
        };
        let calendars_detail = if src.calendars.is_empty() {
            "add `calendars = [\"https://…/basic.ics\"]` to config.toml: a secret iCal address, no OAuth"
                .to_owned()
        } else {
            format!("{} feeds every 15 min, \u{b1}7 days", src.calendars.len())
        };
        let mut calendars_on = !src.calendars.is_empty();
        let mut links_on = !self.link_rows.is_empty();
        let mut workspaces_on = !self.workspace_rows.is_empty();
        let mut tmux_on = self.tmux_on_path;
        let mut docker_on = self.docker_on_path;
        let shell_hook_detail = if src.shell_hook {
            "eval \"$(chronicle shell-init zsh)\" in your rc file \u{b7} cwd, program name and duration per command, never the command line"
                .to_owned()
        } else {
            "the local route answers 403".to_owned()
        };

        subhead(ui, "Files on this machine", |_| {});
        let mut files: [SourceRow; 8] = [
            (
                "Claude Code sessions",
                Some(&mut sessions_on),
                missing_dir.map(|d| format!("no such directory: {d}")),
                &[ActivityKind::AiSession],
                session_detail,
            ),
            (
                "Other agents' sessions",
                Some(&mut formats_on),
                None,
                &[],
                formats_detail,
            ),
            (
                "Browser history",
                Some(&mut src.browser_history),
                self.browser_profiles
                    .is_empty()
                    .then(|| "no history database found".to_owned()),
                &[ActivityKind::Browse],
                browser_detail,
            ),
            (
                "Repo discovery",
                Some(&mut src.discover_repos),
                None,
                &[],
                discover_detail,
            ),
            ("Link files", Some(&mut links_on), None, &[], links_detail),
            (
                "Editor workspaces",
                Some(&mut workspaces_on),
                None,
                &[],
                workspaces_detail,
            ),
            (
                "Shell history (atuin)",
                Some(&mut src.shell_history),
                (!self.atuin_present).then(|| "no atuin history.db".to_owned()),
                &[ActivityKind::Shell],
                "atuin history.db every 60 s \u{b7} cwd, program name and duration only".to_owned(),
            ),
            (
                "Calendars (ICS)",
                Some(&mut calendars_on),
                None,
                &[ActivityKind::Meeting],
                calendars_detail,
            ),
        ];
        self.source_rows(ui, width, &mut files, &[1, 4, 5, 7]);

        ui.add_space(theme::SPACE_SM);
        subhead(ui, "Local servers and sockets", |_| {});
        let mut local: [SourceRow; 5] = [
            (
                "Editor heartbeats (WakaTime plugins)",
                Some(&mut src.editor_heartbeats),
                None,
                &[ActivityKind::Edit],
                "vim-wakatime and friends post to this machine \u{b7} folded into edit spans per project"
                    .to_owned(),
            ),
            (
                "Shell hook",
                Some(&mut src.shell_hook),
                None,
                &[ActivityKind::Shell],
                shell_hook_detail,
            ),
            (
                "tmux panes",
                Some(&mut tmux_on),
                (!self.tmux_on_path).then(|| "tmux not on PATH".to_owned()),
                &[ActivityKind::Cwd],
                "the attached panes' working directories every 60 s".to_owned(),
            ),
            (
                "Docker Compose stacks",
                Some(&mut docker_on),
                (!self.docker_on_path).then(|| "docker not on PATH".to_owned()),
                &[ActivityKind::Cwd],
                "each running stack's working_dir label every 60 s".to_owned(),
            ),
            (
                "Calls (microphone in use)",
                Some(&mut src.mic_capture),
                (!self.pw_dump_on_path).then(|| "pw-dump not on PATH".to_owned()),
                &[ActivityKind::Call],
                "pw-dump every 20 s \u{b7} each stretch becomes a call".to_owned(),
            ),
        ];
        self.source_rows(ui, width, &mut local, &[2, 3]);
        if let Some(key) = self.wakapi_key.clone() {
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

        ui.add_space(theme::SPACE_SM);
        subhead(ui, "Your CLIs", |_| {});
        let mut clis: [SourceRow; 2] = [
            (
                "GitHub pull requests (gh)",
                Some(&mut src.github_prs),
                (!self.gh_on_path).then(|| "gh not on PATH".to_owned()),
                &[ActivityKind::PrAuthored, ActivityKind::PrReviewed],
                "gh search prs, authored + reviewed, every 5 min \u{b7} your existing login".to_owned(),
            ),
            (
                "GitLab merge requests (glab)",
                Some(&mut src.gitlab_mrs),
                (!self.glab_on_path).then(|| "glab not on PATH".to_owned()),
                &[ActivityKind::PrAuthored, ActivityKind::PrReviewed],
                "glab mr list, assigned + reviewing, every 5 min per GitLab repo \u{b7} your existing login"
                    .to_owned(),
            ),
        ];
        self.source_rows(ui, width, &mut clis, &[]);

        ui.add_space(theme::SPACE_SM);
        subhead(ui, "Accounts", |_| {});
        let mut accounts: [SourceRow; 1] = [(
            "Google Calendar",
            Some(&mut src.google_calendar),
            self.google_account
                .is_none()
                .then(|| "not signed in: run `chronicle gcal-login`".to_owned()),
            &[ActivityKind::Meeting],
            gcal_detail,
        )];
        self.source_rows(ui, width, &mut accounts, &[]);

        if sessions_on != !src.ai_session_dirs.is_empty() {
            src.ai_session_dirs = if sessions_on {
                chronicle_core::config::Config::default().ai_session_dirs
            } else {
                Vec::new()
            };
            self.session_dir_missing = missing_session_dir(&src.ai_session_dirs);
        }
    }

    /// The rows of one sources group. `detected` indexes rows whose switch
    /// only reports what is on this machine (no config behind it): drawn
    /// without a toggle.
    fn source_rows(
        &self,
        ui: &mut egui::Ui,
        width: f32,
        rows: &mut [SourceRow],
        detected: &[usize],
    ) {
        for (i, (name, on, blocker, kinds, detail)) in rows.iter_mut().enumerate() {
            let is_on = on.as_deref().copied().unwrap_or(true);
            let last = self
                .kind_last
                .iter()
                .filter(|e| kinds.contains(&e.kind))
                .max_by_key(|e| e.end_ts.unwrap_or(e.ts));
            let (dot, chip, color) = match (is_on, blocker.as_deref(), last) {
                (false, _, _) => (palette::TEXT_DIM, "off".to_owned(), palette::TEXT_DIM),
                (true, Some(why), _) => (palette::RED, why.to_owned(), palette::RED),
                (true, None, Some(ev)) => (
                    palette::GREEN,
                    format!(
                        "{} {}",
                        kind_label(ev.kind),
                        ago(ev.end_ts.unwrap_or(ev.ts).as_millisecond())
                    ),
                    palette::TEXT_DIM,
                ),
                (true, None, None) if kinds.is_empty() => {
                    (palette::GREEN, "on".to_owned(), palette::TEXT_DIM)
                }
                (true, None, None) => (
                    palette::AMBER,
                    "nothing seen yet".to_owned(),
                    palette::AMBER,
                ),
            };
            let toggle = !detected.contains(&i);
            theme::ListRow::new(name)
                .emphasis()
                .dot(dot)
                .chip(chip, color)
                .show(ui, width, |ui| {
                    if toggle && let Some(on) = on.as_deref_mut() {
                        theme::toggle(ui, on);
                    }
                });
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                caption(ui, detail.clone(), None);
            });
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
