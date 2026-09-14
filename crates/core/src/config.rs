use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("failed to write config: {0}")]
    Write(#[from] toml::ser::Error),
    #[error("invalid config: {0}")]
    Invalid(String),
}

// Serialize: the UI settings panel writes the whole struct back to
// config.toml (TOML has no null — skip the Nones).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Batch = this many minutes of non-AFK activity.
    pub batch_minutes: u32,
    /// A batch may also close at an AFK gap of 5 minutes or more once it
    /// holds this many minutes of activity, so windows end at natural breaks.
    pub batch_min_minutes: u32,
    /// The AFK poller reports idle after this long without input; the
    /// sessionizer treats the stretch as quiet (span stays open) until it
    /// reaches `quiet_secs` / `away_secs`.
    pub afk_close_secs: u32,
    /// Idle at least this long with nothing live on screen (no agent writing
    /// to the focused tool, no call, no meeting window) closes the span at
    /// the idle start (m32 chunk 1). 0 = close on any idle, the m31 rule.
    pub quiet_secs: u32,
    /// Idle at least this long with a live context closes the span.
    pub away_secs: u32,
    /// Count keys / buttons / motion / scroll per minute into `presence`
    /// (counts only, never what was typed). Off = no row is written.
    /// Linux X11 and macOS only: Wayland has no protocol for it (m39).
    pub capture_presence: bool,
    /// Which focus provider runs on Linux (m39): `auto` reads the session
    /// environment, `x11` forces X11 (which is also how to capture X
    /// clients through Xwayland), `wlr` the wlr-foreign-toplevel protocol,
    /// `kwin` the KWin script. Ignored on macOS.
    pub focus_route: String,
    /// Derivation may start when idle at least this long.
    pub derive_idle_secs: u32,
    /// Live tier (m27): while active, the resident worker labels the current
    /// stretch this often. 0 = off.
    pub live_secs: u32,
    /// The resident derive worker (model loaded, prompt prefix cached) exits
    /// after this long without a request. 0 = exit after each request.
    pub worker_idle_secs: u32,
    /// The deterministic pre-pass (branch → ticket, repo → project, past
    /// corrections) places provisional intervals over the not-yet-derived
    /// tail this often. 0 = off.
    pub prepass_secs: u32,
    /// Consecutive same-app events merge into one span when normalized title
    /// similarity is at least this (absorbs jitter like unread-count prefixes).
    pub title_similarity: f64,
    /// Rows older than this are pruned daily (0 = keep forever).
    /// Corrections are always kept.
    pub retention_days: u32,
    /// Open derived tasks with no interval this many days are auto-closed
    /// daily (0 = never; declared tasks only close by hand).
    pub task_autoclose_days: u32,
    /// How the live tail is placed (m30 chunk 3): `model` = the m27 path
    /// (deterministic pre-pass, then the resident model every `live_secs`,
    /// then a model batch derive); `segmenter` = deterministic segmentation
    /// over span anchors scored against task evidence, with the batch tier
    /// reduced to a re-score and the model only naming new tasks.
    pub derive_mode: String,
    /// Segmenter: minutes a run of unrelated spans must last before it
    /// becomes a segment of its own (shorter excursions fold in).
    pub segment_switch_min: u32,
    /// Segmenter: minutes a stretch the scorer calls new must last before
    /// a task is created for it.
    pub segment_new_task_min: u32,
    /// Embedding model for the soft tier (m30 chunk 6): a file name in the
    /// models directory (`chronicle model pull bge-small`) or a path. Unset
    /// = no vectors; title words alone carry the soft tier.
    pub embed_model: Option<String>,
    /// Segmenter: the score margin a placement needs to skip "to confirm".
    /// Unset = the scorer's default; `chronicle bench --calibrate` prints
    /// the value the verdict log supports.
    pub scorer_delta: Option<f64>,
    /// AW-compatible HTTP server port.
    pub port: u16,
    /// Extra CORS origin regexes for sideloaded browser extensions
    /// (full-match; stock aw-watcher-web origins are built in).
    pub cors_allow: Vec<String>,
    /// Apps whose focus spans are split per site by browser URL heartbeats
    /// (case-insensitive substring match on the window app name).
    pub browser_apps: Vec<String>,
    /// Regexes; matching apps/titles are never stored at all.
    pub excluded_apps: Vec<String>,
    pub excluded_titles: Vec<String>,
    /// Regexes marking apps/sites as distractions in insights (matched
    /// against the app name and browser site key). Empty = feature off.
    pub distraction_patterns: Vec<String>,
    /// Derived tasks totaling under this many minutes in a day collapse into
    /// the timeline's background strip and stay out of activity-only standup
    /// drafts (0 = off). Declared tasks and tasks with journals, checkpoints,
    /// or a ticket ref never collapse.
    pub background_minutes: u32,
    /// Repo paths polled for branch/commit evidence (`~` expanded).
    /// Empty = git capture off. Repos under a `dev_roots` entry do not need
    /// to be listed here too.
    pub git_repos: Vec<String>,
    /// Folders (`~` expanded) whose immediate git-repo subdirectories are
    /// all watched, the same as if each were listed in `git_repos`: no
    /// config edit for a new clone. See `Config::watched_repos`.
    pub dev_roots: Vec<String>,
    /// Projects (m35 chunk 0): every focus span is filed into the first
    /// project whose rule matches it (repo path or place, ticket prefix,
    /// domain, title regex, app), else left unfiled. Empty = one project per
    /// `git_repos` entry, named after its folder.
    pub projects: Vec<ProjectCfg>,
    /// An unfiled span shorter than this many minutes between two spans of
    /// one project joins that project (a glance at a tab). 0 = off.
    pub project_join_min: u32,
    /// Directories of AI coding transcripts watched for `ai_session`
    /// evidence (`<dir>/<project>/*.jsonl`, Claude Code layout; `~`
    /// expanded). Empty = off.
    pub ai_session_dirs: Vec<String>,
    /// Session formats to read besides the Claude Code dirs above (m37
    /// chunk 0): `codex`, `gemini`, `copilot`, `aider`, `cline`, `amp`,
    /// `opencode`, `cursor`. Empty = every format whose directory exists.
    pub ai_session_formats: Vec<String>,
    /// Poll `gh search prs` for the user's authored/reviewed PRs (needs
    /// `gh auth login`; off by default).
    pub github_prs: bool,
    /// Poll `glab mr list` for the user's assigned/reviewed MRs (needs
    /// `glab auth login`; off by default).
    pub gitlab_mrs: bool,
    /// Watch for apps capturing the microphone (PipeWire, Linux) and store
    /// each stretch as a `call`.
    pub mic_capture: bool,
    /// Poll the primary Google Calendar for `meeting` spans (needs
    /// `chronicle gcal-login`; off by default).
    pub google_calendar: bool,
    /// Fold atuin shell history into `shell` spans per repo (cwd, argv[0]
    /// and duration only; off by default).
    pub shell_history: bool,
    /// Accept WakaTime heartbeats from editor plugins on the local endpoint
    /// and fold them into `edit` spans. Off = the routes answer 403.
    pub editor_heartbeats: bool,
    /// Accept the `chronicle shell-init` precmd hook's posts (cwd, program,
    /// duration; never the command line) on the local endpoint and fold
    /// them into `shell` spans (m37 chunk 1). Off = the route answers 403.
    pub shell_hook: bool,
    /// Scan the parents of `git_repos` entries for git repos no project
    /// claims and file them as discovered projects (m37 chunk 2).
    pub discover_repos: bool,
    /// Read the browsers' history databases (copied, read-only; query
    /// strings dropped) into `browse` rows so a tab's real URL anchors the
    /// span (m37 chunk 4).
    pub browser_history: bool,
    /// ICS calendars (URLs or paths) polled for `meeting` spans, no OAuth
    /// (m37 chunk 4).
    pub calendars: Vec<String>,
    /// Full-match-anywhere regex extracting a ticket key from branch names,
    /// used to anchor derived tasks (`tasks.external_ref`).
    pub ticket_regex: String,
    /// Idle at least this long (lunch-scale) queues a checkpoint per task
    /// with activity since its last one. 0 = feature off.
    pub checkpoint_afk_secs: u32,
    /// An open task nothing has moved for this many days (no interval, no
    /// fresh checkpoint) wears a `stuck` chip. 0 = feature off.
    pub task_stuck_days: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_path: Option<PathBuf>,
    /// A larger model for the day-tier consolidation only (m27 chunk 6);
    /// unset = the same model as everything else.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_path_heavy: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_config: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            batch_minutes: 30,
            batch_min_minutes: 10,
            afk_close_secs: 120,
            quiet_secs: 600,
            away_secs: 1800,
            capture_presence: true,
            focus_route: "auto".into(),
            derive_idle_secs: 300,
            live_secs: 300,
            worker_idle_secs: 1200,
            prepass_secs: 60,
            title_similarity: 0.8,
            retention_days: 180,
            task_autoclose_days: 3,
            derive_mode: "model".into(),
            segment_switch_min: 3,
            segment_new_task_min: 10,
            scorer_delta: None,
            embed_model: None,
            port: 5600,
            cors_allow: Vec::new(),
            browser_apps: [
                "firefox",
                "librewolf",
                "zen",
                "navigator",
                "chrome",
                "chromium",
                "brave",
                "vivaldi",
                "opera",
                "edge",
                "safari",
            ]
            .map(String::from)
            .to_vec(),
            excluded_apps: Vec::new(),
            excluded_titles: Vec::new(),
            distraction_patterns: Vec::new(),
            background_minutes: 10,
            git_repos: Vec::new(),
            dev_roots: Vec::new(),
            projects: Vec::new(),
            project_join_min: 2,
            ai_session_dirs: vec!["~/.claude/projects".into()],
            ai_session_formats: Vec::new(),
            github_prs: false,
            gitlab_mrs: false,
            mic_capture: true,
            google_calendar: false,
            shell_history: false,
            editor_heartbeats: true,
            shell_hook: true,
            discover_repos: true,
            browser_history: true,
            calendars: Vec::new(),
            ticket_regex: "[A-Z][A-Z0-9]+-[0-9]+".into(),
            checkpoint_afk_secs: 1800,
            task_stuck_days: 3,
            model_path: None,
            model_path_heavy: None,
            mcp_config: None,
        }
    }
}

