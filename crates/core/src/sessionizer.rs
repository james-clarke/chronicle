//! Deterministic events → spans → batches. No LLM anywhere in here; golden
//! tests replay fixture JSONL through these pure functions.

use std::ops::Range;

use jiff::Timestamp;
use rusqlite::Connection;

use crate::config::Config;
use crate::storage::{self, StorageError};
use crate::types::Event;

/// Focus spans shorter than this collapse into `context-switching`.
pub const MIN_SPAN_SECS: i64 = 5;
/// An AFK gap at least this long force-closes the open batch.
pub const BATCH_BREAK_MINS: i64 = 30;
/// An AFK gap at least this long closes a batch that already holds
/// `batch_min_minutes` of activity (m27: windows end at natural breaks). Same
/// threshold as the interval split rule.
pub const AFK_SPLIT_MINS: i64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    Focus,
    ContextSwitching,
    Afk,
}

impl SpanKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SpanKind::Focus => "focus",
            SpanKind::ContextSwitching => "context-switching",
            SpanKind::Afk => "afk",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "focus" => Some(SpanKind::Focus),
            "context-switching" => Some(SpanKind::ContextSwitching),
            "afk" => Some(SpanKind::Afk),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SpanDraft {
    pub start: Timestamp,
    pub end: Timestamp,
    pub app: String,
    pub title: String,
    pub kind: SpanKind,
    /// Last page URL seen while this browser span was open (M6).
    pub url: Option<String>,
}

impl SpanDraft {
    pub fn duration_ms(&self) -> i64 {
        self.end.as_millisecond() - self.start.as_millisecond()
    }
}

/// Half-open range of span indices belonging to one batch. AFK gaps between
/// batches stay unbatched.
#[derive(Debug, Clone, PartialEq)]
pub struct BatchDraft {
    pub start: Timestamp,
    pub end: Timestamp,
    pub spans: Range<usize>,
}

pub fn sessionize(events: &[Event], stream_end: Timestamp, config: &Config) -> Vec<SpanDraft> {
    let mut spans: Vec<SpanDraft> = Vec::new();
    let mut open: Option<SpanDraft> = None;
    let mut afk_since: Option<Timestamp> = None;
    let mut last_focus: Option<(String, String)> = None;

    let close = |mut span: SpanDraft, at: Timestamp, spans: &mut Vec<SpanDraft>| {
        span.end = at;
        if span.duration_ms() > 0 {
            spans.push(span);
        }
    };

    for ev in events {
        match ev.kind.as_str() {
            "focus" | "title" => {
                last_focus = Some((ev.app.clone(), ev.title.clone()));
                // Titles change on their own (terminals, tab counters) and the
                // daemon snapshots focus on startup — none of that proves the
                // user is present. Only idle=false ends an AFK span.
                if afk_since.is_some() {
                    continue;
                }
                // A URL-carrying browser span merges any same-app title churn:
                // the heartbeat stream owns tab identity there, and window
                // titles ("page — Mozilla Firefox") won't match page titles.
                let same_activity = open.as_ref().is_some_and(|span| {
                    span.app == ev.app
                        && (span.url.is_some()
                            || strsim::normalized_levenshtein(&span.title, &ev.title)
                                >= config.title_similarity)
                });
                if !same_activity {
                    if let Some(span) = open.take() {
                        close(span, ev.ts, &mut spans);
                    }
                    open = Some(focus_span(ev.ts, ev.app.clone(), ev.title.clone()));
                }
            }
            // Browser heartbeats refine the open span only while a browser is
            // focused — extensions report the active tab even when the browser
            // window isn't (audible tabs), so URL events never open spans.
            "url" => {
                if afk_since.is_some() {
                    continue;
                }
                let Some(url) = ev.url.as_deref().filter(|u| !u.is_empty()) else {
                    continue;
                };
                if let Some(span) = open.as_mut()
                    && is_browser(&span.app, config)
                {
                    let same_site =
                        span.url.is_none() || span.url.as_deref().map(domain) == Some(domain(url));
                    if !same_site {
                        let app = span.app.clone();
                        close(open.take().expect("span checked above"), ev.ts, &mut spans);
                        open = Some(focus_span(ev.ts, app, String::new()));
                    }
                    let span = open.as_mut().expect("span open in both branches");
                    span.url = Some(url.to_owned());
                    // Page title beats the window title (no browser suffix).
                    if !ev.title.is_empty() {
                        span.title = ev.title.clone();
                    }
                }
            }
            "afk" => match ev.idle {
                Some(true) => {
                    // ts is backdated to when input stopped, so close there.
                    if let Some(span) = open.take() {
                        close(span, ev.ts, &mut spans);
                    }
                    afk_since.get_or_insert(ev.ts);
                }
                Some(false) => {
                    if let Some(afk_start) = afk_since.take() {
                        close(afk_span(afk_start), ev.ts, &mut spans);
                        // The user resumed in whatever was focused last.
                        if let Some((app, title)) = &last_focus {
                            open = Some(focus_span(ev.ts, app.clone(), title.clone()));
                        }
                    }
                }
                None => {}
            },
            _ => {}
        }
    }
    if let Some(span) = open.take() {
        close(span, stream_end, &mut spans);
    }
    if let Some(afk_start) = afk_since.take() {
        close(afk_span(afk_start), stream_end, &mut spans);
    }
    collapse_short(spans)
}

