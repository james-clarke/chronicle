//! Google Calendar collector (m26 chunk 2): primary calendar events as
//! `meeting` spans. OAuth 2.0 desktop flow — `chronicle gcal-login` writes
//! `<data dir>/google.toml` (mode 0600) and the poller trades the refresh
//! token for an access token every hour. Only the event id, title and
//! start/end are stored; the scope is events-read-only.

use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use chronicle_core::types::{ActivityEvent, ActivityKind, CaptureEvent};
use crossbeam_channel::Sender;
use jiff::{SignedDuration, Timestamp};
use serde::{Deserialize, Serialize};

use crate::{BoxError, FocusProvider};

const POLL: Duration = Duration::from_secs(300);
/// Polled window: yesterday through tomorrow, every tick.
const WINDOW: SignedDuration = SignedDuration::from_hours(24);
/// Refresh the access token this long before it expires.
const REFRESH_MARGIN: SignedDuration = SignedDuration::from_secs(60);
/// Narrowest scope that lists the primary calendar.
pub const SCOPE: &str = "https://www.googleapis.com/auth/calendar.events.readonly";

/// Google's endpoints, overridden by the tests to point at a local server.
#[derive(Debug, Clone)]
pub struct Endpoints {
    pub auth: String,
    pub token: String,
    pub api: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            auth: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token: "https://oauth2.googleapis.com/token".into(),
            api: "https://www.googleapis.com/calendar/v3".into(),
        }
    }
}

/// `<data dir>/google.toml`: the OAuth client and the long-lived refresh
/// token from `chronicle gcal-login`. Mode 0600, never logged.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Tokens {
    pub client_id: String,
    pub client_secret: String,
    pub refresh_token: String,
    /// Account the refresh token belongs to (the primary calendar's id).
    #[serde(default)]
    pub email: String,
}

pub fn token_path(data_dir: &Path) -> PathBuf {
    data_dir.join("google.toml")
}

impl Tokens {
    pub fn load(path: &Path) -> Result<Self, BoxError> {
        Ok(toml::from_str(&std::fs::read_to_string(path)?)?)
    }

