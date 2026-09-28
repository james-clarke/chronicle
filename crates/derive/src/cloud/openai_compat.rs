//! Chat Completions over ureq 3 (sync), streaming SSE: the wire shape
//! OpenRouter, OpenAI and most self-hosted servers speak. OpenRouter is the
//! default base URL, so one key reaches every model it lists. The key lives
//! in the struct and the `authorization` header, nowhere else; errors carry
//! the provider's message only.

use std::io::BufRead;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::sse::SseReader;
use super::transport::{api_message, post_stream};
use super::{CloudError, max_output_for};
use crate::text::{Completion, JobKind, Request, TextBackend};

pub const DEFAULT_BASE_URL: &str = "https://openrouter.ai/api/v1";
/// Prompt budget when the model id says nothing about its window. Every
/// current frontier model on OpenRouter takes at least 128k; older or
/// self-hosted ones may not, and the base URL is the only hint we have.
const CONTEXT_TOKENS: usize = 100_000;

pub struct OpenAiCompatBackend {
    name: String,
    model: String,
    api_key: String,
    base_url: String,
}

impl OpenAiCompatBackend {
    pub fn new(name: &str, model: &str, api_key: &str, base_url: Option<&str>) -> Self {
        Self {
            name: name.to_owned(),
            model: model.to_owned(),
            api_key: api_key.to_owned(),
            base_url: base_url
                .map(str::trim)
                .filter(|u| !u.is_empty())
                .unwrap_or(DEFAULT_BASE_URL)
                .trim_end_matches('/')
                .to_owned(),
        }
    }

