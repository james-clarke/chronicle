//! rmcp client wrapper, stdio transport only, allowlisted context calls.
//!
//! Everything a server returns is untrusted labeling data; the derive
//! grammar is the containment layer. Failures here are never fatal to
//! derivation — log and skip.

mod config;
pub use config::{
    ActionCall, ContextCall, ImportEntry, McpConfig, McpConfigError, ServerConfig,
    parse_mcp_servers_json,
};

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context as _;
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResponse};
use rmcp::transport::TokioChildProcess;
use tokio::process::Command;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// Spawn + handshake + tools/list for the settings "test" button. Looser
/// than `CONNECT_TIMEOUT`: `uvx`-style launchers cold-start slowly.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// ~500 tokens: tool JSON tokenizes at ~2.7 chars per token (measured
/// 2026-09-02), and the whole digest has ~1900. Results are compacted
/// first (`compact_json`), so this holds roughly what 3200 pretty-printed
/// chars did at half the tokens.
const MAX_CONTEXT_CHARS: usize = 1400;
/// Task context is a standalone document the user reads (and chat injects),
/// not squeezed into the digest's cap ladder — bigger budget.
const MAX_FETCH_CHARS: usize = 6000;

/// Run every allowlisted context call from the TOML config at `config_path`
/// and format the results for the digest's `## Workspace context` section.
/// None = nothing to inject: missing/empty config, or every call failed.
pub fn gather_context(config_path: &Path) -> Option<String> {
    let cfg = match McpConfig::load(config_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!("mcp config {}: {e}", config_path.display());
            return None;
        }
    };
    if cfg.context_calls.is_empty() {
        return None;
    }
    let now = jiff::Zoned::now();
    let calls: Vec<ContextCall> = cfg
        .context_calls
        .iter()
        .map(|c| ContextCall {
            args_json: c.args_json.as_ref().map(|raw| substitute_time(raw, &now)),
            ..c.clone()
        })
        .collect();
    run_blocking(&cfg, &calls, MAX_CONTEXT_CHARS)
}

/// Fetch external context for one task ref (m16): run every `fetch_calls`
/// entry with `{ref}` in its args replaced by `ext_ref`. None = feature off
/// (no fetch_calls), bad config, or every call failed.
pub fn fetch_context(config_path: &Path, ext_ref: &str) -> Option<String> {
    let cfg = match McpConfig::load(config_path) {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!("mcp config {}: {e}", config_path.display());
            return None;
        }
    };
    if cfg.fetch_calls.is_empty() {
        return None;
    }
    let now = jiff::Zoned::now();
    let calls: Vec<ContextCall> = cfg
        .fetch_calls
        .iter()
        .map(|c| ContextCall {
            args_json: c
                .args_json
                .as_ref()
                .map(|raw| substitute_time(&substitute_ref(raw, ext_ref), &now)),
            ..c.clone()
        })
        .collect();
    run_blocking(&cfg, &calls, MAX_FETCH_CHARS)
}

/// Run one allowlisted action call (m26): the user clicked "post" in the
/// task pane. `key` fills `{key}` (the ticket) and `body` fills `{body}`
/// (the journal entry or checkpoint). Ok = the tool's reply.
///
/// `action` must be listed in `action_calls` in the file at `config_path`:
/// the UI passes back the entry it showed in the dialog and this re-reads
/// the file of record before anything leaves the machine. Nothing else in
/// this crate can reach an action call — the read paths take their calls
/// from `context_calls` / `fetch_calls` only.
pub fn run_action(
    config_path: &Path,
    action: &ActionCall,
    key: &str,
    body: &str,
) -> anyhow::Result<String> {
    let cfg = McpConfig::load(config_path)
        .with_context(|| format!("mcp config {}", config_path.display()))?;
    if !cfg.action_calls.contains(action) {
        anyhow::bail!(
            "{}.{} is not an action call in {}",
            action.server,
            action.tool,
            config_path.display()
        );
    }
    let server = cfg
        .servers
        .iter()
        .find(|s| s.name == action.server)
        .with_context(|| format!("unknown server {}", action.server))?;
    if !server.enabled {
        anyhow::bail!("server {} is disabled", server.name);
    }
    let call = ContextCall {
        server: action.server.clone(),
        tool: action.tool.clone(),
        args_json: action
            .args_json
            .as_ref()
            .map(|raw| substitute_action(raw, key, body)),
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("mcp runtime")?;
    rt.block_on(async {
        let mut client = tokio::time::timeout(CONNECT_TIMEOUT, connect(server))
            .await
            .map_err(|_| anyhow::anyhow!("connect timed out"))??;
        let out = tokio::time::timeout(CALL_TIMEOUT, run_call(&client, &call))
            .await
            .map_err(|_| anyhow::anyhow!("timed out"))?;
        if let Err(e) = client.close().await {
            tracing::debug!("mcp server {}: close: {e}", server.name);
        }
        out
    })
}

/// Replace `{key}` and `{body}` in an action template. Both placeholders sit
/// inside JSON string literals and both carry user text (a whole journal
/// entry for `{body}`), so each is JSON-escaped first.
fn substitute_action(raw: &str, key: &str, body: &str) -> String {
    raw.replace("{key}", &json_escape(key))
        .replace("{body}", &json_escape(body))
}

/// The text of a JSON string literal, without its quotes.
fn json_escape(s: &str) -> String {
    let quoted = serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_owned());
    quoted[1..quoted.len() - 1].to_owned()
}

