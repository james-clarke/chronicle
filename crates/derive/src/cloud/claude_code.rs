//! Headless Claude Code as a text backend (m36 chunk 0): `claude -p` on the
//! user's own login, so a subscription works where there is no API key.
//! One subprocess per request, prompt on stdin, `--output-format json`
//! parsed for the text, the usage and Claude Code's own cost estimate.
//! Structured output goes through `--json-schema`, which spends a tool
//! turn, so the turn cap is 3. `--bare` is never passed: it skips the
//! stored login. The working directory is a scratch dir, never a repo,
//! because Claude Code reads `CLAUDE.md` from wherever it starts.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::CloudError;
use crate::text::{Completion, JobKind, Request, TextBackend};

pub const DEFAULT_COMMAND: &str = "claude";
/// Structured output is a tool call under the hood: the model's reply, the
/// tool result, the final turn.
const MAX_TURNS: &str = "3";
/// A Claude Code call has never taken close to this; the daemon reaps a
/// worker at 300 s and the job must fail before then.
const TIMEOUT: Duration = Duration::from_secs(240);
const POLL: Duration = Duration::from_millis(100);

pub struct ClaudeCodeBackend {
    name: String,
    model: String,
    command: String,
}

impl ClaudeCodeBackend {
    pub fn new(name: &str, model: &str, command: Option<&str>) -> Self {
        Self {
            name: name.to_owned(),
            model: model.to_owned(),
            command: command
                .map(str::trim)
                .filter(|c| !c.is_empty())
                .unwrap_or(DEFAULT_COMMAND)
                .to_owned(),
        }
    }

    /// One tiny request, for the Settings test button: `Ok(latency)` or the
    /// classified failure (a missing binary, not logged in).
    pub fn probe(&self) -> Result<Duration, CloudError> {
        let req = Request {
            job: JobKind::TaskDescription,
            system: None,
            user: "Reply with the single word OK.",
            history: &[],
            schema: None,
            max_output: 8,
        };
        let t0 = Instant::now();
        self.call(&req)?;
        Ok(t0.elapsed())
    }

    fn args(&self, req: &Request<'_>) -> Vec<String> {
        let mut args: Vec<String> = [
            "-p",
            "--output-format",
            "json",
            "--model",
            &self.model,
            "--max-turns",
            MAX_TURNS,
            "--no-session-persistence",
            "--tools",
            "",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        if let Some(schema) = req.schema {
            args.push("--json-schema".into());
            args.push(schema.to_string());
        }
        if let Some(system) = req.system {
            args.push("--system-prompt".into());
            args.push(system.to_owned());
        }
        args
    }

    /// Chat history folds into the prompt: one subprocess has no memory of
    /// the last, and `--resume` would persist sessions we do not want kept.
    fn prompt(req: &Request<'_>) -> String {
        if req.history.is_empty() {
            return req.user.to_owned();
        }
        let mut out = String::from("Earlier in this conversation:\n\n");
        for (q, a) in req.history {
            out.push_str("User: ");
            out.push_str(q);
            out.push_str("\nAssistant: ");
            out.push_str(a);
            out.push_str("\n\n");
        }
        out.push_str("User: ");
        out.push_str(req.user);
        out
    }

    fn scratch_dir() -> PathBuf {
        let dir = std::env::temp_dir().join("chronicle-claude-code");
        let _ = std::fs::create_dir_all(&dir);
        dir
    }

    fn call(&self, req: &Request<'_>) -> Result<Completion, CloudError> {
        let t0 = Instant::now();
        let mut child = Command::new(&self.command)
            .args(self.args(req))
            .current_dir(Self::scratch_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::NotFound {
                    CloudError::Transport(format!("{} not found on PATH", self.command))
                } else {
                    CloudError::Transport(format!("spawn {}: {e}", self.command))
                }
            })?;
        let prompt = Self::prompt(req);
        if let Some(mut stdin) = child.stdin.take() {
            // A closed pipe here means the process died; its exit tells why.
            let _ = stdin.write_all(prompt.as_bytes());
        }
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let out_reader = std::thread::spawn(move || read_all(stdout));
        let err_reader = std::thread::spawn(move || read_all(stderr));
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if t0.elapsed() > TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(CloudError::Transport(format!(
                        "{} took longer than {} s",
                        self.command,
                        TIMEOUT.as_secs()
                    )));
                }
                Ok(None) => std::thread::sleep(POLL),
                Err(e) => return Err(CloudError::Transport(format!("wait: {e}"))),
            }
        };
        let stdout = out_reader.join().unwrap_or_default();
        let stderr = err_reader.join().unwrap_or_default();
        let mut c = parse_output(&stdout, &stderr, status.success(), req.schema.is_some())?;
        c.wall_ms = t0.elapsed().as_millis() as u64;
        Ok(c)
    }
}