/// One project: a name and the rules that file a span into it. Identity is
/// the git remote of its repos (`host/org/repo`, read at match time); the
/// paths and their worktrees are instances of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProjectCfg {
    pub name: String,
    /// Repo or folder paths (`~` expanded); worktrees of a repo count too.
    pub repos: Vec<String>,
    /// Work-item key prefixes (`ACME`); matched on item anchors and on
    /// branch names, case-insensitively.
    pub tickets: Vec<String>,
    /// Sites (`contoso.atlassian.net`); a subdomain of a listed site matches.
    /// `localhost:<port>` is not needed here: the ports collector maps a dev
    /// server to its repo's place.
    pub domains: Vec<String>,
    /// Window-title regexes (find anywhere).
    pub titles: Vec<String>,
    /// Whole apps (case-insensitive app name).
    pub apps: Vec<String>,
    /// Mint derived sub-tasks inside this project. A project with children
    /// never mints: its own rules catch the client's furniture (the board,
    /// the chat, the calendar) as its other work.
    pub derive: bool,
    /// The project this one sits under (m44 chunk 0): a client above its
    /// code projects. Rules on a child run before rules on any parent, so a
    /// ticket prefix or site listed on the parent is the fallback for a span
    /// no child claims. Reports roll children into their parent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