    /// The base URL's host, lowercased: what decides the vendor extras.
    fn host(&self) -> String {
        let rest = self
            .base_url
            .split_once("://")
            .map_or(self.base_url.as_str(), |(_, r)| r);
        rest.split(['/', ':'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase()
    }

    fn is_openrouter(&self) -> bool {
        self.host() == "openrouter.ai"
    }

    fn is_openai(&self) -> bool {
        self.host() == "api.openai.com"
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
        let mut messages = Vec::with_capacity(req.history.len() * 2 + 2);
        if let Some(system) = req.system {
            messages.push(json!({"role": "system", "content": system}));
        }
        for (q, a) in req.history {
            messages.push(json!({"role": "user", "content": q}));
            messages.push(json!({"role": "assistant", "content": a}));
        }
        messages.push(json!({"role": "user", "content": req.user}));
        let mut body = json!({
            "model": self.model,
            "stream": true,
            "stream_options": { "include_usage": true },
            "messages": messages,
        });
        // OpenAI itself rejects `max_tokens` for its reasoning models and
        // takes the newer name for all of them; every other server still
        // reads the classic field, and some reject the new one.
        let max_field = if self.is_openai() {
            "max_completion_tokens"
        } else {
            "max_tokens"
        };
        let max_output = if req.max_output == 0 {
            max_output_for(req.job)
        } else {
            req.max_output
        };
        body[max_field] = json!(max_output);
        if let Some(schema) = req.schema {
            body["response_format"] = json!({
                "type": "json_schema",
                "json_schema": { "name": "output", "strict": true, "schema": schema },
            });
        }
        if self.is_openrouter() {
            // OpenRouter prices the call itself in `usage.cost`; other
            // servers reject the field, so it goes only where it is read.
            body["usage"] = json!({ "include": true });
        }
        body.to_string()
    }

    /// POST with retries on 429/5xx and transport failures.
    fn call(
        &self,
        req: &Request<'_>,
        on_token: &mut dyn FnMut(&str),
    ) -> Result<Completion, CloudError> {
        let body = self.body(req);
        let url = format!("{}/chat/completions", self.base_url);
        let auth = format!("Bearer {}", self.api_key);
        post_stream(
            "chat completions",
            &url,
            &[
                ("authorization", auth.as_str()),
                // OpenRouter's app attribution headers; other servers
                // ignore them.
                ("http-referer", "https://github.com/james-clarke/chronicle"),
                ("x-title", "Chronicle"),
            ],
            &body,
            on_token,
            parse_stream,
        )
    }
}

impl TextBackend for OpenAiCompatBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn model(&self) -> &str {
        &self.model
    }

    fn context_tokens(&self) -> usize {
        CONTEXT_TOKENS
    }

    fn complete(
        &self,
        req: &Request<'_>,
        on_token: &mut dyn FnMut(&str),
    ) -> anyhow::Result<Completion> {
        Ok(self.call(req, on_token)?)
    }
}

/// Walk the chunk stream: `choices[0].delta.content` to `on_token`, the
/// usage object from whichever chunk carries it (the last, with
/// `include_usage`), `finish_reason` decides success, `[DONE]` ends it.
fn parse_stream(
    reader: &mut dyn BufRead,
    on_token: &mut dyn FnMut(&str),
) -> Result<Completion, CloudError> {
    let mut c = Completion::default();
    let mut finish_reason: Option<String> = None;
    let mut finished = false;
    for ev in SseReader::new(reader) {
        let ev = ev.map_err(|e| CloudError::Transport(format!("stream: {}", e.kind())))?;
        if ev.data.trim() == "[DONE]" {
            finished = true;
            break;
        }
        let data: Value = match serde_json::from_str(&ev.data) {
            Ok(v) => v,
            Err(_) if ev.data.is_empty() => continue,
            Err(_) => return Err(CloudError::Stopped("malformed stream event".into())),
        };
        if data.get("error").is_some() {
            return Err(CloudError::Stopped(api_message(&ev.data)));
        }
        if let Some(choice) = data["choices"].get(0) {
            if let Some(t) = choice["delta"]["content"].as_str()
                && !t.is_empty()
            {
                c.text.push_str(t);
                on_token(t);
            }
            if let Some(r) = choice["finish_reason"].as_str() {
                finish_reason = Some(r.to_owned());
            }
        }
        let usage = &data["usage"];
        if usage.is_object() {
            c.input_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0) as u32;
            c.output_tokens = usage["completion_tokens"].as_u64().unwrap_or(0) as u32;
            c.cache_read_tokens = usage["prompt_tokens_details"]["cached_tokens"]
                .as_u64()
                .unwrap_or(0) as u32;
            c.cost_usd = usage["cost"].as_f64();
        }
    }
    if !finished {
        return Err(CloudError::Transport("stream ended early".into()));
    }
    match finish_reason.as_deref() {
        Some("stop") => Ok(c),
        Some(other) => Err(CloudError::Stopped(other.to_owned())),
        None => Err(CloudError::Stopped("no finish reason".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::super::mock::{http, mock};
    use super::*;

    const STREAM: &str = ": OPENROUTER PROCESSING\n\ndata: {\"id\":\"gen-1\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"gen-1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hel\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"gen-1\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"lo\"},\"finish_reason\":\"stop\"}]}\n\ndata: {\"id\":\"gen-1\",\"choices\":[],\"usage\":{\"prompt_tokens\":120,\"completion_tokens\":7,\"prompt_tokens_details\":{\"cached_tokens\":100},\"cost\":0.00042}}\n\ndata: [DONE]\n\n";

    fn plain(job: JobKind) -> Request<'static> {
        Request {
            job,
            system: None,
            user: "x",
            history: &[],
            schema: None,
            max_output: 0,
        }
    }

    #[test]
    fn streams_text_usage_and_cost() {
        let (base, rx) = mock(vec![http(
            "200 OK",
            "content-type: text/event-stream\r\n",
            STREAM,
        )]);
        let b = OpenAiCompatBackend::new(
            "or",
            "anthropic/claude-sonnet-5",
            "sk-or-test",
            Some(&format!("{base}/")),
        );
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
        assert_eq!(c.cost_usd, Some(0.00042));
        let sent = rx.recv().unwrap();
        assert!(sent.starts_with("POST /chat/completions "), "{sent}");
        assert!(sent.contains("authorization: Bearer sk-or-test"));
        let body: Value = serde_json::from_str(sent.split_once("\n\n").unwrap().1.trim()).unwrap();
        assert_eq!(body["model"], "anthropic/claude-sonnet-5");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        assert_eq!(body["max_tokens"], 600);
        assert_eq!(body["response_format"]["type"], "json_schema");
        assert_eq!(body["response_format"]["json_schema"]["strict"], true);
        let messages = body["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0]["role"], "system");
        assert_eq!(messages[0]["content"], "sys");
        assert_eq!(messages[2]["role"], "assistant");
        assert_eq!(messages[3]["content"], "hi");
        // A loopback mock is not OpenRouter: no `usage.include`.
        assert!(body.get("usage").is_none());
    }

    #[test]
    fn openrouter_asks_for_its_cost_figure() {
        let b = OpenAiCompatBackend::new("or", "openai/gpt-5", "k", None);
        assert_eq!(b.base_url, DEFAULT_BASE_URL);
        let req = plain(JobKind::Chat);
        let body: Value = serde_json::from_str(&b.body(&req)).unwrap();
        assert_eq!(body["usage"]["include"], true);
        assert!(body.get("response_format").is_none());
        assert_eq!(body["max_tokens"], 4096);
        // The host decides, however it was typed.
        let b = OpenAiCompatBackend::new("or", "m", "k", Some("HTTPS://OpenRouter.ai/api/v1/"));
        assert!(b.is_openrouter());
        assert!(!b.is_openai());
    }

    #[test]
    fn openai_takes_the_newer_output_field() {
        let b = OpenAiCompatBackend::new("oa", "gpt-5", "k", Some("https://api.openai.com/v1"));
        assert!(b.is_openai());
        let req = plain(JobKind::Chat);
        let body: Value = serde_json::from_str(&b.body(&req)).unwrap();
        assert_eq!(body["max_completion_tokens"], 4096);
        assert!(body.get("max_tokens").is_none());
        assert!(body.get("usage").is_none());
    }

    #[test]
    fn retries_rate_limit_then_succeeds() {
        let (base, _rx) = mock(vec![
            http(
                "429 Too Many Requests",
                "retry-after: 0\r\ncontent-type: application/json\r\n",
                r#"{"error":{"message":"slow down","code":429}}"#,
            ),
            http("200 OK", "content-type: text/event-stream\r\n", STREAM),
        ]);
        let b = OpenAiCompatBackend::new("or", "m", "k", Some(&base));
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
            r#"{"error":{"message":"No auth credentials found","code":401}}"#,
        )]);
        let b = OpenAiCompatBackend::new("or", "m", "sk-or-secret", Some(&base));
        let err = b.probe().unwrap_err();
        assert_eq!(
            err,
            CloudError::Auth(401, "No auth credentials found".into())
        );
        assert!(!err.brief().contains("sk-or-secret"));
    }

    #[test]
    fn non_stop_finish_and_stream_errors_fail() {
        let cut = STREAM.replace("\"stop\"", "\"length\"");
        let err = parse_stream(&mut cut.as_bytes(), &mut |_| {}).unwrap_err();
        assert_eq!(err, CloudError::Stopped("length".into()));
        let mid = "data: {\"error\":{\"message\":\"Provider returned error\",\"code\":502}}\n\n";
        let err = parse_stream(&mut mid.as_bytes(), &mut |_| {}).unwrap_err();
        assert_eq!(err, CloudError::Stopped("Provider returned error".into()));
        let early = &STREAM[..STREAM.len() - 14];
        assert!(matches!(
            parse_stream(&mut early.as_bytes(), &mut |_| {}).unwrap_err(),
            CloudError::Transport(_)
        ));
    }
}