fn read_all(mut r: impl Read) -> String {
    let mut s = String::new();
    let _ = r.read_to_string(&mut s);
    s
}

fn brief(s: &str) -> String {
    let s = s.trim();
    let mut b: String = s.chars().take(160).collect();
    if b.len() < s.len() {
        b.push('…');
    }
    b
}

fn is_login_failure(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    t.contains("not logged in") || t.contains("please run /login") || t.contains("invalid api key")
}

/// The `--output-format json` result object. Warnings from wrappers can
/// precede it on stdout, so parsing starts at the first `{`.
fn parse_output(
    stdout: &str,
    stderr: &str,
    exited_ok: bool,
    want_json: bool,
) -> Result<Completion, CloudError> {
    let start = stdout.find('{');
    let doc: Option<Value> = start.and_then(|i| serde_json::from_str(&stdout[i..]).ok());
    let Some(doc) = doc else {
        let text = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        return Err(if is_login_failure(text) {
            CloudError::Auth(401, brief(text))
        } else if !exited_ok {
            CloudError::Transport(format!("claude exited with an error: {}", brief(text)))
        } else {
            CloudError::Stopped(format!("no result object in output: {}", brief(text)))
        });
    };
    let result_text = doc.get("result").and_then(Value::as_str).unwrap_or("");
    if doc
        .get("is_error")
        .and_then(Value::as_bool)
        .unwrap_or(false)
        || !exited_ok
    {
        let subtype = doc
            .get("subtype")
            .and_then(Value::as_str)
            .unwrap_or("error");
        let detail = if result_text.is_empty() {
            stderr
        } else {
            result_text
        };
        return Err(if is_login_failure(detail) {
            CloudError::Auth(401, brief(detail))
        } else {
            CloudError::Stopped(format!("{subtype}: {}", brief(detail)))
        });
    }
    let text = match doc.get("structured_output") {
        Some(v) if want_json && v.is_object() => v.to_string(),
        _ => result_text.trim().to_owned(),
    };
    if text.is_empty() {
        return Err(CloudError::Stopped("empty result".into()));
    }
    let usage = doc.get("usage").cloned().unwrap_or(Value::Null);
    let n = |k: &str| usage.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
    let cache_read = n("cache_read_input_tokens");
    let cache_write = n("cache_creation_input_tokens");
    Ok(Completion {
        text,
        input_tokens: n("input_tokens")
            .saturating_add(cache_read)
            .saturating_add(cache_write),
        output_tokens: n("output_tokens"),
        cache_read_tokens: cache_read,
        wall_ms: 0,
        cost_usd: doc.get("total_cost_usd").and_then(Value::as_f64),
        redactions: Vec::new(),
    })
}

