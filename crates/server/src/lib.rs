//! AW-compatible localhost HTTP endpoint (M6): enough of the ActivityWatch
//! REST surface for stock `aw-watcher-web` to report browser tabs. Heartbeats
//! merge in memory per AW semantics; each page *change* becomes one
//! `CaptureEvent::Url` sent into the daemon's event channel (same filter +
//! storage path as capture events). Since m26 the same endpoint speaks the
//! WakaTime protocol (`/api/v1/users/current/heartbeats.bulk`), folding
//! editor heartbeats into `edit` activity events.
//!
//! Hardening: binds 127.0.0.1 only, strict `Host` allowlist (DNS-rebinding
//! defense — ActivityWatch shipped CVE-2022-31149 for exactly this), CORS
//! restricted to stock extension origins plus configured regexes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, Query, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use axum::{Router, extract::DefaultBodyLimit};
use chronicle_core::config::Config;
use chronicle_core::heartbeats::{Folder, Heartbeat};
use chronicle_core::types::{CaptureEvent, UrlEvent};
use crossbeam_channel::Sender;
use jiff::Timestamp;
use serde::Deserialize;
use serde_json::{Value, json};

/// Stock aw-watcher-web origin on the Chrome Web Store; Firefox installs get
/// a random per-install UUID, so those match by scheme (as aw-server does).
const CHROME_EXT_ORIGIN: &str = "chrome-extension://nglaklhklhcoonedhgnpgddginnjdadi";
const MOZ_EXT_RE: &str = "^moz-extension://.*$";

struct AppState {
    hosts: [String; 4],
    cors: Vec<regex::Regex>,
    buckets: Mutex<HashMap<String, Bucket>>,
    tx: Sender<CaptureEvent>,
    /// Key WakaTime plugins authenticate with (meta `wakapi_api_key`).
    /// None = `editor_heartbeats` is off in config: the routes exist but
    /// refuse everything.
    api_key: Option<String>,
    /// Open edit span per (project, branch) across heartbeats.
    edits: Mutex<Folder>,
}

struct Bucket {
    meta: Value,
    last: Option<LastEvent>,
}

struct LastEvent {
    data: Value,
    start_ms: i64,
    end_ms: i64,
}

fn app_state(
    config: &Config,
    tx: Sender<CaptureEvent>,
    api_key: Option<String>,
) -> anyhow::Result<Arc<AppState>> {
    let mut cors = vec![regex::Regex::new(MOZ_EXT_RE).expect("static regex")];
    for pat in &config.cors_allow {
        cors.push(
            regex::Regex::new(&format!("^(?:{pat})$"))
                .map_err(|e| anyhow::anyhow!("bad cors_allow regex {pat:?}: {e}"))?,
        );
    }
    let port = config.port;
    Ok(Arc::new(AppState {
        hosts: [
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            "127.0.0.1".into(),
            "localhost".into(),
        ],
        cors,
        buckets: Mutex::new(HashMap::new()),
        tx,
        api_key,
        edits: Mutex::new(Folder::default()),
    }))
}

/// Bind and serve on a background thread. A bind failure (port taken)
/// surfaces here so the daemon can log it and warn in the UI.
pub fn spawn(
    config: &Config,
    tx: Sender<CaptureEvent>,
    api_key: Option<String>,
) -> anyhow::Result<()> {
    let state = app_state(config, tx, api_key)?;
    let port = config.port;
    let listener = std::net::TcpListener::bind(("127.0.0.1", port))?;
    listener.set_nonblocking(true)?;
    std::thread::Builder::new()
        .name("aw-server".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("aw endpoint runtime failed: {e}");
                    return;
                }
            };
            rt.block_on(async {
                let listener = match tokio::net::TcpListener::from_std(listener) {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::error!("aw endpoint listener failed: {e}");
                        return;
                    }
                };
                if let Err(e) = axum::serve(listener, router(state)).await {
                    tracing::error!("aw endpoint exited: {e}");
                }
            });
        })?;
    tracing::info!("aw endpoint listening on 127.0.0.1:{port}");
    Ok(())
}

fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/api/0/info", get(info))
        .route("/api/0/buckets", get(list_buckets))
        .route("/api/0/buckets/{id}", get(get_bucket).post(create_bucket))
        .route("/api/0/buckets/{id}/events", get(get_events))
        .route(
            "/api/0/buckets/{id}/heartbeat",
            axum::routing::post(heartbeat),
        )
        // WakaTime protocol (m26): the bulk route every editor plugin posts
        // to, plus wakapi's single-heartbeat route (both take one object or
        // an array).
        .route(
            "/api/v1/users/current/heartbeats.bulk",
            axum::routing::post(wakatime_heartbeats),
        )
        .route("/api/heartbeat", axum::routing::post(wakatime_heartbeats))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .with_state(state)
}

/// Host allowlist + CORS, one gate for every route (including 404s).
async fn guard(State(st): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    // Owned copies: holding &req across next.run(req).await would make the
    // future !Send (axum's Body is !Sync).
    let (host, origin) = {
        let h = req.headers();
        let get =
            |name: header::HeaderName| h.get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
        (get(header::HOST).unwrap_or_default(), get(header::ORIGIN))
    };
    if !st.hosts.contains(&host) {
        return (StatusCode::FORBIDDEN, "bad Host").into_response();
    }
    let allowed = origin
        .as_deref()
        .is_some_and(|o| o == CHROME_EXT_ORIGIN || st.cors.iter().any(|r| r.is_match(o)));
    if req.method() == Method::OPTIONS {
        if !allowed {
            return StatusCode::FORBIDDEN.into_response();
        }
        return cors_headers(
            (
                StatusCode::NO_CONTENT,
                [
                    (header::ACCESS_CONTROL_ALLOW_METHODS, "GET, POST, OPTIONS"),
                    (header::ACCESS_CONTROL_ALLOW_HEADERS, "Content-Type"),
                ],
            )
                .into_response(),
            &origin,
        );
    }
    let res = next.run(req).await;
    if allowed {
        cors_headers(res, &origin)
    } else {
        res
    }
}

fn cors_headers(mut res: Response, origin: &Option<String>) -> Response {
    if let Some(origin) = origin
        && let Ok(v) = HeaderValue::from_str(origin)
    {
        res.headers_mut()
            .insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
        res.headers_mut()
            .insert(header::VARY, HeaderValue::from_static("Origin"));
    }
    res
}

async fn info() -> Json<Value> {
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_owned())
        .unwrap_or_else(|_| "localhost".into());
    Json(json!({
        "hostname": hostname,
        "version": concat!("chronicle ", env!("CARGO_PKG_VERSION")),
        "testing": false,
        "device_id": "chronicle",
    }))
}

async fn list_buckets(State(st): State<Arc<AppState>>) -> Json<Value> {
    let buckets = st.buckets.lock().expect("bucket lock");
    Json(Value::Object(
        buckets
            .iter()
            .map(|(k, b)| (k.clone(), b.meta.clone()))
            .collect(),
    ))
}

