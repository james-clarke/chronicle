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
            prepass_secs: 60,
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
            ai_session_dirs: vec!["~/.claude/projects".into()],
            github_prs: false,
            mic_capture: true,
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