impl TextBackend for ClaudeCodeBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn context_tokens(&self) -> usize {
        if self.model.contains("haiku") {
            180_000
        } else {
            900_000
        }
    }

    /// Not streamed: `json` output arrives whole, so `on_token` sees the
    /// text once. Chat through this backend renders at the end.
    fn complete(
        &self,
        req: &Request<'_>,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<Completion> {
        let c = self.call(req)?;
        on_token(&c.text);
        Ok(c)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const OK: &str = r#"{"type":"result","subtype":"success","is_error":false,"result":"{\"label\":\"x\"}","structured_output":{"label":"x"},"total_cost_usd":0.0147,"usage":{"input_tokens":20,"output_tokens":370,"cache_read_input_tokens":64314,"cache_creation_input_tokens":3233}}"#;

    #[test]
    fn parses_structured_output_usage_and_cost() {
        let c = parse_output(&format!("warning line\n{OK}"), "", true, true).unwrap();
        assert_eq!(c.text, r#"{"label":"x"}"#);
        assert_eq!(c.input_tokens, 20 + 64314 + 3233);
        assert_eq!(c.cache_read_tokens, 64314);
        assert_eq!(c.output_tokens, 370);
        assert_eq!(c.cost_usd, Some(0.0147));
        // A prose job takes `result` even when structured_output is present.
        let c = parse_output(OK, "", true, false).unwrap();
        assert_eq!(c.text, r#"{"label":"x"}"#);
    }

    #[test]
    fn not_logged_in_is_auth_and_never_retried() {
        let out = r#"{"type":"result","subtype":"success","is_error":true,"result":"Not logged in · Please run /login","total_cost_usd":0}"#;
        let e = parse_output(out, "", true, false).unwrap_err();
        assert!(matches!(e, CloudError::Auth(401, _)), "{e}");
        assert!(!e.is_transient());
    }

    #[test]
    fn max_turns_and_garbage_are_stopped_or_transport() {
        let out = r#"{"type":"result","subtype":"error_max_turns","is_error":true,"result":null}"#;
        let e = parse_output(out, "", true, true).unwrap_err();
        assert_eq!(e.brief(), "stopped: error_max_turns: ");
        let e = parse_output("", "boom", false, false).unwrap_err();
        assert!(matches!(e, CloudError::Transport(_)), "{e}");
        assert!(e.brief().contains("boom"));
    }

    #[test]
    fn args_and_prompt_shape() {
        let b = ClaudeCodeBackend::new("claude", "claude-sonnet-5", Some(" "));
        assert_eq!(b.command, DEFAULT_COMMAND);
        let schema = json!({"type": "object"});
        let req = Request {
            job: JobKind::Chat,
            system: Some("sys"),
            user: "now",
            history: &[("q1".into(), "a1".into())],
            schema: Some(&schema),
            max_output: 0,
        };
        let args = b.args(&req);
        assert!(!args.iter().any(|a| a == "--bare"));
        let at = |flag: &str| {
            args.iter()
                .position(|a| a == flag)
                .map(|i| args[i + 1].clone())
        };
        assert_eq!(at("--model").as_deref(), Some("claude-sonnet-5"));
        assert_eq!(at("--max-turns").as_deref(), Some(MAX_TURNS));
        assert_eq!(at("--json-schema").as_deref(), Some(r#"{"type":"object"}"#));
        assert_eq!(at("--system-prompt").as_deref(), Some("sys"));
        let p = ClaudeCodeBackend::prompt(&req);
        assert!(p.starts_with("Earlier in this conversation:"));
        assert!(p.ends_with("User: now"));
        assert!(p.contains("Assistant: a1"));
    }

    #[cfg(unix)]
    #[test]
    fn spawns_the_command_and_reads_its_json() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("chronicle-cc-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("claude");
        std::fs::write(
            &fake,
            format!("#!/bin/sh\ncat >/dev/null\nprintf '%s' '{OK}'\n"),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let b = ClaudeCodeBackend::new("claude", "sonnet", fake.to_str());
        let req = Request {
            job: JobKind::NameTask,
            system: None,
            user: "hi",
            history: &[],
            schema: None,
            max_output: 0,
        };
        let mut seen = Vec::new();
        let c = b.complete(&req, &mut |t| seen.push(t.to_owned())).unwrap();
        assert_eq!(seen, [c.text.clone()]);
        assert_eq!(c.cost_usd, Some(0.0147));
        let missing = ClaudeCodeBackend::new("claude", "sonnet", Some("/nonexistent/claude"));
        let e = missing.probe().unwrap_err();
        assert!(matches!(e, CloudError::Transport(_)), "{e}");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