impl Default for ProjectCfg {
    fn default() -> Self {
        Self {
            name: String::new(),
            repos: Vec::new(),
            tickets: Vec::new(),
            domains: Vec::new(),
            titles: Vec::new(),
            apps: Vec::new(),
            derive: true,
            parent: None,
        }
    }
}

impl Config {
    /// Every repo path in force: `git_repos` as written (in order), then the
    /// immediate git-repo children of each `dev_roots` entry (by name), `~`
    /// expanded, deduped by canonical path. Hidden folders are skipped.
    pub fn watched_repos(&self) -> Vec<PathBuf> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for repo in &self.git_repos {
            let path = expand_home(repo);
            let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
            if seen.insert(canon) {
                out.push(path);
            }
        }
        for root in &self.dev_roots {
            for child in children_repos(&expand_home(root)) {
                let canon = std::fs::canonicalize(&child).unwrap_or_else(|_| child.clone());
                if seen.insert(canon) {
                    out.push(child);
                }
            }
        }
        out
    }

    /// The projects in force: `projects` as written, else one per
    /// `watched_repos()` entry named after its folder (the first-run
    /// default).
    pub fn projects_effective(&self) -> Vec<ProjectCfg> {
        if !self.projects.is_empty() {
            return self.projects.clone();
        }
        self.watched_repos()
            .iter()
            .filter_map(|p| {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().to_ascii_lowercase())?;
                Some(ProjectCfg {
                    name,
                    repos: vec![p.display().to_string()],
                    ..ProjectCfg::default()
                })
            })
            .collect()
    }

    /// Missing file = defaults. A present-but-invalid file is an error, not a silent fallback.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let cfg: Self = toml::from_str(&std::fs::read_to_string(path)?)?;
        validate_projects(&cfg.projects).map_err(ConfigError::Invalid)?;
        Ok(cfg)
    }

    /// Write the whole struct back, the way the Settings panel does; the
    /// daemon reads config.toml at start, so a change applies on its next
    /// start.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        Ok(std::fs::write(path, toml::to_string_pretty(self)?)?)
    }

    /// The MCP allowlist file: `mcp_config` when set, else `mcp.toml` beside
    /// config.toml in the data dir.
    pub fn mcp_path(&self, data_dir: &Path) -> PathBuf {
        self.mcp_config
            .clone()
            .unwrap_or_else(|| data_dir.join("mcp.toml"))
    }
}

