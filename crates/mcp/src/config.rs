use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum McpConfigError {
    #[error("failed to read mcp config: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to parse mcp config: {0}")]
    Parse(#[from] toml::de::Error),
    #[error("failed to serialize mcp config: {0}")]
    Serialize(#[from] toml::ser::Error),
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
///
/// Serialize: the settings panel writes the whole struct back to mcp.toml
/// (hand comments are lost). Empty lists and default values are skipped so
/// the written file stays as small as a hand-written one.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct McpConfig {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub servers: Vec<ServerConfig>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub context_calls: Vec<ContextCall>,
    /// Per-task context fetch (m16): same allowlist shape, but each call's
    /// `args_json` may carry a `{ref}` placeholder replaced with the task's
    /// external_ref at fetch time. Empty = feature off.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fetch_calls: Vec<ContextCall>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    pub name: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// Disabled servers keep their allowlist entries but are never spawned
    /// (settings toggle) — the file stays valid either way.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContextCall {
    pub server: String,
    pub tool: String,
    /// JSON object literal, passed verbatim as the tool's arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args_json: Option<String>,
}

fn default_true() -> bool {
    true
}

fn is_true(v: &bool) -> bool {
    *v
}

impl McpConfig {
    /// Missing file = empty config (MCP off), matching core `Config::load`.
    /// A present-but-invalid file is an error, not a silent fallback.
    pub fn load(path: &Path) -> Result<Self, McpConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let cfg: Self = toml::from_str(&std::fs::read_to_string(path)?)?;
        cfg.validate()?;
        Ok(cfg)
    }

    /// Every call must name a listed server (enabled or not) and carry a
    /// JSON object for its arguments.
    pub fn validate(&self) -> Result<(), McpConfigError> {
        for call in self.context_calls.iter().chain(&self.fetch_calls) {
            if !self.servers.iter().any(|s| s.name == call.server) {
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
        Ok(())
    }

    /// Write the whole config back (settings panel). Atomic — temp file in
    /// the same directory, then rename — so a daemon loading mid-save never
    /// sees a torn file. Mode 0600: `env` carries API tokens.
    pub fn save(&self, path: &Path) -> Result<(), McpConfigError> {
        use std::io::Write;

        self.validate()?;
        let text = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
        // A leftover from an interrupted save may carry other permissions;
        // `mode` only applies on create.
        let _ = std::fs::remove_file(&tmp);
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        {
            let mut file = opts.open(&tmp)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
        }
        std::fs::rename(&tmp, path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn sample() -> McpConfig {
        McpConfig {
            servers: vec![
                ServerConfig {
                    name: "jira".into(),
                    command: "/usr/bin/uvx".into(),
                    args: vec!["mcp-atlassian".into()],
                    enabled: true,
                    env: BTreeMap::from([("JIRA_API_TOKEN".to_owned(), "s3cret".to_owned())]),
                },
                ServerConfig {
                    name: "off".into(),
                    command: "sleep".into(),
                    args: Vec::new(),
                    enabled: false,
                    env: BTreeMap::new(),
                },
            ],
            context_calls: vec![ContextCall {
                server: "jira".into(),
                tool: "jira_search".into(),
                args_json: Some(r#"{"jql": "assignee = currentUser()"}"#.into()),
            }],
            fetch_calls: vec![ContextCall {
                server: "off".into(),
                tool: "get".into(),
                args_json: None,
            }],
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chronicle-mcp-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn toml_round_trip_keeps_servers_and_calls() {
        let cfg = sample();
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: McpConfig = toml::from_str(&text).unwrap();
        assert_eq!(back, cfg);
        // Defaults stay out of the written file; the disabled flag goes in.
        assert_eq!(text.matches("enabled").count(), 1, "{text}");
        assert_eq!(text.matches("args_json").count(), 1, "{text}");
    }

    #[test]
    fn enabled_defaults_true() {
        let cfg: McpConfig =
            toml::from_str("[[servers]]\nname = \"s\"\ncommand = \"c\"\n").unwrap();
        assert!(cfg.servers[0].enabled);
    }

    #[test]
    fn save_is_private_and_reloads() {
        let dir = temp_dir("save");
        let path = dir.join("mcp.toml");
        let cfg = sample();
        cfg.save(&path).unwrap();
        assert!(!dir.join("mcp.toml.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert_eq!(McpConfig::load(&path).unwrap(), cfg);
        // Overwrite keeps working (the temp file is recreated each time).
        cfg.save(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn save_rejects_calls_to_unknown_servers() {
        let dir = temp_dir("reject");
        let path = dir.join("mcp.toml");
        let mut cfg = sample();
        cfg.servers.pop();
        let err = cfg.save(&path).unwrap_err();
        assert!(
            matches!(err, McpConfigError::UnknownServer(ref s) if s == "off"),
            "{err}"
        );
        assert!(!path.exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
