//! rmcp client wrapper, stdio transport only, allowlisted context calls.
//!
//! Everything a server returns is untrusted labeling data; the derive
//! grammar is the containment layer. Failures here are never fatal to
//! derivation — log and skip.

mod config;
pub use config::{ContextCall, McpConfig, McpConfigError, ServerConfig};

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResponse};
use rmcp::transport::TokioChildProcess;
use tokio::process::Command;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const CALL_TIMEOUT: Duration = Duration::from_secs(10);
/// ~800 tokens by the digest's chars/4 heuristic.
const MAX_CONTEXT_CHARS: usize = 800 * 4;
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
    run_blocking(&cfg, &cfg.context_calls, MAX_CONTEXT_CHARS)
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
    let calls: Vec<ContextCall> = cfg
        .fetch_calls
        .iter()
        .map(|c| ContextCall {
            args_json: c.args_json.as_ref().map(|raw| substitute_ref(raw, ext_ref)),
            ..c.clone()
        })
        .collect();
    run_blocking(&cfg, &calls, MAX_FETCH_CHARS)
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

async fn connect(
    server: &ServerConfig,
) -> anyhow::Result<rmcp::service::RunningService<rmcp::RoleClient, ()>> {
    let mut cmd = Command::new(&server.command);
    cmd.args(&server.args).envs(&server.env);
    // Server stderr must not leak into the derive worker's (quiet) stderr.
    let (transport, _stderr) = TokioChildProcess::builder(cmd)
        .stderr(Stdio::null())
        .spawn()?;
    Ok(().serve(transport).await?)
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
    Ok(text.join("\n").trim().to_owned())
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
