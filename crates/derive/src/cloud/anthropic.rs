//! Anthropic Messages API over ureq 3 (sync), streaming SSE. Hand-rolled:
//! the request shape is a dozen fields and the unofficial crates lag the
//! API. The key lives in the struct and the `x-api-key` header, nowhere
//! else; errors carry the provider's message only.

use std::io::BufReader;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::sse::SseReader;
use super::{CloudError, effort_for, max_output_for};
use crate::text::{Completion, JobKind, Request, TextBackend};

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const API_VERSION: &str = "2023-06-01";
const MAX_TRIES: u32 = 3;
/// Retries stop once this much wall time has gone; the daemon reaps a
/// worker after `DERIVE_TIMEOUT` (300 s) and the job must fail before that.
const RETRY_BUDGET: Duration = Duration::from_secs(120);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

pub struct AnthropicBackend {
    name: String,
    model: String,
    api_key: String,
    base_url: String,
}

impl AnthropicBackend {
    pub fn new(name: &str, model: &str, api_key: &str, base_url: Option<&str>) -> Self {
        Self {
            name: name.to_owned(),
            model: model.to_owned(),
            api_key: api_key.to_owned(),
            base_url: base_url
                .unwrap_or(DEFAULT_BASE_URL)
                .trim_end_matches('/')
                .to_owned(),
        }
    }

    /// One tiny request, for the Settings test button: `Ok(latency)` or the
    /// classified failure.
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
        self.call(&req, &mut |_| {})?;
        Ok(t0.elapsed())
    }

    fn body(&self, req: &Request<'_>) -> String {
        let mut messages = Vec::with_capacity(req.history.len() * 2 + 1);
        for (q, a) in req.history {
            messages.push(json!({"role": "user", "content": q}));
            messages.push(json!({"role": "assistant", "content": a}));
        }
        messages.push(json!({"role": "user", "content": req.user}));
        let mut output_config = json!({ "effort": effort_for(req.job) });
        if let Some(schema) = req.schema {
            output_config["format"] = json!({ "type": "json_schema", "schema": schema });
        }
        let mut body = json!({
            "model": self.model,
            "max_tokens": if req.max_output == 0 { max_output_for(req.job) } else { req.max_output },
            "stream": true,
            "messages": messages,
            "output_config": output_config,
        });
        if let Some(system) = req.system {
            body["system"] = json!([{
                "type": "text",
                "text": system,
                "cache_control": {"type": "ephemeral"},
            }]);
        }
        body.to_string()
    }

    /// POST with retries on 429/529/5xx and transport failures.
    fn call(
        &self,
        req: &Request<'_>,
        on_token: &mut dyn FnMut(&str),
    ) -> Result<Completion, CloudError> {
        let body = self.body(req);
        let url = format!("{}/v1/messages", self.base_url);
        let started = Instant::now();
        let mut last = CloudError::Transport("no attempt made".into());
        for attempt in 0..MAX_TRIES {
            let t0 = Instant::now();
            match self.once(&url, &body, on_token) {
                Ok(mut c) => {
                    c.wall_ms = t0.elapsed().as_millis() as u64;
                    return Ok(c);
                }
                Err((e, retry_after)) => {
                    if !e.is_transient() {
                        return Err(e);
                    }
                    tracing::warn!(attempt, "anthropic call failed: {}", e.brief());
                    last = e;
                    let wait = retry_after
                        .unwrap_or_else(|| Duration::from_secs(1 << attempt))
                        .min(Duration::from_secs(30));
                    if started.elapsed() + wait > RETRY_BUDGET || attempt + 1 == MAX_TRIES {
                        break;
                    }
                    std::thread::sleep(wait);
                }
            }
        }
        Err(last)
    }

    /// One HTTP exchange. `Err` carries the `retry-after` hint when the
    /// server sent one.
    fn once(
        &self,
        url: &str,
        body: &str,
        on_token: &mut dyn FnMut(&str),
    ) -> Result<Completion, (CloudError, Option<Duration>)> {
        let resp = ureq::post(url)
            .config()
            .http_status_as_error(false)
            .timeout_connect(Some(CONNECT_TIMEOUT))
            .timeout_recv_response(Some(RESPONSE_TIMEOUT))
            .build()
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", API_VERSION)
            .header("content-type", "application/json")
            .header("accept", "text/event-stream")
            .send(body)
            .map_err(|e| (CloudError::Transport(transport_brief(&e)), None))?;
        let status = resp.status().as_u16();
        if status != 200 {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(Duration::from_secs);
            let text = resp.into_body().read_to_string().unwrap_or_default();
            return Err((classify_status(status, &text), retry_after));
        }
        let reader = BufReader::new(resp.into_body().into_reader());
        parse_stream(reader, on_token).map_err(|e| (e, None))
    }
}

