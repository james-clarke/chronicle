use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("failed to read config: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse config: {0}")]
    Parse(#[from] toml::de::Error),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Batch = this many minutes of non-AFK activity.
    pub batch_minutes: u32,
    /// Close spans when idle at least this long.
    pub afk_close_secs: u32,
    /// Derivation may start when idle at least this long.
    pub derive_idle_secs: u32,
    pub retention_days: u32,
    /// AW-compatible HTTP server port.
    pub port: u16,
    /// Regexes; matching apps/titles are never stored at all.
    pub excluded_apps: Vec<String>,
    pub excluded_titles: Vec<String>,
    pub model_path: Option<PathBuf>,
    pub mcp_config: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            batch_minutes: 30,
            afk_close_secs: 120,
            derive_idle_secs: 300,
            retention_days: 180,
            port: 5600,
            excluded_apps: Vec::new(),
            excluded_titles: Vec::new(),
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