/// The one place the `[[projects]]` block is written (m44 chunk 3): the
/// file as it stands with `projects` and `project_join_min` replaced,
/// validated, then saved whole. Every surface that adds a rule (the
/// Settings editor, the timeline and Home menus, `project attach`, the
/// repo card) goes through here. A missing file starts from the defaults.
pub fn write_projects(
    path: &Path,
    projects: Vec<ProjectCfg>,
    join_min: Option<u32>,
) -> Result<Config, ConfigError> {
    validate_projects(&projects).map_err(ConfigError::Invalid)?;
    for p in &projects {
        for t in &p.titles {
            regex::Regex::new(t).map_err(|e| {
                ConfigError::Invalid(format!("project `{}`: title regex {t}: {e}", p.name))
            })?;
        }
    }
    let mut config = Config::load(path)?;
    config.projects = projects;
    if let Some(j) = join_min {
        config.project_join_min = j;
    }
    config.save(path)?;
    Ok(config)
}

/// One rule to add to a project by name (m44 chunk 3), from any surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectRule {
    Repo(String),
    Ticket(String),
    Domain(String),
    Title(String),
    App(String),
    /// Put the project under this one (`None` lifts it to the top).
    Parent(Option<String>),
}

impl ProjectRule {
    pub fn kind(&self) -> &'static str {
        match self {
            ProjectRule::Repo(_) => "repo",
            ProjectRule::Ticket(_) => "ticket",
            ProjectRule::Domain(_) => "domain",
            ProjectRule::Title(_) => "title",
            ProjectRule::App(_) => "app",
            ProjectRule::Parent(_) => "parent",
        }
    }

    pub fn value(&self) -> &str {
        match self {
            ProjectRule::Repo(v)
            | ProjectRule::Ticket(v)
            | ProjectRule::Domain(v)
            | ProjectRule::Title(v)
            | ProjectRule::App(v) => v,
            ProjectRule::Parent(p) => p.as_deref().unwrap_or(""),
        }
    }
}

/// Add `rules` to the project named `name` in the effective projects
/// (m44 chunk 3): a name no project has becomes a new `[[projects]]`
/// entry at the end. Names compare case-insensitively; a rule already
/// listed is not listed twice. Returns the projects to write and whether
/// anything changed.
pub fn attach_rules(config: &Config, name: &str, rules: &[ProjectRule]) -> (Vec<ProjectCfg>, bool) {
    let mut projects = config.projects_effective();
    let name = name.trim();
    let at = match projects
        .iter()
        .position(|p| p.name.trim().eq_ignore_ascii_case(name))
    {
        Some(i) => i,
        None => {
            projects.push(ProjectCfg {
                name: name.to_owned(),
                ..ProjectCfg::default()
            });
            projects.len() - 1
        }
    };
    let mut changed = config.projects.is_empty();
    let push = |list: &mut Vec<String>, v: &str, changed: &mut bool| {
        let v = v.trim();
        if v.is_empty() || list.iter().any(|x| x.trim().eq_ignore_ascii_case(v)) {
            return;
        }
        list.push(v.to_owned());
        *changed = true;
    };
    for rule in rules {
        let p = &mut projects[at];
        match rule {
            ProjectRule::Repo(v) => push(&mut p.repos, v, &mut changed),
            ProjectRule::Ticket(v) => push(&mut p.tickets, &v.to_ascii_uppercase(), &mut changed),
            ProjectRule::Domain(v) => push(&mut p.domains, &v.to_ascii_lowercase(), &mut changed),
            ProjectRule::Title(v) => push(&mut p.titles, v, &mut changed),
            ProjectRule::App(v) => push(&mut p.apps, v, &mut changed),
            ProjectRule::Parent(parent) => {
                let parent = parent
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                if p.parent != parent {
                    p.parent = parent;
                    changed = true;
                }
            }
        }
    }
    (projects, changed)
}

