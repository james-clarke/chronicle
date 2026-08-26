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
                let same_activity = open.as_ref().is_some_and(|span| {
                    span.app == ev.app
                        && strsim::normalized_levenshtein(&span.title, &ev.title)
                            >= config.title_similarity
                });
                if !same_activity {
                    if let Some(span) = open.take() {
                        close(span, ev.ts, &mut spans);
                    }
                    open = Some(focus_span(ev.ts, ev.app.clone(), ev.title.clone()));
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
    }
}

fn afk_span(start: Timestamp) -> SpanDraft {
    SpanDraft {
        start,
        end: start,
        app: String::new(),
        title: String::new(),
        kind: SpanKind::Afk,
    }
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
                ..span
            }),
        }
    }
    out
}

/// A batch closes once it accumulates `batch_minutes` of non-AFK time, or at
/// the start of an AFK gap ≥ `BATCH_BREAK_MINS`. The tail stays unbatched.
pub fn assign_batches(spans: &[SpanDraft], config: &Config) -> Vec<BatchDraft> {
    let target_ms = i64::from(config.batch_minutes) * 60_000;
    let mut batches = Vec::new();
    let mut accum = 0i64;
    let mut start_idx: Option<usize> = None;
    for (i, span) in spans.iter().enumerate() {
        if span.kind == SpanKind::Afk {
            if span.duration_ms() >= BATCH_BREAK_MINS * 60_000
                && let Some(s) = start_idx.take()
            {
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
