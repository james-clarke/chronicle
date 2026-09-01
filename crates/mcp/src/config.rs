use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

#[derive(Debug, thiserror::Error)]
pub enum McpConfigError {
    #[error("failed to read mcp config: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse mcp config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("context call references unknown server \"{0}\"")]
    UnknownServer(String),
    #[error("context call args_json for {server}.{tool} is not a JSON object: {err}")]
    BadArgs {
        server: String,
        tool: String,
        err: serde_json::Error,
    },
}

/// Explicit allowlist config: every call the gatherer may make is spelled out
/// here (tool + args); no dynamic tool selection in v1.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    pub servers: Vec<ServerConfig>,
    pub context_calls: Vec<ContextCall>,
    /// Per-task context fetch (m16): same allowlist shape, but each call's
    /// `args_json` may carry a `{ref}` placeholder replaced with the task's
    /// external_ref at fetch time. Empty = feature off.
    pub fetch_calls: Vec<ContextCall>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCall {
    pub server: String,
    pub tool: String,
    /// JSON object literal, passed verbatim as the tool's arguments.
    #[serde(default)]
    pub args_json: Option<String>,
}

impl McpConfig {
    /// Missing file = empty config (MCP off), matching core `Config::load`.
    /// A present-but-invalid file is an error, not a silent fallback.
    pub fn load(path: &Path) -> Result<Self, McpConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let cfg: Self = toml::from_str(&std::fs::read_to_string(path)?)?;
        for call in cfg.context_calls.iter().chain(&cfg.fetch_calls) {
            if !cfg.servers.iter().any(|s| s.name == call.server) {
                return Err(McpConfigError::UnknownServer(call.server.clone()));
            }
            if let Some(raw) = &call.args_json
                && let Err(err) =
                    serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(raw)
            {
                return Err(McpConfigError::BadArgs {
                    server: call.server.clone(),
                    tool: call.tool.clone(),
                    err,
                });
            }
        }
        Ok(cfg)
    }
}