/// Replace `{now}`, `{today}` (local midnight) and `{tomorrow}` (next
/// midnight) with RFC 3339 timestamps in the local offset — what calendar
/// servers' `timeMin`/`timeMax`/`start`/`end` take. Untouched when absent.
fn substitute_time(raw: &str, now: &jiff::Zoned) -> String {
    if !raw.contains("{now}") && !raw.contains("{today}") && !raw.contains("{tomorrow}") {
        return raw.to_owned();
    }
    let today = now.start_of_day().unwrap_or_else(|_| now.clone());
    let tomorrow = today
        .checked_add(jiff::Span::new().days(1))
        .unwrap_or_else(|_| today.clone());
    let fmt = |z: &jiff::Zoned| z.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string();
    raw.replace("{now}", &fmt(now))
        .replace("{today}", &fmt(&today))
        .replace("{tomorrow}", &fmt(&tomorrow))
}

/// Replace `{ref}` inside an args template. The ref is JSON-escaped first:
/// the placeholder sits inside a string literal and the ref is user-editable,
/// so a quote in it must not break out of the argument value.
fn substitute_ref(raw: &str, ext_ref: &str) -> String {
    let escaped = serde_json::to_string(ext_ref).unwrap_or_default();
    raw.replace("{ref}", escaped.trim_matches('"'))
}

fn run_blocking(cfg: &McpConfig, calls: &[ContextCall], max_chars: usize) -> Option<String> {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            tracing::warn!("mcp runtime: {e}");
            return None;
        }
    };
    let out = rt.block_on(gather(cfg, calls));
    let out = out.trim();
    if out.is_empty() {
        return None;
    }
    Some(truncate_chars(out, max_chars))
}

async fn gather(cfg: &McpConfig, all_calls: &[ContextCall]) -> String {
    let mut out = String::new();
    for server in &cfg.servers {
        let calls: Vec<&ContextCall> = all_calls
            .iter()
            .filter(|c| c.server == server.name)
            .collect();
        if calls.is_empty() {
            continue;
        }
        if !server.enabled {
            tracing::debug!(
                "mcp server {}: disabled, skipping {} call(s)",
                server.name,
                calls.len()
            );
            continue;
        }
        let mut client = match tokio::time::timeout(CONNECT_TIMEOUT, connect(server)).await {
            Ok(Ok(client)) => client,
            Ok(Err(e)) => {
                tracing::warn!("mcp server {}: connect failed: {e:#}", server.name);
                continue;
            }
            Err(_) => {
                tracing::warn!("mcp server {}: connect timed out", server.name);
                continue;
            }
        };
        for call in calls {
            match tokio::time::timeout(CALL_TIMEOUT, run_call(&client, call)).await {
                Ok(Ok(text)) => {
                    out.push_str(&format!("### {}.{}\n{}\n", call.server, call.tool, text));
                }
                Ok(Err(e)) => {
                    tracing::warn!("mcp call {}.{}: {e:#}", call.server, call.tool);
                }
                Err(_) => {
                    tracing::warn!("mcp call {}.{}: timed out", call.server, call.tool);
                }
            }
        }
        if let Err(e) = client.close().await {
            tracing::debug!("mcp server {}: close: {e}", server.name);
        }
    }
    out
}