/// A title as a rule that matches it and nothing much else: the exact
/// text, regex-escaped, anchored at both ends.
pub fn title_rule(title: &str) -> String {
    format!("^{}$", regex::escape(title.trim()))
}

/// The project tree is well formed: every `parent` names a configured
/// project, no project is its own ancestor, and no two projects share a
/// name. Names compare case-insensitively, as `Matcher::resolve` does.
pub fn validate_projects(projects: &[ProjectCfg]) -> Result<(), String> {
    let name_of = |p: &ProjectCfg| p.name.trim().to_owned();
    let parent_of = |name: &str| -> Option<String> {
        projects
            .iter()
            .find(|q| name_of(q).eq_ignore_ascii_case(name))
            .and_then(|q| q.parent.as_deref())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    for (i, p) in projects.iter().enumerate() {
        let name = name_of(p);
        if name.is_empty() {
            continue;
        }
        if projects[..i]
            .iter()
            .any(|q| name_of(q).eq_ignore_ascii_case(&name))
        {
            return Err(format!("project `{name}` is listed twice"));
        }
        let Some(parent) = p.parent.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        if !projects
            .iter()
            .any(|q| name_of(q).eq_ignore_ascii_case(parent))
        {
            return Err(format!(
                "project `{name}`: parent `{parent}` is not a project"
            ));
        }
        let mut cur = parent.to_owned();
        for _ in 0..=projects.len() {
            if cur.eq_ignore_ascii_case(&name) {
                return Err(format!(
                    "project `{name}`: its parent chain loops back to it"
                ));
            }
            match parent_of(&cur) {
                Some(next) => cur = next,
                None => break,
            }
        }
    }
    Ok(())
}

/// A `.git` directory only: a linked worktree or submodule carries a `.git`
/// file, and either belongs to a repo that is watched in its own right.
fn is_main_repo(path: &Path) -> bool {
    path.join(".git").is_dir()
}

/// Immediate children of `root` that are git repos, hidden folders skipped,
/// sorted by name. A root that does not exist yields no children. Kept here
/// rather than in `project`, which this module cannot depend on without a
/// cycle (`project` already depends on `config`).
fn children_repos(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        if name.starts_with('.') || !is_main_repo(&path) {
            continue;
        }
        out.push(path);
    }
    out.sort();
    out
}

/// `~/x` → `$HOME/x`; anything else passes through unchanged. Config paths
/// are stored as written so config.toml stays portable between machines.
pub fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::var_os("HOME")) {
        (Some(rest), Some(home)) => PathBuf::from(home).join(rest),
        _ => PathBuf::from(path),
    }
}

