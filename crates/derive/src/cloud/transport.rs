//! The HTTP exchange the streaming backends share: one POST with retries
//! on 429/5xx and transport failures, the timeouts, and the error
//! classification. Headers are the caller's; nothing here logs one.

use std::io::{BufRead, BufReader};
use std::time::{Duration, Instant};

use serde_json::Value;

use super::CloudError;
use crate::text::Completion;

const MAX_TRIES: u32 = 3;
/// Retries stop once this much wall time has gone; the daemon reaps a
/// worker after `DERIVE_TIMEOUT` (300 s) and the job must fail before that.
const RETRY_BUDGET: Duration = Duration::from_secs(120);
pub(super) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

/// A backend's parser for a 200 stream: tokens out as they land.
pub(super) type ParseStream =
    fn(&mut dyn BufRead, &mut dyn FnMut(&str)) -> Result<Completion, CloudError>;

/// POST `body` to `url` with retries on 429/5xx and transport failures;
/// `parse` consumes a 200 stream. `what` names the API in the retry log.
pub(super) fn post_stream(
    what: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: &str,
    on_token: &mut dyn FnMut(&str),
    parse: ParseStream,
) -> Result<Completion, CloudError> {
    let started = Instant::now();
    let mut last = CloudError::Transport("no attempt made".into());
    for attempt in 0..MAX_TRIES {
        let t0 = Instant::now();
        match once(url, headers, body, on_token, parse) {
            Ok(mut c) => {
                c.wall_ms = t0.elapsed().as_millis() as u64;
                return Ok(c);
            }
            Err((e, retry_after)) => {
                if !e.is_transient() {
                    return Err(e);
                }
                tracing::warn!(attempt, "{} call failed: {}", what, e.brief());
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
    url: &str,
    headers: &[(&str, &str)],
    body: &str,
    on_token: &mut dyn FnMut(&str),
    parse: ParseStream,
) -> Result<Completion, (CloudError, Option<Duration>)> {
    let mut req = ureq::post(url)
        .config()
        .http_status_as_error(false)
        .timeout_connect(Some(CONNECT_TIMEOUT))
        .timeout_recv_response(Some(RESPONSE_TIMEOUT))
        .build()
        .header("content-type", "application/json")
        .header("accept", "text/event-stream");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let resp = req
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
    let mut reader = BufReader::new(resp.into_body().into_reader());
    parse(&mut reader, on_token).map_err(|e| (e, None))
}

/// A transport error's shape without any of its payload.
pub(super) fn transport_brief(e: &ureq::Error) -> String {
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
pub(super) fn api_message(text: &str) -> String {
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

pub(super) fn classify_status(status: u16, text: &str) -> CloudError {
    let m = api_message(text);
    match status {
        401 | 403 => CloudError::Auth(status, m),
        429 => CloudError::RateLimited(m),
        500..=599 => CloudError::Server(status, m),
        _ => CloudError::BadRequest(status, m),
    }
}