/// What a live server said about itself — the settings panel's "test"
/// verdict and the `mcp-check` per-server line.
#[derive(Debug, Clone)]
pub struct ServerProbe {
    pub server_name: String,
    pub server_version: String,
    pub tools: Vec<String>,
    pub elapsed: Duration,
}

/// Spawn `server`, complete the handshake, list its tools, shut it down.
/// Blocking, on its own current-thread runtime like the gatherers. The
/// error chain is the user's diagnostic (spawn failure names the command).
pub fn probe_server(server: &ServerConfig) -> anyhow::Result<ServerProbe> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("mcp runtime")?;
    rt.block_on(async {
        let start = std::time::Instant::now();
        let (server_name, server_version, tools) =
            tokio::time::timeout(PROBE_TIMEOUT, probe(server))
                .await
                .map_err(|_| anyhow::anyhow!("timed out after {}s", PROBE_TIMEOUT.as_secs()))??;
        Ok(ServerProbe {
            server_name,
            server_version,
            tools,
            elapsed: start.elapsed(),
        })
    })
}

async fn probe(server: &ServerConfig) -> anyhow::Result<(String, String, Vec<String>)> {
    let mut client = connect(server).await?;
    let (name, version) = client
        .peer_info()
        .and_then(|info| {
            info.server_info
                .as_ref()
                .map(|s| (s.name.clone(), s.version.clone()))
        })
        .unwrap_or_default();
    let tools = client.list_all_tools().await.context("tools/list")?;
    let names = tools.into_iter().map(|t| t.name.into_owned()).collect();
    if let Err(e) = client.close().await {
        tracing::debug!("mcp server {}: close: {e}", server.name);
    }
    Ok((name, version, names))
}

async fn connect(
    server: &ServerConfig,
) -> anyhow::Result<rmcp::service::RunningService<rmcp::RoleClient, ()>> {
    let mut cmd = Command::new(&server.command);
    cmd.args(&server.args).envs(&server.env);
    // Server stderr must not leak into the derive worker's (quiet) stderr.
    let (transport, _stderr) = TokioChildProcess::builder(cmd)
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn {}", server.command))?;
    ().serve(transport).await.context("handshake")
}

async fn run_call(
    client: &rmcp::service::RunningService<rmcp::RoleClient, ()>,
    call: &ContextCall,
) -> anyhow::Result<String> {
    let mut params = CallToolRequestParams::new(call.tool.clone());
    if let Some(raw) = &call.args_json {
        // Validated as an object at config load.
        params = params.with_arguments(serde_json::from_str::<
            serde_json::Map<String, serde_json::Value>,
        >(raw)?);
    }
    let CallToolResponse::Complete(result) = client.call_tool_once(params).await? else {
        anyhow::bail!("unsupported response kind (task/input-required)");
    };
    if result.is_error == Some(true) {
        anyhow::bail!("tool returned an error");
    }
    let text: Vec<&str> = result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|t| t.text.as_str()))
        .collect();
    if text.is_empty() {
        anyhow::bail!("no text content in result");
    }
    Ok(text
        .iter()
        .map(|t| compact_json(t))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned())
}

/// Pretty-printed JSON re-serialized without whitespace: same content,
/// ~15 % fewer tokens and a much smaller char count under the digest cap.
/// Anything that is not one JSON document passes through unchanged.
fn compact_json(text: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(v) => serde_json::to_string(&v).unwrap_or_else(|_| text.to_owned()),
        Err(_) => text.to_owned(),
    }
}

