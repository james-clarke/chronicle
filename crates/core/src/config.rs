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
    /// Close spans when idle at least this long.
    pub afk_close_secs: u32,
    /// Derivation may start when idle at least this long.
    pub derive_idle_secs: u32,
    /// Consecutive same-app events merge into one span when normalized title
    /// similarity is at least this (absorbs jitter like unread-count prefixes).
    pub title_similarity: f64,
    /// Rows older than this are pruned daily (0 = keep forever).
    /// Corrections are always kept.
    pub retention_days: u32,
    /// Open derived tasks with no interval this many days are auto-closed
    /// daily (0 = never; declared tasks only close by hand).
    pub task_autoclose_days: u32,
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
    /// Full-match-anywhere regex extracting a ticket key from branch names,
    /// used to anchor derived tasks (`tasks.external_ref`).
    pub ticket_regex: String,
    /// Idle at least this long (lunch-scale) queues a checkpoint per task
    /// with activity since its last one. 0 = feature off.
    pub checkpoint_afk_secs: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_config: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            batch_minutes: 30,
            afk_close_secs: 120,
            derive_idle_secs: 300,
            title_similarity: 0.8,
            retention_days: 180,
            task_autoclose_days: 3,
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
            ticket_regex: "[A-Z][A-Z0-9]+-[0-9]+".into(),
            checkpoint_afk_secs: 1800,
            model_path: None,
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
}