impl TextBackend for AnthropicBackend {
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

    fn complete(
        &self,
        req: &Request<'_>,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<Completion> {
        Ok(self.call(req, on_token)?)
    }
}

/// A transport error's shape without any of its payload.
fn transport_brief(e: &ureq::Error) -> String {
    match e {
        ureq::Error::Timeout(t) => format!("timeout ({t:?})"),
        ureq::Error::Io(io) => format!("io: {}", io.kind()),
        ureq::Error::HostNotFound => "host not found".into(),
        ureq::Error::ConnectionFailed => "connection failed".into(),
        ureq::Error::Tls(_) => "tls".into(),
        other => {
            // Variant name only; ureq's Display for some variants echoes
            // the URL, which is fine, but never a header.
            let s = other.to_string();
            s.split(':').next().unwrap_or("error").trim().to_owned()
        }
    }
}

/// The API's `{"error": {"type", "message"}}` body, or the raw text head.
fn api_message(text: &str) -> String {
    let m = serde_json::from_str::<Value>(text)
        .ok()
        .and_then(|v| {
            let e = v.get("error")?;
            let t = e.get("type").and_then(Value::as_str).unwrap_or("");
            let m = e.get("message").and_then(Value::as_str).unwrap_or("");
            Some(if t.is_empty() {
                m.to_owned()
            } else {
                format!("{t}: {m}")
            })
        })
        .unwrap_or_else(|| text.chars().take(160).collect());
    m.trim().to_owned()
}

fn classify_status(status: u16, text: &str) -> CloudError {
    let m = api_message(text);
    match status {
        401 | 403 => CloudError::Auth(status, m),
        429 => CloudError::RateLimited(m),
        529 | 500..=599 => CloudError::Server(status, m),
        _ => CloudError::BadRequest(status, m),
    }
}