async fn get_bucket(State(st): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let buckets = st.buckets.lock().expect("bucket lock");
    match buckets.get(&id) {
        Some(b) => Json(b.meta.clone()).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// AW contract: creating an existing bucket answers 304 Not Modified.
async fn create_bucket(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
    body: Option<Json<Value>>,
) -> Response {
    let mut buckets = st.buckets.lock().expect("bucket lock");
    if buckets.contains_key(&id) {
        return StatusCode::NOT_MODIFIED.into_response();
    }
    let body = body.map(|Json(v)| v).unwrap_or_else(|| json!({}));
    let meta = bucket_meta(&id, &body);
    buckets.insert(
        id,
        Bucket {
            meta: meta.clone(),
            last: None,
        },
    );
    Json(meta).into_response()
}

fn bucket_meta(id: &str, body: &Value) -> Value {
    json!({
        "id": id,
        "created": Timestamp::now().to_string(),
        "name": null,
        "type": body.get("type").cloned().unwrap_or(Value::Null),
        "client": body.get("client").cloned().unwrap_or(Value::Null),
        "hostname": body.get("hostname").cloned().unwrap_or(Value::Null),
        "data": {},
    })
}

async fn get_events(State(st): State<Arc<AppState>>, Path(id): Path<String>) -> Response {
    let buckets = st.buckets.lock().expect("bucket lock");
    match buckets.get(&id) {
        Some(b) => Json(
            b.last
                .as_ref()
                .map(event_json)
                .map_or_else(Vec::new, |e| vec![e]),
        )
        .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(Deserialize)]
struct HeartbeatQuery {
    #[serde(default)]
    pulsetime: f64,
}

#[derive(Deserialize)]
struct HeartbeatBody {
    timestamp: String,
    #[serde(default)]
    duration: f64,
    data: Value,
}

async fn heartbeat(
    State(st): State<Arc<AppState>>,
    Path(id): Path<String>,
    Query(q): Query<HeartbeatQuery>,
    Json(body): Json<HeartbeatBody>,
) -> Response {
    let Ok(ts) = body.timestamp.parse::<Timestamp>() else {
        return (StatusCode::BAD_REQUEST, "bad timestamp").into_response();
    };
    let start_ms = ts.as_millisecond();
    let end_ms = start_ms + (body.duration * 1000.0) as i64;
    let mut buckets = st.buckets.lock().expect("bucket lock");
    // Buckets are in-memory: after a daemon restart extensions keep
    // heartbeating without re-creating, so materialize on the fly.
    let bucket = buckets.entry(id.clone()).or_insert_with(|| Bucket {
        meta: bucket_meta(&id, &json!({})),
        last: None,
    });
    let merged = match &mut bucket.last {
        // Identical data within pulsetime of the previous event's end:
        // extend that event instead of creating a new one.
        Some(last)
            if last.data == body.data
                && start_ms <= last.end_ms + (q.pulsetime * 1000.0) as i64 =>
        {
            last.end_ms = last.end_ms.max(end_ms).max(start_ms);
            true
        }
        _ => false,
    };
    if !merged {
        bucket.last = Some(LastEvent {
            data: body.data,
            start_ms,
            end_ms: end_ms.max(start_ms),
        });
        let last = bucket.last.as_ref().expect("just set");
        if let Some(url) = last
            .data
            .get("url")
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty())
        {
            let title = last.data.get("title").and_then(Value::as_str).unwrap_or("");
            let event = CaptureEvent::Url(UrlEvent {
                ts,
                app: format!("browser:{}", browser_name(&id)),
                title: title.to_owned(),
                url: url.to_owned(),
            });
            if st.tx.send(event).is_err() {
                tracing::error!("daemon event channel closed; dropping url event");
            }
        }
    }
    Json(event_json(bucket.last.as_ref().expect("set above"))).into_response()
}

/// WakaTime protocol (m26): editor plugins post heartbeats here (bulk or
/// single) with `Authorization: Basic base64(api_key)`. Each one folds into
/// its `(project, branch)` span and is re-emitted with the same `ext_id`, so
/// the stored `edit` row's `end_ts` grows while the file stays open.
async fn wakatime_heartbeats(
    State(st): State<Arc<AppState>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let Some(key) = st.api_key.as_deref() else {
        return (StatusCode::FORBIDDEN, "editor heartbeats are off in config").into_response();
    };
    if !authorized(&headers, key) {
        return (StatusCode::UNAUTHORIZED, "bad api key").into_response();
    }
    let Some(heartbeats) = parse_heartbeats(&body) else {
        return (StatusCode::BAD_REQUEST, "bad heartbeat body").into_response();
    };
    let now_ms = Timestamp::now().as_millisecond();
    let mut responses = Vec::with_capacity(heartbeats.len());
    // A panic inside the fold must not wedge the endpoint for the rest of
    // the daemon's life: the folder is plain in-memory state.
    let mut folder = st
        .edits
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for hb in &heartbeats {
        if let Some(event) = folder.fold(hb, now_ms)
            && st.tx.send(CaptureEvent::Activity(event)).is_err()
        {
            tracing::error!("daemon event channel closed; dropping edit event");
        }
        responses.push(json!([{"data": {"entity": hb.entity, "time": hb.time}}, 201]));
    }
    drop(folder);
    (StatusCode::CREATED, Json(json!({ "responses": responses }))).into_response()
}

/// Both routes take one heartbeat or an array of them (wakapi's contract).
fn parse_heartbeats(body: &[u8]) -> Option<Vec<Heartbeat>> {
    serde_json::from_slice::<Vec<Heartbeat>>(body)
        .or_else(|_| serde_json::from_slice::<Heartbeat>(body).map(|h| vec![h]))
        .ok()
}

/// WakaTime's own scheme: `base64(api_key)` with no password. Some clients
/// encode the separator anyway, so `api_key:` passes too.
fn authorized(headers: &HeaderMap, key: &str) -> bool {
    let Some(value) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    else {
        return false;
    };
    let Some(encoded) = value
        .strip_prefix("Basic ")
        .or_else(|| value.strip_prefix("basic "))
    else {
        return false;
    };
    let Some(decoded) = base64_decode(encoded.trim()) else {
        return false;
    };
    let decoded = decoded.strip_suffix(':').unwrap_or(&decoded);
    !key.is_empty() && ct_eq(decoded.as_bytes(), key.as_bytes())
}

/// Length check then an XOR fold over every byte: a byte-at-a-time compare
/// leaks the key's prefix through response timing.
fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Enough base64 to read one Basic credential (standard or url-safe
/// alphabet); not worth a dependency.
fn base64_decode(s: &str) -> Option<String> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut acc: u32 = 0;
    let mut bits = 0u32;
    let mut out = Vec::new();
    for c in s.bytes().filter(|c| *c != b'=') {
        let c = match c {
            b'-' => b'+',
            b'_' => b'/',
            c => c,
        };
        let v = ALPHABET.iter().position(|a| *a == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    String::from_utf8(out).ok()
}

fn event_json(last: &LastEvent) -> Value {
    json!({
        "timestamp": chronicle_core::types::ms_to_ts(last.start_ms).to_string(),
        "duration": (last.end_ms - last.start_ms) as f64 / 1000.0,
        "data": last.data,
    })
}

/// `aw-watcher-web-firefox` → `firefox`.
fn browser_name(bucket_id: &str) -> &str {
    bucket_id
        .strip_prefix("aw-watcher-web-")
        .or_else(|| bucket_id.rsplit('-').next())
        .unwrap_or(bucket_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use crossbeam_channel::Receiver;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    /// `base64("test-api-key")` = `dGVzdC1hcGkta2V5`.
    const TEST_KEY: &str = "test-api-key";

    fn test_router() -> (Router, Receiver<CaptureEvent>) {
        router_with_key(Some(TEST_KEY.to_owned()))
    }

    /// None = `editor_heartbeats` off, the state the daemon builds when the
    /// config switch is off.
    fn router_with_key(key: Option<String>) -> (Router, Receiver<CaptureEvent>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let state = app_state(&Config::default(), tx, key).unwrap();
        (router(state), rx)
    }

    fn req(method: &str, uri: &str, host: &str, body: Option<Value>) -> Request<Body> {
        let mut b = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::HOST, host);
        if body.is_some() {
            b = b.header(header::CONTENT_TYPE, "application/json");
        }
        b.body(body.map_or_else(Body::empty, |v| Body::from(v.to_string())))
            .unwrap()
    }

    async fn body_json(res: Response) -> Value {
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn host_check_rejects_rebinding() {
        let (app, _rx) = test_router();
        let res = app
            .clone()
            .oneshot(req("GET", "/api/0/info", "evil.example:5600", None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let res = app
            .oneshot(req("GET", "/api/0/info", "127.0.0.1:5600", None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn bucket_create_is_304_when_exists() {
        let (app, _rx) = test_router();
        let body = json!({"client": "aw-watcher-web", "type": "web.tab.current", "hostname": "x"});
        let uri = "/api/0/buckets/aw-watcher-web-firefox";
        let res = app
            .clone()
            .oneshot(req("POST", uri, "localhost:5600", Some(body.clone())))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let res = app
            .clone()
            .oneshot(req("POST", uri, "localhost:5600", Some(body)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_MODIFIED);
        let res = app
            .oneshot(req("GET", uri, "localhost:5600", None))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(body_json(res).await["type"], "web.tab.current");
    }

    #[tokio::test]
    async fn heartbeat_merges_within_pulsetime_and_splits_on_new_data() {
        let (app, rx) = test_router();
        let uri = "/api/0/buckets/aw-watcher-web-firefox/heartbeat?pulsetime=60";
        let hb = |ts: &str, url: &str| {
            json!({"timestamp": ts, "duration": 0.0,
                   "data": {"url": url, "title": "t", "audible": false, "incognito": false}})
        };
        let res = app
            .clone()
            .oneshot(req(
                "POST",
                uri,
                "127.0.0.1:5600",
                Some(hb("2026-08-27T10:00:00Z", "https://docs.rs/axum")),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let ev = rx.try_recv().expect("first heartbeat stores a url event");
        let CaptureEvent::Url(ev) = ev else {
            panic!("expected url event, got {ev:?}")
        };
        assert_eq!(ev.app, "browser:firefox");
        assert_eq!(ev.url, "https://docs.rs/axum");

        // Same data 30 s later: merged, no second stored event.
        let res = app
            .clone()
            .oneshot(req(
                "POST",
                uri,
                "127.0.0.1:5600",
                Some(hb("2026-08-27T10:00:30Z", "https://docs.rs/axum")),
            ))
            .await
            .unwrap();
        assert_eq!(body_json(res).await["duration"], 30.0);
        assert!(rx.try_recv().is_err(), "merged heartbeat must not store");

        // New page: new event.
        app.oneshot(req(
            "POST",
            uri,
            "127.0.0.1:5600",
            Some(hb("2026-08-27T10:01:00Z", "https://github.com/a/b")),
        ))
        .await
        .unwrap();
        let CaptureEvent::Url(ev) = rx.try_recv().expect("page change stores") else {
            panic!("expected url event")
        };
        assert_eq!(ev.url, "https://github.com/a/b");
    }

    fn waka_req(uri: &str, auth: &str, body: Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(uri)
            .header(header::HOST, "127.0.0.1:5600")
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::AUTHORIZATION, auth)
            .body(Body::from(body.to_string()))
            .unwrap()
    }

    fn waka_hb(time: f64, entity: &str) -> Value {
        json!({"entity": entity, "type": "file", "time": time, "project": "contoso",
               "branch": "main", "language": "Python", "is_write": true})
    }

    /// Whole epoch seconds near our own clock: the folder ignores a `time`
    /// more than 30 days out and substitutes now.
    fn now_secs() -> f64 {
        (Timestamp::now().as_millisecond() / 1000) as f64
    }

    fn activity_ext_id(ev: CaptureEvent) -> (String, i64) {
        let CaptureEvent::Activity(ev) = ev else {
            panic!("expected activity event, got {ev:?}")
        };
        assert_eq!(ev.kind, chronicle_core::types::ActivityKind::Edit);
        assert_eq!(ev.repo, "contoso");
        (
            ev.ext_id.expect("edit spans carry an ext_id"),
            ev.end_ts
                .expect("edit spans carry an end_ts")
                .as_millisecond(),
        )
    }

    #[tokio::test]
    async fn heartbeats_bulk_folds_into_one_span_until_the_gap() {
        let (app, rx) = test_router();
        let base = now_secs();
        let base_ms = base as i64 * 1000;
        let bulk = json!([
            waka_hb(base, "/dev/contoso/a.py"),
            waka_hb(base + 60.0, "/dev/contoso/b.py")
        ]);
        let res = app
            .clone()
            .oneshot(waka_req(
                "/api/v1/users/current/heartbeats.bulk",
                "Basic dGVzdC1hcGkta2V5",
                bulk,
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        assert_eq!(
            body_json(res).await["responses"].as_array().unwrap().len(),
            2
        );
        let (first, first_end) = activity_ext_id(rx.try_recv().expect("first heartbeat stores"));
        let (second, second_end) = activity_ext_id(rx.try_recv().expect("second heartbeat stores"));
        assert_eq!(
            first,
            format!("contoso@main#{base_ms}"),
            "span start is the first heartbeat"
        );
        assert_eq!(second, first, "same span refreshes end_ts via Upsert");
        assert_eq!((first_end, second_end), (base_ms, base_ms + 60_000));

        // Single-heartbeat route, past the gap, key with the empty password
        // separator some clients encode: a new span.
        let res = app
            .oneshot(waka_req(
                "/api/heartbeat",
                "Basic dGVzdC1hcGkta2V5Og==",
                waka_hb(
                    base + 60.0 + chronicle_core::heartbeats::GAP_SECS as f64 + 1.0,
                    "/dev/contoso/a.py",
                ),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CREATED);
        let (third, _) = activity_ext_id(rx.try_recv().expect("new span stores"));
        assert_ne!(third, first);
    }

    #[tokio::test]
    async fn heartbeats_reject_a_bad_api_key() {
        let (app, rx) = test_router();
        let uri = "/api/v1/users/current/heartbeats.bulk";
        let body = json!([waka_hb(1000.0, "/dev/contoso/a.py")]);
        // base64("wrong-key")
        let res = app
            .clone()
            .oneshot(waka_req(uri, "Basic d3Jvbmcta2V5", body.clone()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let res = app
            .clone()
            .oneshot(waka_req(uri, "", body.clone()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        assert!(
            rx.try_recv().is_err(),
            "a rejected heartbeat stores nothing"
        );
    }

    #[tokio::test]
    async fn heartbeats_are_forbidden_when_the_switch_is_off() {
        let (app, rx) = router_with_key(None);
        let res = app
            .oneshot(waka_req(
                "/api/v1/users/current/heartbeats.bulk",
                "Basic dGVzdC1hcGkta2V5",
                json!([waka_hb(1000.0, "/dev/contoso/a.py")]),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&bytes[..], b"editor heartbeats are off in config");
        assert!(rx.try_recv().is_err(), "nothing folds with the switch off");
    }

    #[tokio::test]
    async fn cors_allows_stock_extensions_only() {
        let (app, _rx) = test_router();
        let mut preflight = req(
            "OPTIONS",
            "/api/0/buckets/x/heartbeat",
            "127.0.0.1:5600",
            None,
        );
        preflight.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_static("moz-extension://abc-123"),
        );
        let res = app.clone().oneshot(preflight).await.unwrap();
        assert_eq!(res.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            res.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN],
            "moz-extension://abc-123"
        );

        let mut evil = req(
            "OPTIONS",
            "/api/0/buckets/x/heartbeat",
            "127.0.0.1:5600",
            None,
        );
        evil.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        let res = app.clone().oneshot(evil).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);

        // Non-preflight from a disallowed origin: handled, but no ACAO header
        // (the browser blocks the read).
        let mut get = req("GET", "/api/0/info", "127.0.0.1:5600", None);
        get.headers_mut().insert(
            header::ORIGIN,
            HeaderValue::from_static("https://evil.example"),
        );
        let res = app.oneshot(get).await.unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(
            !res.headers()
                .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        );
    }
}