fn focus_span(start: Timestamp, app: String, title: String) -> SpanDraft {
    SpanDraft {
        start,
        end: start,
        app,
        title,
        kind: SpanKind::Focus,
        url: None,
    }
}

fn afk_span(start: Timestamp) -> SpanDraft {
    SpanDraft {
        start,
        end: start,
        app: String::new(),
        title: String::new(),
        kind: SpanKind::Afk,
        url: None,
    }
}

fn is_browser(app: &str, config: &Config) -> bool {
    let app = app.to_lowercase();
    config
        .browser_apps
        .iter()
        .any(|b| app.contains(&b.to_lowercase()))
}

/// Host part of a URL, `www.` stripped; good enough for site grouping.
pub fn domain(url: &str) -> &str {
    let rest = url.split_once("://").map_or(url, |(_, r)| r);
    let host = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    host.strip_prefix("www.").unwrap_or(host)
}

fn collapse_short(spans: Vec<SpanDraft>) -> Vec<SpanDraft> {
    let mut out: Vec<SpanDraft> = Vec::new();
    for span in spans {
        let short = span.kind == SpanKind::Focus && span.duration_ms() < MIN_SPAN_SECS * 1000;
        if !short {
            out.push(span);
            continue;
        }
        match out.last_mut() {
            Some(prev) if prev.kind == SpanKind::ContextSwitching && prev.end == span.start => {
                prev.end = span.end;
            }
            _ => out.push(SpanDraft {
                app: String::new(),
                title: String::new(),
                kind: SpanKind::ContextSwitching,
                url: None,
                ..span
            }),
        }
    }
    out
}

/// A batch closes once it accumulates `batch_minutes` of non-AFK time, at
/// the start of an AFK gap ≥ `BATCH_BREAK_MINS`, or at the start of an AFK
/// gap ≥ `AFK_SPLIT_MINS` once it holds `batch_min_minutes` of activity. The
/// tail stays unbatched.
pub fn assign_batches(spans: &[SpanDraft], config: &Config) -> Vec<BatchDraft> {
    let target_ms = i64::from(config.batch_minutes) * 60_000;
    let min_ms = i64::from(config.batch_min_minutes) * 60_000;
    let mut batches = Vec::new();
    let mut accum = 0i64;
    let mut start_idx: Option<usize> = None;
    for (i, span) in spans.iter().enumerate() {
        if span.kind == SpanKind::Afk {
            let afk_ms = span.duration_ms();
            let breaks = afk_ms >= BATCH_BREAK_MINS * 60_000
                || (afk_ms >= AFK_SPLIT_MINS * 60_000 && accum >= min_ms);
            if breaks && let Some(s) = start_idx.take() {
                batches.push(BatchDraft {
                    start: spans[s].start,
                    end: span.start,
                    spans: s..i,
                });
                accum = 0;
            }
            continue;
        }
        start_idx.get_or_insert(i);
        accum += span.duration_ms();
        if accum >= target_ms {
            let s = start_idx.take().expect("batch start set above");
            batches.push(BatchDraft {
                start: spans[s].start,
                end: span.end,
                spans: s..i + 1,
            });
            accum = 0;
        }
    }
    batches
}

/// Rebuild spans/batches for everything after the last closed batch. Closed
/// batches and their spans are immutable; only the unbatched tail is replaced.
pub fn refresh(conn: &mut Connection, config: &Config, now: Timestamp) -> Result<(), StorageError> {
    let t0 = storage::latest_batch_end(conn)?.unwrap_or(0);
    let events = storage::load_events_from(conn, t0)?;
    if events.is_empty() {
        return Ok(());
    }
    let spans = sessionize(&events, now, config);
    let batches = assign_batches(&spans, config);
    storage::replace_tail(conn, t0, &spans, &batches)
}