/// Walk the event stream: text deltas to `on_token`, usage from
/// `message_start` and `message_delta`, `stop_reason` decides success.
fn parse_stream<R: std::io::BufRead>(
    reader: R,
    on_token: &mut dyn FnMut(&str),
) -> Result<Completion, CloudError> {
    let mut c = Completion::default();
    let mut stop_reason: Option<String> = None;
    let mut finished = false;
    for ev in SseReader::new(reader) {
        let ev = ev.map_err(|e| CloudError::Transport(format!("stream: {}", e.kind())))?;
        let data: Value = match serde_json::from_str(&ev.data) {
            Ok(v) => v,
            Err(_) if ev.data.is_empty() => continue,
            Err(_) => return Err(CloudError::Stopped("malformed stream event".into())),
        };
        let kind = data
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or(ev.event.as_str());
        match kind {
            "message_start" => {
                let usage = &data["message"]["usage"];
                c.input_tokens = usage["input_tokens"].as_u64().unwrap_or(0) as u32;
                c.cache_read_tokens = usage["cache_read_input_tokens"].as_u64().unwrap_or(0) as u32;
            }
            "content_block_delta" => {
                let delta = &data["delta"];
                if delta["type"] == "text_delta"
                    && let Some(t) = delta["text"].as_str()
                {
                    c.text.push_str(t);
                    on_token(t);
                }
            }
            "message_delta" => {
                if let Some(s) = data["delta"]["stop_reason"].as_str() {
                    stop_reason = Some(s.to_owned());
                }
                if let Some(n) = data["usage"]["output_tokens"].as_u64() {
                    c.output_tokens = n as u32;
                }
                if let Some(n) = data["usage"]["input_tokens"].as_u64() {
                    c.input_tokens = n as u32;
                }
                if let Some(n) = data["usage"]["cache_read_input_tokens"].as_u64() {
                    c.cache_read_tokens = n as u32;
                }
            }
            "message_stop" => {
                finished = true;
                break;
            }
            "error" => {
                return Err(CloudError::Stopped(api_message(&ev.data)));
            }
            _ => {} // ping, content_block_start/stop
        }
    }
    if !finished {
        return Err(CloudError::Transport("stream ended early".into()));
    }
    match stop_reason.as_deref() {
        Some("end_turn") => Ok(c),
        Some(other) => Err(CloudError::Stopped(other.to_owned())),
        None => Err(CloudError::Stopped("no stop reason".into())),
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;

    use super::*;

    /// Serve `responses` in order on a loopback port, one connection each;
    /// returns the base URL and the captured request bodies.
    fn mock(responses: Vec<String>) -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for resp in responses {
                let (mut s, _) = listener.accept().unwrap();
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                let body_start;
                loop {
                    let n = s.read(&mut tmp).unwrap();
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        body_start = i + 4;
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&buf[..body_start]).to_string();
                let len: usize = head
                    .lines()
                    .find_map(|l| {
                        l.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse().unwrap())
                    })
                    .unwrap_or(0);
                while buf.len() < body_start + len {
                    let n = s.read(&mut tmp).unwrap();
                    buf.extend_from_slice(&tmp[..n]);
                }
                let body = String::from_utf8_lossy(&buf[body_start..]).to_string();
                tx.send(format!("{head}\n{body}")).unwrap();
                s.write_all(resp.as_bytes()).unwrap();
                s.flush().unwrap();
            }
        });
        (base, rx)
    }

    fn http(status: &str, extra: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 {status}\r\ncontent-length: {}\r\n{extra}connection: close\r\n\r\n{body}",
            body.len()
        )
    }

    const STREAM: &str = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"usage\":{\"input_tokens\":120,\"cache_read_input_tokens\":100}}}\n\nevent: content_block_start\ndata: {\"type\":\"content_block_start\",\"index\":0}\n\nevent: ping\ndata: {\"type\":\"ping\"}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"Hel\"}}\n\nevent: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"lo\"}}\n\nevent: message_delta\ndata: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":7}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";

    #[test]
    fn streams_text_and_usage() {
        let (base, rx) = mock(vec![http(
            "200 OK",
            "content-type: text/event-stream\r\n",
            STREAM,
        )]);
        let b = AnthropicBackend::new("a", "claude-opus-5", "sk-ant-test", Some(&base));
        let schema = json!({"type": "object", "properties": {}, "required": [], "additionalProperties": false});
        let req = Request {
            job: JobKind::Checkpoint,
            system: Some("sys"),
            user: "hi",
            history: &[("q1".into(), "a1".into())],
            schema: Some(&schema),
            max_output: 0,
        };
        let mut pieces = Vec::new();
        let c = b
            .complete(&req, &mut |t| pieces.push(t.to_owned()))
            .unwrap();
        assert_eq!(c.text, "Hello");
        assert_eq!(pieces, ["Hel", "lo"]);
        assert_eq!(
            (c.input_tokens, c.cache_read_tokens, c.output_tokens),
            (120, 100, 7)
        );
        let sent = rx.recv().unwrap();
        assert!(sent.contains("x-api-key: sk-ant-test"));
        assert!(sent.contains("anthropic-version: 2023-06-01"));
        let body: Value = serde_json::from_str(sent.split_once("\n\n").unwrap().1.trim()).unwrap();
        assert_eq!(body["model"], "claude-opus-5");
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], 600);
        assert_eq!(body["output_config"]["effort"], "low");
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(body["system"][0]["cache_control"]["type"], "ephemeral");
        assert_eq!(body["messages"].as_array().unwrap().len(), 3);
        assert_eq!(body["messages"][1]["role"], "assistant");
        assert_eq!(body["messages"][2]["content"], "hi");
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn retries_rate_limit_then_succeeds() {
        let (base, _rx) = mock(vec![
            http(
                "429 Too Many Requests",
                "retry-after: 0\r\ncontent-type: application/json\r\n",
                r#"{"type":"error","error":{"type":"rate_limit_error","message":"slow down"}}"#,
            ),
            http("200 OK", "content-type: text/event-stream\r\n", STREAM),
        ]);
        let b = AnthropicBackend::new("a", "claude-sonnet-5", "k", Some(&base));
        let req = Request {
            job: JobKind::Journal,
            system: None,
            user: "x",
            history: &[],
            schema: None,
            max_output: 50,
        };
        let c = b.complete(&req, &mut |_| {}).unwrap();
        assert_eq!(c.text, "Hello");
    }

    #[test]
    fn auth_failure_is_not_retried_and_hides_the_key() {
        let (base, _rx) = mock(vec![http(
            "401 Unauthorized",
            "content-type: application/json\r\n",
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
        )]);
        let b = AnthropicBackend::new("a", "claude-opus-5", "sk-ant-secret", Some(&base));
        let err = b.probe().unwrap_err();
        assert_eq!(
            err,
            CloudError::Auth(401, "authentication_error: invalid x-api-key".into())
        );
        assert!(!err.brief().contains("sk-ant-secret"));
    }

    #[test]
    fn non_end_turn_and_stream_errors_fail() {
        let cut = STREAM.replace("end_turn", "max_tokens");
        let err = parse_stream(cut.as_bytes(), &mut |_| {}).unwrap_err();
        assert_eq!(err, CloudError::Stopped("max_tokens".into()));
        let mid = "event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
        let err = parse_stream(mid.as_bytes(), &mut |_| {}).unwrap_err();
        assert_eq!(
            err,
            CloudError::Stopped("overloaded_error: Overloaded".into())
        );
        let early = &STREAM[..STREAM.len() - 40];
        assert!(matches!(
            parse_stream(early.as_bytes(), &mut |_| {}).unwrap_err(),
            CloudError::Transport(_)
        ));
    }
}
