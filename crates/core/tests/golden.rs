//! Fixture-driven golden tests. Bless with: UPDATE_GOLDEN=1 cargo test -p chronicle-core

use std::fmt::Write;
use std::path::PathBuf;

use chronicle_core::config::Config;
use chronicle_core::digest::{MAX_TOKENS, approx_tokens, build_digest};
use chronicle_core::sessionizer::{SpanDraft, SpanKind, assign_batches, sessionize};
use chronicle_core::types::Event;
use jiff::Timestamp;
use jiff::tz::TimeZone;

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures")
}

fn load_fixture(name: &str) -> Vec<Event> {
    let raw = std::fs::read_to_string(fixtures_dir().join(name)).unwrap();
    raw.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn check_golden(name: &str, actual: &str) {
    let path = fixtures_dir().join(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(
        expected, actual,
        "golden mismatch for {name}; bless with UPDATE_GOLDEN=1 cargo test -p chronicle-core"
    );
}

// Goldens render in UTC so results don't depend on the machine's timezone.
fn render_spans(spans: &[SpanDraft]) -> String {
    let mut out = String::new();
    for span in spans {
        let start = span.start.to_zoned(TimeZone::UTC);
        let end = span.end.to_zoned(TimeZone::UTC);
        let _ = write!(
            out,
            "{}\u{2013}{} [{}]",
            start.strftime("%H:%M:%S"),
            end.strftime("%H:%M:%S"),
            span.kind.as_str()
        );
        if span.kind == SpanKind::Focus {
            let _ = write!(out, " {}: {}", span.app, span.title);
        }
        out.push('\n');
    }
    out
}

fn day1() -> (Vec<SpanDraft>, Config) {
    let config = Config::default();
    let events = load_fixture("day1.jsonl");
    let stream_end: Timestamp = "2026-08-26T17:50:00Z".parse().unwrap();
    (sessionize(&events, stream_end, &config), config)
}

#[test]
fn day1_spans_golden() {
    let (spans, _) = day1();
    check_golden("day1.spans.golden", &render_spans(&spans));
}

#[test]
fn day1_batches() {
    let (spans, config) = day1();
    let batches = assign_batches(&spans, &config);
    assert_eq!(batches.len(), 1, "fixture should close exactly one batch");
    let batch = &batches[0];
    assert_eq!(batch.start.to_string(), "2026-08-26T17:00:00Z");
    assert_eq!(batch.end.to_string(), "2026-08-26T17:40:00Z");
    // The tail span after the closed batch stays unbatched.
    assert_eq!(batch.spans.end, spans.len() - 1);
}

#[test]
fn day1_digest_golden() {
    let (spans, config) = day1();
    let batches = assign_batches(&spans, &config);
    let digest = build_digest(&spans[batches[0].spans.clone()], &TimeZone::UTC, &[]);
    assert!(approx_tokens(&digest) <= MAX_TOKENS);
    check_golden("day1.digest.golden", &digest);
}