fn truncate_chars(s: &str, max_chars: usize) -> String {
    let mut it = s.chars();
    let head: String = it.by_ref().take(max_chars).collect();
    if it.next().is_some() {
        head + "\u{2026}"
    } else {
        head
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn compact_json_strips_whitespace_and_passes_text_through() {
        assert_eq!(
            super::compact_json("{\n  \"a\": [1, 2],\n  \"b\": \"x y\"\n}"),
            r#"{"a":[1,2],"b":"x y"}"#
        );
        assert_eq!(super::compact_json("plain: text"), "plain: text");
    }

    #[test]
    fn substitute_ref_escapes_json_metacharacters() {
        assert_eq!(
            super::substitute_ref(r#"{"issueIdOrKey":"{ref}"}"#, "ABC-123"),
            r#"{"issueIdOrKey":"ABC-123"}"#
        );
        // A quote in a user-edited ref stays inside the string literal.
        let out = super::substitute_ref(r#"{"q":"{ref}"}"#, "a\"b");
        assert_eq!(out, r#"{"q":"a\"b"}"#);
        serde_json::from_str::<serde_json::Map<String, serde_json::Value>>(&out).unwrap();
    }
}

#[cfg(test)]
mod action_tests {
    use super::{ActionCall, McpConfig, ServerConfig, run_action, substitute_action};
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("chronicle-act-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const JIRA_ARGS: &str = r#"{"issue_key": "{key}", "comment": "{body}"}"#;

    #[test]
    fn substitute_action_fills_key_and_body_as_json_strings() {
        let out = substitute_action(JIRA_ARGS, "ACME-12", "shipped the fold");
        assert_eq!(
            out,
            r#"{"issue_key": "ACME-12", "comment": "shipped the fold"}"#
        );
        // A multi-line body with quotes stays inside its string literal.
        let out = substitute_action(JIRA_ARGS, "ACME-12", "said \"go\"\nthen went");
        let args: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(args["comment"], "said \"go\"\nthen went");
        assert_eq!(args["issue_key"], "ACME-12");
        // A body ending in a quote is not truncated.
        let out = substitute_action(JIRA_ARGS, "K-1", "ends with \"");
        let args: serde_json::Map<String, serde_json::Value> = serde_json::from_str(&out).unwrap();
        assert_eq!(args["comment"], "ends with \"");
        // No placeholders, no change.
        assert_eq!(substitute_action(r#"{"a":"b"}"#, "K", "B"), r#"{"a":"b"}"#);
    }

    /// A config whose only calls are actions: the read paths must find
    /// nothing to run — an action call is reachable from a click and
    /// nowhere else. The server's command would leave a marker file if it
    /// were ever spawned.
    #[test]
    fn read_paths_never_run_action_calls() {
        let dir = temp_dir("read-paths");
        let marker = dir.join("spawned");
        let action = ActionCall {
            server: "fake".into(),
            tool: "jira_add_comment".into(),
            args_json: Some(JIRA_ARGS.into()),
            label: "comment on {key}".into(),
        };
        let cfg = McpConfig {
            servers: vec![ServerConfig {
                name: "fake".into(),
                command: "/bin/sh".into(),
                args: vec!["-c".into(), format!("touch {}", marker.display())],
                enabled: true,
                env: Default::default(),
            }],
            context_calls: Vec::new(),
            fetch_calls: Vec::new(),
            action_calls: vec![action.clone()],
        };
        let path = dir.join("mcp.toml");
        cfg.save(&path).unwrap();
        assert!(super::gather_context(&path).is_none());
        assert!(super::fetch_context(&path, "ACME-1").is_none());
        assert!(!marker.exists(), "a read path spawned the action's server");

        // An action the file does not list is refused before any spawn.
        let unlisted = ActionCall {
            tool: "jira_delete_issue".into(),
            ..action
        };
        let err = run_action(&path, &unlisted, "ACME-1", "body").unwrap_err();
        assert!(
            format!("{err:#}").contains("is not an action call"),
            "{err:#}"
        );
        assert!(!marker.exists(), "a refused action spawned the server");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[cfg(test)]
mod time_tests {
    use super::substitute_time;

    #[test]
    fn time_placeholders_expand_to_local_rfc3339() {
        let now: jiff::Zoned = "2026-09-02T16:58:00-04:00[America/New_York]"
            .parse()
            .unwrap();
        let got = substitute_time(
            r#"{"timeMin":"{today}","timeMax":"{tomorrow}","at":"{now}"}"#,
            &now,
        );
        assert_eq!(
            got,
            r#"{"timeMin":"2026-09-02T00:00:00-04:00","timeMax":"2026-09-03T00:00:00-04:00","at":"2026-09-02T16:58:00-04:00"}"#
        );
        assert_eq!(substitute_time(r#"{"q":"x"}"#, &now), r#"{"q":"x"}"#);
    }
}
