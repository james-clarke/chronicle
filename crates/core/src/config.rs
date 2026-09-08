use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
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
    pub capture_presence: bool,
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
    /// Empty = git capture off.
    pub git_repos: Vec<String>,
    /// Directories of AI coding transcripts watched for `ai_session`
    /// evidence (`<dir>/<project>/*.jsonl`, Claude Code layout; `~`
    /// expanded). Empty = off.
    pub ai_session_dirs: Vec<String>,
    /// Poll `gh search prs` for the user's authored/reviewed PRs (needs
    /// `gh auth login`; off by default).
    pub github_prs: bool,
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
            ai_session_dirs: vec!["~/.claude/projects".into()],
            github_prs: false,
            mic_capture: true,
            google_calendar: false,
            shell_history: false,
            editor_heartbeats: true,
            ticket_regex: "[A-Z][A-Z0-9]+-[0-9]+".into(),
            checkpoint_afk_secs: 1800,
            task_stuck_days: 3,
            model_path: None,
            model_path_heavy: None,
            mcp_config: None,
        }
    }
}

impl Config {
    /// Missing file = defaults. A present-but-invalid file is an error, not a silent fallback.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        Ok(toml::from_str(&std::fs::read_to_string(path)?)?)
    }

    /// The MCP allowlist file: `mcp_config` when set, else `mcp.toml` beside
    /// config.toml in the data dir.
    pub fn mcp_path(&self, data_dir: &Path) -> PathBuf {
        self.mcp_config
            .clone()
            .unwrap_or_else(|| data_dir.join("mcp.toml"))
    }
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
}