    /// Atomic like `mcp.toml`, mode 0600: the file carries a client secret
    /// and a refresh token.
    pub fn save(&self, path: &Path) -> Result<(), BoxError> {
        let text = toml::to_string_pretty(self)?;
        let tmp = path.with_extension("toml.tmp");
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

/// The refresh-token grant returns one of these; only the login sees a
/// `refresh_token`.
pub struct Grant {
    pub access_token: String,
    pub refresh_token: Option<String>,
}

pub struct GcalProvider {
    tokens: Tokens,
    endpoints: Endpoints,
    /// Access token and when it stops being usable.
    access: Option<(String, Timestamp)>,
    /// Start of every live event of the last poll, so a later `cancelled`
    /// payload — which carries no times — can still be tombstoned.
    seen: HashMap<String, Timestamp>,
    /// Last error warned about; repeats stay quiet.
    last_err: Option<String>,
}

impl GcalProvider {
    pub fn new(tokens: Tokens) -> Self {
        Self {
            tokens,
            endpoints: Endpoints::default(),
            access: None,
            seen: HashMap::new(),
            last_err: None,
        }
    }

    fn access_token(&mut self) -> Result<String, BoxError> {
        let now = Timestamp::now();
        if let Some((token, expires)) = &self.access
            && *expires > now + REFRESH_MARGIN
        {
            return Ok(token.clone());
        }
        let (token, expires_in) = refresh_access(&self.endpoints, &self.tokens)?;
        self.access = Some((token.clone(), now + SignedDuration::from_secs(expires_in)));
        Ok(token)
    }

    fn poll(&mut self) -> Result<Vec<ActivityEvent>, BoxError> {
        let access = self.access_token()?;
        let now = Timestamp::now();
        let json = events_json(
            &self.endpoints,
            &access,
            &[
                ("timeMin", (now - WINDOW).to_string()),
                ("timeMax", (now + WINDOW).to_string()),
                ("singleEvents", "true".to_owned()),
                ("orderBy", "startTime".to_owned()),
                // Deletions come back as `status = cancelled` instead of
                // silently dropping out of the window.
                ("showDeleted", "true".to_owned()),
            ],
        )?;
        let (events, seen) = parse_events(&json, &self.seen, (now - WINDOW, now + WINDOW));
        self.seen = seen;
        Ok(events)
    }
}

impl FocusProvider for GcalProvider {
    fn run(mut self, tx: Sender<CaptureEvent>) -> Result<(), BoxError> {
        crate::poll_loop(&tx, POLL, move || match self.poll() {
            Ok(events) => {
                self.last_err = None;
                events
            }
            Err(e) => {
                let err = e.to_string();
                if self.last_err.as_deref() != Some(err.as_str()) {
                    tracing::warn!("google calendar poll: {err}");
                    self.last_err = Some(err);
                }
                Vec::new()
            }
        })
    }
}

fn agent() -> ureq::Agent {
    // Read the body on 4xx: Google puts the reason (revoked grant, bad
    // client) in it, and "HTTP 400" alone is not a fixable message.
    ureq::Agent::config_builder()
        .http_status_as_error(false)
        .build()
        .new_agent()
}

fn refresh_access(endpoints: &Endpoints, t: &Tokens) -> Result<(String, i64), BoxError> {
    let v = post_token(
        &endpoints.token,
        &[
            ("client_id", &t.client_id),
            ("client_secret", &t.client_secret),
            ("refresh_token", &t.refresh_token),
            ("grant_type", "refresh_token"),
        ],
    )?;
    let token = v
        .get("access_token")
        .and_then(|t| t.as_str())
        .ok_or("google token endpoint: no access_token")?;
    let expires_in = v.get("expires_in").and_then(|e| e.as_i64()).unwrap_or(3600);
    Ok((token.to_owned(), expires_in))
}

/// Authorization-code grant of the loopback flow (`chronicle gcal-login`).
pub fn exchange_code(
    endpoints: &Endpoints,
    client_id: &str,
    client_secret: &str,
    code: &str,
    redirect_uri: &str,
) -> Result<Grant, BoxError> {
    let v = post_token(
        &endpoints.token,
        &[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("grant_type", "authorization_code"),
        ],
    )?;
    Ok(Grant {
        access_token: v
            .get("access_token")
            .and_then(|t| t.as_str())
            .ok_or("google token endpoint: no access_token")?
            .to_owned(),
        refresh_token: v
            .get("refresh_token")
            .and_then(|t| t.as_str())
            .map(str::to_owned),
    })
}

fn post_token(url: &str, form: &[(&str, &str)]) -> Result<serde_json::Value, BoxError> {
    let mut resp = agent().post(url).send_form(form.iter().copied())?;
    let status = resp.status().as_u16();
    let body = resp.body_mut().read_to_string()?;
    let v: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| format!("google token endpoint: HTTP {status}: {e}"))?;
    if let Some(err) = v.get("error").and_then(|e| e.as_str()) {
        let desc = v
            .get("error_description")
            .and_then(|d| d.as_str())
            .unwrap_or("");
        return Err(format!("google token endpoint: {err} {desc}")
            .trim_end()
            .into());
    }
    Ok(v)
}

fn events_json(
    endpoints: &Endpoints,
    access: &str,
    params: &[(&str, String)],
) -> Result<String, BoxError> {
    let mut req = agent()
        .get(format!("{}/calendars/primary/events", endpoints.api))
        .header("Authorization", format!("Bearer {access}"));
    for (k, v) in params {
        req = req.query(*k, v);
    }
    let mut resp = req.call()?;
    let status = resp.status().as_u16();
    let body = resp.body_mut().read_to_string()?;
    if status != 200 {
        let head: String = body.trim().chars().take(200).collect();
        return Err(format!("calendar events.list: HTTP {status}: {head}").into());
    }
    Ok(body)
}

/// The primary calendar's id is the account's address, and events.list
/// returns it as the payload's `summary` — so the login learns the account
/// without asking for a profile scope.
pub fn account_email(endpoints: &Endpoints, access: &str) -> Result<String, BoxError> {
    let json = events_json(endpoints, access, &[("maxResults", "1".to_owned())])?;
    let v: serde_json::Value = serde_json::from_str(&json)?;
    Ok(v.get("summary")
        .and_then(|s| s.as_str())
        .unwrap_or_default()
        .to_owned())
}

/// Consent-screen URL for the loopback flow. `access_type=offline` with
/// `prompt=consent` so Google always hands back a refresh token, not only on
/// the very first authorization.
pub fn auth_url(endpoints: &Endpoints, client_id: &str, redirect_uri: &str, state: &str) -> String {
    let query = [
        ("client_id", client_id),
        ("redirect_uri", redirect_uri),
        ("response_type", "code"),
        ("scope", SCOPE),
        ("access_type", "offline"),
        ("prompt", "consent"),
        ("state", state),
    ]
    .iter()
    .map(|(k, v)| format!("{k}={}", percent_encode(v)))
    .collect::<Vec<_>>()
    .join("&");
    format!("{}?{query}", endpoints.auth)
}

/// One query parameter of the loopback redirect's request line
/// (`GET /?code=…&state=… HTTP/1.1`).
pub fn redirect_param(request_line: &str, key: &str) -> Option<String> {
    let query = request_line.split_whitespace().nth(1)?.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (k, v) = pair.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            // Over bytes, not chars: `&s[i + 1..i + 3]` would panic on a
            // `%` followed by a multi-byte character.
            b'%' if i + 2 < bytes.len()
                && bytes[i + 1].is_ascii_hexdigit()
                && bytes[i + 2].is_ascii_hexdigit() =>
            {
                if let Some(b) = std::str::from_utf8(&bytes[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok())
                {
                    out.push(b);
                }
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// events.list payload → meeting spans, plus the starts to remember for the
/// next poll. All-day events (a `date`, no `dateTime`) and events the user
/// declined are skipped; a `cancelled` event becomes a tombstone
/// (`end_ts = ts`, zero length) so the stored row stops covering time. So
/// does anything `known` from the last poll that this one no longer reports
/// live while its start is still inside `window` — declined since, or gone
/// from the calendar without a `cancelled` item.
fn parse_events(
    json: &str,
    known: &HashMap<String, Timestamp>,
    window: (Timestamp, Timestamp),
) -> (Vec<ActivityEvent>, HashMap<String, Timestamp>) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return (Vec::new(), known.clone());
    };
    let Some(items) = v.get("items").and_then(|i| i.as_array()) else {
        return (Vec::new(), known.clone());
    };
    let mut out = Vec::new();
    let mut seen = HashMap::new();
    let mut cancelled: HashSet<&str> = HashSet::new();
    for item in items {
        let Some(id) = item.get("id").and_then(|i| i.as_str()) else {
            continue;
        };
        if item.get("status").and_then(|s| s.as_str()) == Some("cancelled") {
            // A cancelled instance carries no times, so the start comes from
            // the last poll; unknown means nothing was ever stored to zero.
            let start = event_time(item.get("start"))
                .or_else(|| event_time(item.get("originalStartTime")))
                .or_else(|| known.get(id).copied());
            if let Some(ts) = start {
                cancelled.insert(id);
                out.push(meeting(ts, ts, id, None, Vec::new()));
            }
            continue;
        }
        let (Some(ts), Some(end)) = (event_time(item.get("start")), event_time(item.get("end")))
        else {
            continue;
        };
        if declined(item) {
            continue;
        }
        seen.insert(id.to_owned(), ts);
        out.push(meeting(
            ts,
            end,
            id,
            item.get("summary")
                .and_then(|s| s.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned),
            attendees(item),
        ));
    }
    // Sorted so the emitted order does not depend on the map's iteration.
    let mut gone: Vec<(&String, &Timestamp)> = known
        .iter()
        .filter(|(id, start)| {
            !seen.contains_key(*id)
                && !cancelled.contains(id.as_str())
                && **start >= window.0
                && **start <= window.1
        })
        .collect();
    gone.sort_unstable();
    out.extend(
        gone.into_iter()
            .map(|(id, start)| meeting(*start, *start, id, None, Vec::new())),
    );
    (out, seen)
}

fn meeting(
    ts: Timestamp,
    end: Timestamp,
    id: &str,
    summary: Option<String>,
    attendees: Vec<String>,
) -> ActivityEvent {
    ActivityEvent {
        ts,
        end_ts: Some(end),
        repo: String::new(),
        branch: String::new(),
        kind: ActivityKind::Meeting,
        ext_id: Some(id.to_owned()),
        summary,
        detail: (!attendees.is_empty())
            .then(|| serde_json::json!({ "attendees": attendees }).to_string()),
    }
}

/// The other people on the invite (display name, else email), in the
/// order the API lists them; the user and resource rooms are not people
/// the meeting is "with".
fn attendees(item: &serde_json::Value) -> Vec<String> {
    const MAX: usize = 12;
    item.get("attendees")
        .and_then(|a| a.as_array())
        .map(|a| {
            a.iter()
                .filter(|p| p.get("self").and_then(|s| s.as_bool()) != Some(true))
                .filter(|p| p.get("resource").and_then(|s| s.as_bool()) != Some(true))
                .filter_map(|p| {
                    p.get("displayName")
                        .and_then(|n| n.as_str())
                        .filter(|n| !n.trim().is_empty())
                        .or_else(|| p.get("email").and_then(|e| e.as_str()))
                        .map(|s| s.trim().to_owned())
                })
                .take(MAX)
                .collect()
        })
        .unwrap_or_default()
}

/// Timed events only: an all-day event has `date`, not `dateTime`.
fn event_time(v: Option<&serde_json::Value>) -> Option<Timestamp> {
    v?.get("dateTime")?.as_str()?.parse().ok()
}

/// `attendees[self].responseStatus == "declined"`: someone else's meeting
/// the user said no to is not time spent.
fn declined(item: &serde_json::Value) -> bool {
    item.get("attendees")
        .and_then(|a| a.as_array())
        .is_some_and(|a| {
            a.iter().any(|p| {
                p.get("self").and_then(|s| s.as_bool()) == Some(true)
                    && p.get("responseStatus").and_then(|r| r.as_str()) == Some("declined")
            })
        })
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::net::TcpListener;

    use super::*;

    const EVENTS: &str = r#"{
      "kind": "calendar#events",
      "summary": "sam@acme.com",
      "items": [
        {"id":"ev1","status":"confirmed","summary":"  Sprint planning  ",
         "start":{"dateTime":"2026-09-03T09:00:00+01:00"},
         "end":{"dateTime":"2026-09-03T09:30:00+01:00"}},
        {"id":"allday","status":"confirmed","summary":"Bank holiday",
         "start":{"date":"2026-09-03"},"end":{"date":"2026-09-04"}},
        {"id":"nope","status":"confirmed","summary":"Someone else's review",
         "start":{"dateTime":"2026-09-03T11:00:00+01:00"},
         "end":{"dateTime":"2026-09-03T12:00:00+01:00"},
         "attendees":[{"email":"other@x","responseStatus":"accepted"},
                      {"email":"j@x","self":true,"responseStatus":"declined"}]},
        {"id":"ev2","status":"confirmed",
         "start":{"dateTime":"2026-09-03T14:00:00+01:00"},
         "end":{"dateTime":"2026-09-03T14:15:00+01:00"},
         "attendees":[{"email":"j@x","self":true,"responseStatus":"accepted"}]},
        {"id":"gone","status":"cancelled",
         "originalStartTime":{"dateTime":"2026-09-03T16:00:00+01:00"}},
        {"id":"never-stored","status":"cancelled"}
      ]
    }"#;

    fn ts(s: &str) -> Timestamp {
        s.parse().unwrap()
    }

    /// Wide enough to hold every fixture event.
    fn window() -> (Timestamp, Timestamp) {
        (ts("2026-09-02T00:00:00Z"), ts("2026-09-04T00:00:00Z"))
    }

    #[test]
    fn maps_events_and_skips_all_day_and_declined() {
        let (got, seen) = parse_events(EVENTS, &HashMap::new(), window());
        let ids: Vec<&str> = got.iter().filter_map(|e| e.ext_id.as_deref()).collect();
        assert_eq!(ids, vec!["ev1", "ev2", "gone"], "{got:?}");
        assert_eq!(got[0].kind, ActivityKind::Meeting);
        assert_eq!(got[0].ts, ts("2026-09-03T08:00:00Z"));
        assert_eq!(got[0].end_ts, Some(ts("2026-09-03T08:30:00Z")));
        assert_eq!(got[0].summary.as_deref(), Some("Sprint planning"));
        assert_eq!(got[0].repo, "");
        assert_eq!(got[1].summary, None);
        // Cancelled: zero-length, so the upsert clears the stored end.
        assert_eq!(got[2].ts, ts("2026-09-03T15:00:00Z"));
        assert_eq!(got[2].end_ts, Some(ts("2026-09-03T15:00:00Z")));
        assert_eq!(seen.len(), 2, "only live events are remembered: {seen:?}");
        assert_eq!(seen["ev1"], ts("2026-09-03T08:00:00Z"));
        assert!(
            parse_events("not json", &HashMap::new(), window())
                .0
                .is_empty()
        );
        assert!(parse_events("{}", &HashMap::new(), window()).0.is_empty());
    }

    #[test]
    fn cancelled_without_times_tombstones_the_remembered_start() {
        let known = HashMap::from([("never-stored".to_owned(), ts("2026-09-03T12:00:00Z"))]);
        let (got, _) = parse_events(EVENTS, &known, window());
        let tomb = got
            .iter()
            .find(|e| e.ext_id.as_deref() == Some("never-stored"));
        let tomb = tomb.expect("the last poll's start makes it tombstonable");
        assert_eq!(tomb.ts, ts("2026-09-03T12:00:00Z"));
        assert_eq!(tomb.end_ts, Some(tomb.ts));
    }

    /// A meeting we already stored that this poll no longer reports live
    /// keeps covering time until something zeroes it.
    #[test]
    fn declined_or_vanished_meetings_are_tombstoned() {
        let known = HashMap::from([
            // Still live in the payload: untouched.
            ("ev1".to_owned(), ts("2026-09-03T08:00:00Z")),
            // Declined since the last poll: the payload has it, we skip it.
            ("nope".to_owned(), ts("2026-09-03T10:00:00Z")),
            // Deleted without a `cancelled` item: absent from the payload.
            ("vanished".to_owned(), ts("2026-09-03T13:00:00Z")),
            // Absent because it aged out of the window: real history.
            ("old".to_owned(), ts("2026-08-20T09:00:00Z")),
        ]);
        let (got, seen) = parse_events(EVENTS, &known, window());
        let tomb = |id: &str| {
            got.iter()
                .find(|e| e.ext_id.as_deref() == Some(id))
                .map(|e| (e.ts, e.end_ts))
        };
        assert_eq!(
            tomb("nope"),
            Some((ts("2026-09-03T10:00:00Z"), Some(ts("2026-09-03T10:00:00Z"))))
        );
        assert_eq!(
            tomb("vanished"),
            Some((ts("2026-09-03T13:00:00Z"), Some(ts("2026-09-03T13:00:00Z"))))
        );
        assert_eq!(tomb("old"), None, "outside the window is not a deletion");
        assert_eq!(
            tomb("ev1"),
            Some((ts("2026-09-03T08:00:00Z"), Some(ts("2026-09-03T08:30:00Z")))),
            "a live event keeps its span"
        );
        // Tombstoned ids are not remembered, so the next poll stops re-zeroing.
        assert_eq!(seen.len(), 2, "{seen:?}");
    }

    /// Serve one HTTP request from a local socket; returns the URL and a
    /// handle yielding the request text.
    fn serve_once(body: &'static str) -> (String, std::thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut req = Vec::new();
            let mut buf = [0u8; 1024];
            loop {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                req.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&req).to_ascii_lowercase();
                if let Some((head, rest)) = text.split_once("\r\n\r\n") {
                    let len = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:"))
                        .and_then(|v| v.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    if rest.len() >= len {
                        break;
                    }
                }
            }
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(resp.as_bytes()).unwrap();
            stream.flush().unwrap();
            String::from_utf8_lossy(&req).into_owned()
        });
        (format!("http://{addr}/token"), handle)
    }

    #[test]
    fn refresh_exchanges_the_refresh_token_for_an_access_token() {
        let (url, server) = serve_once(r#"{"access_token":"ya29.live","expires_in":3599}"#);
        let tokens = Tokens {
            client_id: "cid".into(),
            client_secret: "csecret".into(),
            refresh_token: "1//refresh".into(),
            email: String::new(),
        };
        let endpoints = Endpoints {
            token: url,
            ..Endpoints::default()
        };
        let (access, expires_in) = refresh_access(&endpoints, &tokens).unwrap();
        assert_eq!(access, "ya29.live");
        assert_eq!(expires_in, 3599);
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /token "), "{request}");
        assert!(request.contains("grant_type=refresh_token"), "{request}");
        assert!(
            request.contains("refresh_token=1%2F%2Frefresh"),
            "{request}"
        );
    }

    #[test]
    fn token_endpoint_errors_carry_googles_reason() {
        let (url, server) = serve_once(
            r#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#,
        );
        let endpoints = Endpoints {
            token: url,
            ..Endpoints::default()
        };
        let err = exchange_code(&endpoints, "cid", "csecret", "4/code", "http://127.0.0.1:1")
            .err()
            .expect("the error payload is surfaced, not the status code")
            .to_string();
        assert_eq!(
            err,
            "google token endpoint: invalid_grant Token has been expired or revoked."
        );
        server.join().unwrap();
    }

    #[test]
    fn auth_url_and_redirect_round_trip_the_code() {
        let url = auth_url(
            &Endpoints::default(),
            "cid.apps.googleusercontent.com",
            "http://127.0.0.1:41234",
            "s7",
        );
        assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
        assert!(
            url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A41234"),
            "{url}"
        );
        assert!(url.contains("access_type=offline"), "{url}");
        assert!(
            url.contains(
                "scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcalendar.events.readonly"
            ),
            "{url}"
        );
        let line = "GET /?state=s7&code=4%2F0AX4Xk-a%2Bb&scope=x HTTP/1.1";
        assert_eq!(
            redirect_param(line, "code").as_deref(),
            Some("4/0AX4Xk-a+b")
        );
        assert_eq!(redirect_param(line, "state").as_deref(), Some("s7"));
        assert_eq!(redirect_param(line, "error"), None);
        assert_eq!(redirect_param("GET /favicon.ico HTTP/1.1", "code"), None);
    }

    /// A `%` in front of a multi-byte character is not a hex escape; slicing
    /// the two bytes after it as `str` would panic mid-character.
    #[test]
    fn percent_decode_survives_a_stray_percent() {
        assert_eq!(percent_decode("%\u{20ac}"), "%\u{20ac}");
        assert_eq!(percent_decode("a%\u{20ac}b"), "a%\u{20ac}b");
        assert_eq!(percent_decode("%zz"), "%zz");
        assert_eq!(percent_decode("%2"), "%2");
        assert_eq!(percent_decode("%"), "%");
        assert_eq!(percent_decode("a+b%2Fc"), "a b/c");
    }
}
