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

/// One server found in an `mcpServers` JSON document (Claude Code
/// `~/.claude.json`, Claude Desktop `claude_desktop_config.json`, a repo's
/// `.mcp.json`). `skip` names why it cannot be imported as-is.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportEntry {
    pub server: ServerConfig,
    pub skip: Option<String>,
}

/// Parse the `mcpServers` map (top-level key, or the whole document when it
/// is already the map). Only stdio servers import: entries with a `url` or
/// a non-stdio `type` come back with `skip` set so the UI can list them.
/// Bare commands are left as written; the caller resolves them.
pub fn parse_mcp_servers_json(text: &str) -> Result<Vec<ImportEntry>, String> {
    let doc: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let map = match doc.get("mcpServers") {
        Some(m) => m,
        None => &doc,
    };
    let Some(map) = map.as_object() else {
        return Err("no mcpServers object".into());
    };
    let mut out = Vec::new();
    for (name, v) in map {
        let mut skip = None;
        let kind = v.get("type").and_then(|t| t.as_str()).unwrap_or("stdio");
        if kind != "stdio" {
            skip = Some(format!("{kind} transport not supported"));
        } else if v.get("url").is_some() {
            skip = Some("remote server (url) not supported".into());
        }
        let command = v
            .get("command")
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_owned();
        if skip.is_none() && command.is_empty() {
            skip = Some("no command".into());
        }
        let args = v
            .get("args")
            .and_then(|a| a.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        // `env` is an object in every writer we know; Claude Code has been
        // seen writing `[]` for none — anything but an object is empty.
        let env = v
            .get("env")
            .and_then(|e| e.as_object())
            .map(|o| {
                o.iter()
                    .filter_map(|(k, val)| val.as_str().map(|s| (k.clone(), s.to_owned())))
                    .collect()
            })
            .unwrap_or_default();
        out.push(ImportEntry {
            server: ServerConfig {
                name: name.clone(),
                command,
                args,
                enabled: true,
                env,
            },
            skip,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn import_json_reads_stdio_entries_and_flags_the_rest() {
        let text = r#"{"mcpServers": {
            "context-mode": {"type": "stdio", "command": "npx", "args": ["-y", "context-mode"], "env": []},
            "jira": {"command": "uvx", "args": ["mcp-atlassian"], "env": {"JIRA_URL": "https://x", "N": 1}},
            "linear": {"type": "http", "url": "https://mcp.linear.app/mcp"},
            "remote": {"command": "npx", "url": "https://x/sse"},
            "empty": {}
        }}"#;
        let got = parse_mcp_servers_json(text).unwrap();
        let by = |n: &str| got.iter().find(|e| e.server.name == n).unwrap();
        assert_eq!(by("context-mode").server.args, vec!["-y", "context-mode"]);
        assert!(by("context-mode").server.env.is_empty());
        assert_eq!(by("context-mode").skip, None);
        assert_eq!(by("jira").server.env.get("JIRA_URL").unwrap(), "https://x");
        assert_eq!(by("jira").server.env.len(), 1, "non-string env dropped");
        assert_eq!(
            by("linear").skip.as_deref(),
            Some("http transport not supported")
        );
        assert_eq!(
            by("remote").skip.as_deref(),
            Some("remote server (url) not supported")
        );
        assert_eq!(by("empty").skip.as_deref(), Some("no command"));

        // Bare map (a `.mcp.json` that is only the servers) works too.
        let bare = r#"{"a": {"command": "a-server"}}"#;
        assert_eq!(
            parse_mcp_servers_json(bare).unwrap()[0].server.command,
            "a-server"
        );
        assert!(parse_mcp_servers_json("[1]").is_err());
        assert!(parse_mcp_servers_json("{").is_err());
    }

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
