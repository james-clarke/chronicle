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
            if let Some(url) = &span.url {
                let _ = write!(out, " <{url}>");
            }
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
    let digest = build_digest(
        &spans[batches[0].spans.clone()],
        &TimeZone::UTC,
        &[],
        &[],
        None,
    );
    assert!(approx_tokens(&digest) <= MAX_TOKENS);
    check_golden("day1.digest.golden", &digest);
}

// M6: browser URL heartbeats split browser focus time per site and surface a
// "Sites by time" digest section; window-title churn inside a URL-carrying
// span no longer splits it.
#[test]
fn day2_web_per_site_spans() {
    let config = Config::default();
    let events = load_fixture("day2_web.jsonl");
    let stream_end: Timestamp = "2026-08-27T17:50:00Z".parse().unwrap();
    let spans = sessionize(&events, stream_end, &config);
    check_golden("day2_web.spans.golden", &render_spans(&spans));

    let sites: Vec<&str> = spans
        .iter()
        .filter(|s| s.app == "firefox")
        .map(|s| chronicle_core::sessionizer::domain(s.url.as_deref().unwrap()))
        .collect();
    assert_eq!(
        sites,
        ["github.com", "docs.rs", "github.com"],
        "browser time must split per site"
    );

    let batches = assign_batches(&spans, &config);
    let digest = build_digest(
        &spans[batches[0].spans.clone()],
        &TimeZone::UTC,
        &[],
        &[],
        None,
    );
    assert!(approx_tokens(&digest) <= MAX_TOKENS);
    assert!(digest.contains("## Sites by time"), "digest: {digest}");
    check_golden("day2_web.digest.golden", &digest);
}

// M5 acceptance: a correction on an earlier, similar batch changes the next
// batch's digest (the deterministic half of "changes the output"); an
// unrelated correction does not surface.
#[test]
fn correction_changes_next_digest() {
    use chronicle_core::sessionizer::BatchDraft;
    use chronicle_core::storage;
    use chronicle_core::types::{NewTask, ms_to_ts, ts_to_ms};

    let db = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("m5_corrections.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(db.with_extension(format!("db{suffix}")));
    }
    let mut conn = storage::open(&db).unwrap();

    let (spans, config) = day1();
    let batches = assign_batches(&spans, &config);
    let current = &spans[batches[0].spans.clone()];

    // Prior batch = the same activity two hours earlier, plus an unrelated
    // one four hours earlier; each gets a task, each task a correction.
    let shift = |spans: &[SpanDraft], hours: i64| -> Vec<SpanDraft> {
        spans
            .iter()
            .map(|s| SpanDraft {
                start: ms_to_ts(ts_to_ms(s.start) - hours * 3_600_000),
                end: ms_to_ts(ts_to_ms(s.end) - hours * 3_600_000),
                app: s.app.clone(),
                title: s.title.clone(),
                kind: s.kind,
                url: s.url.clone(),
            })
            .collect()
    };
    let alien = vec![SpanDraft {
        start: ms_to_ts(ts_to_ms(current[0].start) - 4 * 3_600_000),
        end: ms_to_ts(ts_to_ms(current[0].end) - 4 * 3_600_000 + 600_000),
        app: "blender".into(),
        title: "Sculpting Donut Tutorial".into(),
        kind: SpanKind::Focus,
        url: None,
    }];
    for (prior, label, new_label, new_project) in [
        (
            shift(current, 2),
            "working in terminal",
            "hacking on chronicle capture",
            Some("chronicle"),
        ),
        (
            alien,
            "watching videos",
            "3d modeling practice",
            Some("blender-course"),
        ),
    ] {
        let batch = BatchDraft {
            start: prior.first().unwrap().start,
            end: prior.last().unwrap().end,
            spans: 0..prior.len(),
        };
        storage::replace_tail(
            &mut conn,
            ts_to_ms(batch.start),
            &prior,
            std::slice::from_ref(&batch),
        )
        .unwrap();
        let batch_id: i64 = conn
            .query_row("SELECT MAX(id) FROM batches", [], |r| r.get(0))
            .unwrap();
        storage::store_tasks(
            &mut conn,
            batch_id,
            &[NewTask {
                label: label.into(),
                project: None,
                start_ts: batch.start,
                end_ts: batch.end,
                confidence: 0.5,
            }],
        )
        .unwrap();
        let task_id: i64 = conn
            .query_row("SELECT id FROM tasks WHERE batch_id=?1", [batch_id], |r| {
                r.get(0)
            })
            .unwrap();
        storage::insert_correction(&mut conn, batch.end, task_id, new_label, new_project).unwrap();
        // The correction also applies to the task row itself.
        let stored: String = conn
            .query_row("SELECT label FROM tasks WHERE id=?1", [task_id], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(stored, new_label);
    }

    let corrections = storage::similar_corrections(&conn, current, 4).unwrap();
    assert_eq!(
        corrections.len(),
        1,
        "only the similar correction should match, got {corrections:?}"
    );
    assert_eq!(corrections[0].new_label, "hacking on chronicle capture");

    let plain = build_digest(current, &TimeZone::UTC, &[], &[], None);
    let with = build_digest(current, &TimeZone::UTC, &[], &corrections, None);
    assert_ne!(plain, with, "correction must change the digest");
    check_golden("day1.corrections.digest.golden", &with);
}

#[test]
fn digest_workspace_context_section() {
    let (spans, config) = day1();
    let batches = assign_batches(&spans, &config);
    let current = &spans[batches[0].spans.clone()];
    let plain = build_digest(current, &TimeZone::UTC, &[], &[], None);
    let ctx = "### jira.search\nCHR-42 fix AFK split";
    let with = build_digest(current, &TimeZone::UTC, &[], &[], Some(ctx));
    assert_eq!(with, format!("{plain}\n## Workspace context\n{ctx}\n"));
    // Blank context must not add the section (goldens stay MCP-free).
    assert_eq!(
        build_digest(current, &TimeZone::UTC, &[], &[], Some("  \n")),
        plain
    );
}