/// Bare command → absolute path from PATH plus the usual user bin dirs. The
/// daemon's PATH under systemd is minimal, so a bare `uvx` or `gh` that works
/// in a terminal fails there; resolving up front sidesteps that. Unresolved
/// commands pass through unchanged.
pub fn resolve_command(cmd: &str) -> String {
    if cmd.contains('/') {
        return cmd.to_owned();
    }
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        dirs.push(home.join(".local/bin"));
        dirs.push(home.join(".cargo/bin"));
    }
    dirs.into_iter()
        .map(|d| d.join(cmd))
        .find(|p| p.is_file())
        .map_or_else(|| cmd.to_owned(), |p| p.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_home_only_touches_tilde_slash() {
        let Some(home) = std::env::var_os("HOME") else {
            return;
        };
        assert_eq!(expand_home("~/dev/x"), PathBuf::from(home).join("dev/x"));
        assert_eq!(expand_home("/abs/path"), PathBuf::from("/abs/path"));
        assert_eq!(expand_home("relative"), PathBuf::from("relative"));
        assert_eq!(expand_home("~"), PathBuf::from("~"));
    }

    #[test]
    fn watched_repos_orders_dedupes_and_skips_hidden() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("dev");
        for name in ["alpha", "beta"] {
            std::fs::create_dir_all(root.join(name).join(".git")).unwrap();
        }
        std::fs::create_dir_all(root.join(".hidden").join(".git")).unwrap();
        std::fs::create_dir_all(root.join("plain")).unwrap();
        std::fs::create_dir_all(root.join("linked")).unwrap();
        std::fs::write(
            root.join("linked").join(".git"),
            "gitdir: ../alpha/.git/worktrees/linked",
        )
        .unwrap();
        let outside = tmp.path().join("elsewhere");
        std::fs::create_dir_all(outside.join(".git")).unwrap();

        let cfg = Config {
            git_repos: vec![
                outside.display().to_string(),
                root.join("beta").display().to_string(),
            ],
            dev_roots: vec![root.display().to_string()],
            ..Config::default()
        };
        // `git_repos` as written, then unclaimed root children by name;
        // `beta` is both, so it appears once, at its `git_repos` position,
        // and the hidden, plain (non-git) and linked-worktree folders never
        // show.
        assert_eq!(
            cfg.watched_repos(),
            vec![outside, root.join("beta"), root.join("alpha")]
        );
    }

    fn named(name: &str, parent: Option<&str>) -> ProjectCfg {
        ProjectCfg {
            name: name.into(),
            parent: parent.map(str::to_owned),
            ..ProjectCfg::default()
        }
    }

    #[test]
    fn validate_projects_checks_parents_and_cycles() {
        assert!(validate_projects(&[named("acme", None), named("web", Some("Acme"))]).is_ok());
        let err = validate_projects(&[named("web", Some("acme"))]).unwrap_err();
        assert!(err.contains("parent `acme` is not a project"), "{err}");
        let err = validate_projects(&[named("a", Some("b")), named("b", Some("a"))]).unwrap_err();
        assert!(err.contains("loops back"), "{err}");
        let err = validate_projects(&[named("a", Some("a"))]).unwrap_err();
        assert!(err.contains("loops back"), "{err}");
        let err = validate_projects(&[named("a", None), named("A", None)]).unwrap_err();
        assert!(err.contains("listed twice"), "{err}");
    }

    #[test]
    fn attach_rules_adds_once_and_creates_a_missing_project() {
        let cfg = Config {
            projects: vec![ProjectCfg {
                name: "acme".into(),
                domains: vec!["acme.com".into()],
                ..ProjectCfg::default()
            }],
            ..Config::default()
        };
        let (projects, changed) = attach_rules(
            &cfg,
            "Acme",
            &[
                ProjectRule::Domain("ACME.com".into()),
                ProjectRule::Ticket("acme".into()),
                ProjectRule::Title(title_rule(" Board (1) ")),
            ],
        );
        assert!(changed);
        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].domains, ["acme.com"]);
        assert_eq!(projects[0].tickets, ["ACME"]);
        assert_eq!(projects[0].titles, ["^Board \\(1\\)$"]);
        let (_, changed) = attach_rules(&cfg, "acme", &[ProjectRule::Domain("acme.com".into())]);
        assert!(!changed);
        let (projects, changed) = attach_rules(
            &cfg,
            "acme-web",
            &[
                ProjectRule::Repo("~/dev/web".into()),
                ProjectRule::Parent(Some("acme".into())),
            ],
        );
        assert!(changed);
        assert_eq!(projects.len(), 2);
        assert_eq!(projects[1].name, "acme-web");
        assert_eq!(projects[1].repos, ["~/dev/web"]);
        assert_eq!(projects[1].parent.as_deref(), Some("acme"));
    }

    #[test]
    fn write_projects_validates_and_keeps_the_rest_of_the_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "port = 5601\n[[projects]]\nname = \"a\"\n").unwrap();
        let err = write_projects(&path, vec![named("b", Some("zz"))], None).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)), "{err}");
        let cfg = write_projects(
            &path,
            vec![named("b", None), named("c", Some("b"))],
            Some(4),
        )
        .unwrap();
        assert_eq!(cfg.port, 5601);
        let back = Config::load(&path).unwrap();
        assert_eq!(back.project_join_min, 4);
        assert_eq!(back.projects.len(), 2);
        assert_eq!(back.projects[1].parent.as_deref(), Some("b"));
    }

    #[test]
    fn load_rejects_a_parent_no_project_has() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "[[projects]]\nname = \"web\"\nparent = \"acme\"\n").unwrap();
        let err = Config::load(&path).unwrap_err();
        assert!(matches!(err, ConfigError::Invalid(_)), "{err}");
    }

    #[test]
    fn projects_effective_names_dev_root_children_when_projects_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("dev");
        std::fs::create_dir_all(root.join("widget").join(".git")).unwrap();
        let cfg = Config {
            dev_roots: vec![root.display().to_string()],
            ..Config::default()
        };
        let cfgs = cfg.projects_effective();
        assert_eq!(cfgs.len(), 1);
        assert_eq!(cfgs[0].name, "widget");
        assert_eq!(
            cfgs[0].repos,
            vec![root.join("widget").display().to_string()]
        );
    }
}
