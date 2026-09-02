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
        &[],
        None,
    );
    assert!(approx_tokens(&digest) <= MAX_TOKENS);
    assert!(digest.contains("## Sites by time"), "digest: {digest}");
    check_golden("day2_web.digest.golden", &digest);
}

// Derive v3 eval corpus: two real captured windows (live batches 8 and 9,
// 2026-08-27 morning) whose v2 derivations produced the failures the
// task-identity layer must fix — duplicate labels, cross-tab token bleed,
// identity fragmentation. Digest mirrors the bench fixture path exactly:
// whole stream, stream end = last event, no batch assignment.
fn eval_digest(fixture: &str) -> String {
    let config = Config::default();
    let events = load_fixture(&format!("{fixture}.jsonl"));
    let stream_end = events.last().expect("fixture has events").ts;
    let spans = sessionize(&events, stream_end, &config);
    check_golden(&format!("{fixture}.spans.golden"), &render_spans(&spans));
    build_digest(&spans, &TimeZone::UTC, &[], &[], &[], None)
}

#[test]
fn day3_sms_goldens() {
    let digest = eval_digest("day3_sms");
    assert!(approx_tokens(&digest) <= MAX_TOKENS);
    check_golden("day3_sms.digest.golden", &digest);
}

#[test]
fn day4_heroku_goldens() {
    let digest = eval_digest("day4_heroku");
    assert!(approx_tokens(&digest) <= MAX_TOKENS);
    check_golden("day4_heroku.digest.golden", &digest);
}

// M5 acceptance: a correction on an earlier, similar batch changes the next
// batch's digest (the deterministic half of "changes the output"); an
// unrelated correction does not surface.
#[test]
fn correction_changes_next_digest() {
    use chronicle_core::sessionizer::BatchDraft;
    use chronicle_core::storage;
    use chronicle_core::types::{NewInterval, TaskSlot, ms_to_ts, ts_to_ms};

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
        storage::store_derivation(
            &mut conn,
            batch_id,
            &[TaskSlot::New {
                label: label.into(),
                project: None,
            }],
            &[NewInterval {
                slot: 0,
                start_ts: batch.start,
                end_ts: batch.end,
                confidence: 0.5,
            }],
        )
        .unwrap();
        let task_id: i64 = conn
            .query_row(
                "SELECT task_id FROM intervals WHERE batch_id=?1",
                [batch_id],
                |r| r.get(0),
            )
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

    let plain = build_digest(current, &TimeZone::UTC, &[], &[], &[], None);
    let with = build_digest(current, &TimeZone::UTC, &[], &corrections, &[], None);
    assert_ne!(plain, with, "correction must change the digest");
    check_golden("day1.corrections.digest.golden", &with);
}

#[test]
fn retention_prune() {
    use chronicle_core::storage;
    use chronicle_core::types::ms_to_ts;

    let db = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("prune.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(db.with_extension(format!("db{suffix}")));
    }
    let mut conn = storage::open(&db).unwrap();

    let cutoff: i64 = 1_000_000_000_000;
    let old = cutoff - 3_600_000;
    let fresh = cutoff + 3_600_000;

    // Three old events with batch size 2 exercises the delete loop.
    for i in 0..3 {
        conn.execute(
            "INSERT INTO events (ts, kind, app, title) VALUES (?1, 'focus', 'term', 'old-ev')",
            [old + i],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO events (ts, kind, app, title) VALUES (?1, 'focus', 'term', 'new-ev')",
        [fresh],
    )
    .unwrap();

    // Batch 1: old, uncorrected — span, interval, task, and batch all go.
    // Batch 2: old, but its task is corrected — task and batch survive.
    // Batch 3: fresh — untouched.
    for (id, start, end) in [
        (1, old - 10_000, old),
        (2, old - 10_000, old),
        (3, fresh, fresh + 10_000),
    ] {
        conn.execute(
            "INSERT INTO batches (id, start_ts, end_ts, status) VALUES (?1, ?2, ?3, 'done')",
            [id, start, end],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO spans (start_ts, end_ts, app, title, kind, batch_id)
             VALUES (?2, ?3, 'term', 'ancientspan', 'focus', ?1)",
            [id, start, end],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO tasks (id, label, status, source, created_ts)
             VALUES (?1, 'ancienttask', 'open', 'derived', ?2)",
            [id, start],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence)
             VALUES (?1, ?1, ?2, ?3, 0.5)",
            [id, start, end],
        )
        .unwrap();
    }
    // An old user-declared task with no intervals is never pruned.
    storage::insert_user_task(&conn, ms_to_ts(old), "declared goal", None).unwrap();
    storage::insert_correction(&mut conn, ms_to_ts(old), 2, "kept task", Some("proj")).unwrap();

    conn.execute(
        "INSERT INTO chat_messages (ts, role, content) VALUES (?1, 'user', 'old'), (?2, 'user', 'new')",
        [old, fresh],
    )
    .unwrap();

    let pruned = storage::prune(&conn, cutoff, 2).unwrap();
    assert!(pruned > 0);

    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(count("SELECT count(*) FROM events"), 1);
    assert_eq!(
        count("SELECT count(*) FROM spans"),
        1,
        "only the fresh span survives"
    );
    assert_eq!(
        count("SELECT count(*) FROM intervals"),
        1,
        "only the fresh interval survives"
    );
    assert_eq!(
        count("SELECT count(*) FROM tasks"),
        3,
        "corrected old task + fresh task + declared task"
    );
    assert_eq!(
        count("SELECT count(*) FROM tasks WHERE source='user'"),
        1,
        "user-declared tasks are never pruned"
    );
    assert_eq!(
        count("SELECT count(*) FROM batches"),
        1,
        "only the fresh, still-referenced batch survives"
    );
    assert_eq!(
        count("SELECT count(*) FROM corrections"),
        1,
        "corrections never pruned"
    );
    assert_eq!(count("SELECT count(*) FROM chat_messages"), 1);
    // AFTER DELETE triggers kept the external-content FTS in sync.
    assert_eq!(
        count("SELECT count(*) FROM spans_fts WHERE spans_fts MATCH 'ancientspan'"),
        1
    );
    // Idempotent: nothing left to prune.
    assert_eq!(storage::prune(&conn, cutoff, 2).unwrap(), 0);
}

#[test]
fn merge_task_folds_source_into_target() {
    use chronicle_core::storage;
    use chronicle_core::types::ms_to_ts;
    use rusqlite::params;

    let db = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("merge.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(db.with_extension(format!("db{suffix}")));
    }
    let mut conn = storage::open(&db).unwrap();

    let t0: i64 = 1_000_000_000_000;
    conn.execute(
        "INSERT INTO batches (id, start_ts, end_ts, status) VALUES (1, ?1, ?2, 'done')",
        [t0, t0 + 3_600_000],
    )
    .unwrap();
    // Focus span overlapping the stray's intervals — the merge ctx snapshot.
    conn.execute(
        "INSERT INTO spans (start_ts, end_ts, app, title, kind, batch_id)
         VALUES (?1, ?2, 'term', 'mergectxspan work', 'focus', 1)",
        [t0, t0 + 600_000],
    )
    .unwrap();
    // 10 = derived stray (uncorrected), 30 = derived with a prior correction.
    for (id, label) in [(10_i64, "stray browsing"), (30, "misc video")] {
        conn.execute(
            "INSERT INTO tasks (id, label, status, source, created_ts)
             VALUES (?1, ?2, 'open', 'derived', ?3)",
            params![id, label, t0],
        )
        .unwrap();
    }
    for (task_id, start, end) in [
        (10_i64, t0, t0 + 300_000),
        (10, t0 + 300_000, t0 + 600_000),
        (30, t0 + 600_000, t0 + 900_000),
    ] {
        conn.execute(
            "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence)
             VALUES (?1, 1, ?2, ?3, 0.5)",
            [task_id, start, end],
        )
        .unwrap();
    }
    let target = storage::insert_user_task(&conn, ms_to_ts(t0), "real work", Some("proj")).unwrap();
    storage::insert_correction(&mut conn, ms_to_ts(t0), 30, "watching talks", None).unwrap();

    let count = |conn: &rusqlite::Connection, sql: &str| -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    };

    // Self-merge is a no-op.
    storage::merge_task(&mut conn, ms_to_ts(t0 + 1), target, target).unwrap();
    assert_eq!(count(&conn, "SELECT count(*) FROM corrections"), 1);

    storage::merge_task(&mut conn, ms_to_ts(t0 + 1_000_000), 10, target).unwrap();
    assert_eq!(
        count(&conn, "SELECT count(*) FROM intervals WHERE task_id=10"),
        0,
        "all intervals moved off the source"
    );
    let moved: i64 = conn
        .query_row(
            "SELECT count(*) FROM intervals WHERE task_id=?1",
            [target],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(moved, 2);
    // One merge correction on the target, ctx snapshotted before the move.
    let (task_id, old_label, new_label, ctx, interval_id): (i64, String, String, String, Option<i64>) = conn
        .query_row(
            "SELECT task_id, old_label, new_label, ctx, interval_id FROM corrections WHERE kind='merge'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap();
    assert_eq!(task_id, target);
    assert_eq!(old_label, "stray browsing");
    assert_eq!(new_label, "real work");
    assert!(ctx.contains("mergectxspan"), "ctx = {ctx:?}");
    assert_eq!(interval_id, None);
    // FTS trigger indexed it — the pair is retrievable as teaching data.
    assert_eq!(
        count(
            &conn,
            "SELECT count(*) FROM corrections_fts WHERE corrections_fts MATCH 'mergectxspan'"
        ),
        1
    );
    assert_eq!(
        count(&conn, "SELECT count(*) FROM tasks WHERE id=10"),
        0,
        "unreferenced derived source is deleted"
    );

    // A corrected source survives the merge closed, not deleted.
    storage::merge_task(&mut conn, ms_to_ts(t0 + 2_000_000), 30, target).unwrap();
    let (status, closed_ts): (String, Option<i64>) = conn
        .query_row("SELECT status, closed_ts FROM tasks WHERE id=30", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .unwrap();
    assert_eq!(status, "closed");
    assert_eq!(closed_ts, Some(t0 + 2_000_000));
}

#[test]
fn autoclose_stale_derived_tasks() {
    use chronicle_core::storage;
    use chronicle_core::types::ms_to_ts;
    use rusqlite::params;

    let db = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("autoclose.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(db.with_extension(format!("db{suffix}")));
    }
    let conn = storage::open(&db).unwrap();

    let now: i64 = 1_000_000_000_000;
    let day = 86_400_000_i64;
    conn.execute(
        "INSERT INTO batches (id, start_ts, end_ts, status) VALUES (1, ?1, ?2, 'done')",
        [now - 5 * day, now],
    )
    .unwrap();
    // 1 = derived idle 4d (closes), 2 = derived fresh (stays open).
    for (id, last_end) in [(1_i64, now - 4 * day), (2, now - day)] {
        conn.execute(
            "INSERT INTO tasks (id, label, status, source, created_ts)
             VALUES (?1, 'derived task', 'open', 'derived', ?2)",
            params![id, last_end - 600_000],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence)
             VALUES (?1, 1, ?2, ?3, 0.5)",
            [id, last_end - 600_000, last_end],
        )
        .unwrap();
    }
    // Declared task idle forever stays open.
    let declared =
        storage::insert_user_task(&conn, ms_to_ts(now - 30 * day), "goal", None).unwrap();

    let closed = storage::autoclose_stale_tasks(&conn, ms_to_ts(now), 3).unwrap();
    assert_eq!(closed, 1);
    let status = |id: i64| -> String {
        conn.query_row("SELECT status FROM tasks WHERE id=?1", [id], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(status(1), "closed");
    assert_eq!(status(2), "open");
    assert_eq!(
        status(declared),
        "open",
        "declared tasks only close by hand"
    );
    let closed_ts: i64 = conn
        .query_row("SELECT closed_ts FROM tasks WHERE id=1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(closed_ts, now);
    // Idempotent.
    assert_eq!(
        storage::autoclose_stale_tasks(&conn, ms_to_ts(now), 3).unwrap(),
        0
    );
}

#[test]
fn digest_workspace_context_section() {
    let (spans, config) = day1();
    let batches = assign_batches(&spans, &config);
    let current = &spans[batches[0].spans.clone()];
    let plain = build_digest(current, &TimeZone::UTC, &[], &[], &[], None);
    let ctx = "### jira.search\nCHR-42 fix AFK split";
    let with = build_digest(current, &TimeZone::UTC, &[], &[], &[], Some(ctx));
    assert_eq!(with, format!("{plain}\n## Workspace context\n{ctx}\n"));
    // Blank context must not add the section (goldens stay MCP-free).
    assert_eq!(
        build_digest(current, &TimeZone::UTC, &[], &[], &[], Some("  \n")),
        plain
    );
}

#[test]
fn chat_context_includes_project_totals() {
    use chronicle_core::{chat, storage};
    use jiff::civil;
    use rusqlite::params;

    let db = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("chat_totals.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(db.with_extension(format!("db{suffix}")));
    }
    let conn = storage::open(&db).unwrap();

    // Wed 2026-08-26 14:30 UTC — "this week" resolves to Mon 2026-08-24.
    let now = civil::date(2026, 8, 26)
        .at(14, 30, 0, 0)
        .to_zoned(TimeZone::UTC)
        .unwrap();
    let t0 = civil::date(2026, 8, 24)
        .at(9, 0, 0, 0)
        .to_zoned(TimeZone::UTC)
        .unwrap()
        .timestamp()
        .as_millisecond();
    conn.execute(
        "INSERT INTO batches (id, start_ts, end_ts, status) VALUES (1, ?1, ?2, 'done')",
        [t0, t0 + 3_600_000],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tasks (id, label, project, status, source, created_ts)
         VALUES (1, 'm12 reports', 'chronicle', 'open', 'derived', ?1)",
        params![t0],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence)
         VALUES (1, 1, ?1, ?2, 0.9)",
        [t0, t0 + 90 * 60_000],
    )
    .unwrap();

    let ctx = chat::build_context(&conn, "how long on chronicle this week?", &now).unwrap();
    assert!(
        ctx.contains("## Totals by project") && ctx.contains("- chronicle: 1h30m"),
        "missing totals section:\n{ctx}"
    );
}

#[test]
fn digest_git_activity_section() {
    use chronicle_core::types::{ActivityEvent, ActivityKind, ms_to_ts, ts_to_ms};

    let (spans, config) = day1();
    let batches = assign_batches(&spans, &config);
    let current = &spans[batches[0].spans.clone()];
    let plain = build_digest(current, &TimeZone::UTC, &[], &[], &[], None);
    let t0 = ts_to_ms(current.first().unwrap().start);
    let vcs = [
        ActivityEvent {
            ts: ms_to_ts(t0 + 60_000),
            repo: "app".into(),
            branch: "ABC-123-sending-plans".into(),
            kind: ActivityKind::Checkout,
            ext_id: None,
            end_ts: None,
            summary: None,
        },
        ActivityEvent {
            ts: ms_to_ts(t0 + 120_000),
            repo: "app".into(),
            branch: "ABC-123-sending-plans".into(),
            kind: ActivityKind::Commit,
            ext_id: Some("abc123".into()),
            end_ts: None,
            summary: Some("feat: plan model".into()),
        },
    ];
    let with = build_digest(current, &TimeZone::UTC, &[], &[], &vcs, None);
    assert!(with.contains("## Activity"), "digest: {with}");
    assert!(
        with.contains("checkout app \u{2192} ABC-123-sending-plans"),
        "digest: {with}"
    );
    assert!(
        with.contains("commit app \"feat: plan model\" [ABC-123-sending-plans]"),
        "digest: {with}"
    );
    // Out-of-window events must not add the section (goldens stay git-free).
    let outside = [ActivityEvent {
        ts: ms_to_ts(t0 - 3_600_000),
        ..vcs[0].clone()
    }];
    assert_eq!(
        build_digest(current, &TimeZone::UTC, &[], &[], &outside, None),
        plain
    );
}

#[test]
fn digest_activity_section_mixed_kinds() {
    use chronicle_core::types::{ActivityEvent, ActivityKind, ms_to_ts, ts_to_ms};

    let (spans, config) = day1();
    let batches = assign_batches(&spans, &config);
    let current = &spans[batches[0].spans.clone()];
    let t0 = ts_to_ms(current.first().unwrap().start);
    let ev =
        |kind, off: i64, dur: Option<i64>, repo: &str, branch: &str, summary: &str| ActivityEvent {
            ts: ms_to_ts(t0 + off),
            end_ts: dur.map(|d| ms_to_ts(t0 + off + d)),
            repo: repo.into(),
            branch: branch.into(),
            kind,
            ext_id: Some(format!("{off}")),
            summary: Some(summary.into()),
        };
    let activity = [
        ev(
            ActivityKind::AiSession,
            60_000,
            Some(23 * 60_000),
            "app",
            "ABC-123-x",
            "fix the flaky test",
        ),
        ev(
            ActivityKind::PrAuthored,
            120_000,
            None,
            "app",
            "",
            "#40 ABC-123: ship it · open",
        ),
        ev(
            ActivityKind::Call,
            180_000,
            Some(32 * 60_000),
            "",
            "",
            "Firefox",
        ),
        ev(ActivityKind::Call, 240_000, None, "", "", "Firefox"),
    ];
    let with = build_digest(current, &TimeZone::UTC, &[], &[], &activity, None);
    assert!(with.contains("## Activity"), "digest: {with}");
    assert!(
        with.contains("claude app@ABC-123-x 23m00s \"fix the flaky test\""),
        "digest: {with}"
    );
    assert!(
        with.contains("PR authored app #40 ABC-123: ship it · open"),
        "digest: {with}"
    );
    assert!(with.contains("call 32m00s (Firefox)"), "digest: {with}");
    assert!(with.contains("call (ongoing) (Firefox)"), "digest: {with}");
}

#[test]
fn activity_events_upsert_and_ignore_paths() {
    use chronicle_core::storage;
    use chronicle_core::types::{ActivityEvent, ActivityKind, ms_to_ts};

    let db = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("m22_activity.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(db.with_extension(format!("db{suffix}")));
    }
    let conn = storage::open(&db).unwrap();
    let ev = |kind, ts: i64, end: Option<i64>, ext: &str, summary: Option<&str>| ActivityEvent {
        ts: ms_to_ts(ts),
        end_ts: end.map(ms_to_ts),
        repo: "app".into(),
        branch: "main".into(),
        kind,
        ext_id: Some(ext.into()),
        summary: summary.map(Into::into),
    };

    // Span kinds: one row per ext_id, end_ts follows, empty summary fills in.
    let s = ActivityKind::AiSession;
    storage::insert_activity_event(&conn, &ev(s, 1_000, Some(1_000), "sess", None)).unwrap();
    storage::insert_activity_event(&conn, &ev(s, 1_000, Some(9_000), "sess", Some("hi"))).unwrap();
    storage::insert_activity_event(&conn, &ev(s, 1_000, Some(20_000), "sess", Some("later")))
        .unwrap();
    let rows = storage::activity_in_range(&conn, 0, 100_000).unwrap();
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].end_ts, Some(ms_to_ts(20_000)));
    assert_eq!(rows[0].summary.as_deref(), Some("hi"));

    // PR kinds: one row per (kind, ext_id, ts); a bumped ts is a new marker.
    let p = ActivityKind::PrAuthored;
    storage::insert_activity_event(&conn, &ev(p, 2_000, None, "url", Some("#1"))).unwrap();
    storage::insert_activity_event(&conn, &ev(p, 2_000, None, "url", Some("#1"))).unwrap();
    storage::insert_activity_event(&conn, &ev(p, 3_000, None, "url", Some("#1"))).unwrap();
    storage::insert_activity_event(
        &conn,
        &ev(ActivityKind::PrReviewed, 3_000, None, "url", None),
    )
    .unwrap();
    let rows = storage::activity_in_range(&conn, 0, 100_000).unwrap();
    assert_eq!(rows.len(), 4, "{rows:?}");

    // Per-kind "last seen": the span's extended end wins over its start.
    let per_kind = storage::latest_activity_per_kind(&conn).unwrap();
    let sess = per_kind
        .iter()
        .find(|e| e.kind == ActivityKind::AiSession)
        .unwrap();
    assert_eq!(sess.end_ts, Some(ms_to_ts(20_000)));
    assert_eq!(per_kind.len(), 3, "{per_kind:?}");

    // Git-only views never see the new kinds.
    assert!(storage::vcs_in_range(&conn, 0, 100_000).unwrap().is_empty());
    assert!(
        storage::latest_vcs_event_per_repo(&conn)
            .unwrap()
            .is_empty()
    );

    // A span overlaps every interval it covers, a point only its own.
    conn.execute(
        "INSERT INTO batches (start_ts, end_ts, status) VALUES (0, 100000, 'done')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO tasks (label, status, created_ts) VALUES ('a', 'open', 0)",
        [],
    )
    .unwrap();
    let task = conn.last_insert_rowid();
    for (lo, hi) in [(0, 1_500), (5_000, 6_000), (50_000, 60_000)] {
        conn.execute(
            "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence) VALUES (?1, 1, ?2, ?3, 1.0)",
            rusqlite::params![task, lo, hi],
        )
        .unwrap();
    }
    let by_task = storage::activity_in_range_by_task(&conn, 0, 100_000).unwrap();
    let sessions = by_task
        .iter()
        .filter(|(_, e)| e.kind == ActivityKind::AiSession)
        .count();
    assert_eq!(sessions, 1, "session row is DISTINCT per task: {by_task:?}");
    assert_eq!(
        by_task.len(),
        1,
        "point events outside intervals stay out: {by_task:?}"
    );

    // Repo signal: a task whose project names another repo never inherits a
    // session that merely overlapped it in time; case-insensitive project
    // match, a branch carrying the external_ref, and repo-less rows attach.
    let mut task_with = |project: Option<&str>, ext: Option<&str>| {
        conn.execute(
            "INSERT INTO tasks (label, project, external_ref, status, created_ts)
             VALUES ('t', ?1, ?2, 'open', 0)",
            rusqlite::params![project, ext],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence) VALUES (?1, 1, 2_500, 4_000, 1.0)",
            [id],
        )
        .unwrap();
        id
    };
    let other = task_with(Some("mailer"), None);
    let upper = task_with(Some("APP"), None);
    let anchored = task_with(Some("nothing"), Some("ABC-9"));
    let none = task_with(None, None);
    storage::insert_activity_event(
        &conn,
        &ActivityEvent {
            repo: "mailer".into(),
            ..ev(ActivityKind::Checkout, 100, None, "c", None)
        },
    )
    .unwrap();
    storage::insert_activity_event(
        &conn,
        &ActivityEvent {
            branch: "feat/ABC-9".into(),
            ..ev(ActivityKind::Checkout, 200, None, "c2", None)
        },
    )
    .unwrap();
    storage::insert_activity_event(
        &conn,
        &ActivityEvent {
            repo: String::new(),
            ..ev(ActivityKind::Call, 3_000, Some(3_500), "call", None)
        },
    )
    .unwrap();
    let by_task = storage::activity_in_range_by_task(&conn, 0, 100_000).unwrap();
    let kinds = |t: i64| -> Vec<ActivityKind> {
        by_task
            .iter()
            .filter(|(id, _)| *id == t)
            .map(|(_, e)| e.kind)
            .collect()
    };
    assert_eq!(kinds(other), vec![ActivityKind::Call], "{by_task:?}");
    assert!(
        kinds(upper).contains(&ActivityKind::AiSession),
        "{by_task:?}"
    );
    assert!(
        kinds(anchored).contains(&ActivityKind::AiSession),
        "{by_task:?}"
    );
    assert!(
        kinds(none).contains(&ActivityKind::AiSession),
        "{by_task:?}"
    );
    let journal = storage::activity_for_task_in_range(&conn, other, 0, 100_000).unwrap();
    assert_eq!(
        journal.iter().map(|e| e.kind).collect::<Vec<_>>(),
        vec![ActivityKind::Call],
        "{journal:?}"
    );
}

#[test]
fn vcs_events_store_dedupe_and_anchor_guard() {
    use chronicle_core::storage;
    use chronicle_core::types::{ActivityEvent, ActivityKind, NewInterval, TaskSlot, ms_to_ts};

    let db = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("m15_vcs.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(db.with_extension(format!("db{suffix}")));
    }
    let mut conn = storage::open(&db).unwrap();

    let checkout = |ts: i64, branch: &str| ActivityEvent {
        ts: ms_to_ts(ts),
        repo: "app".into(),
        branch: branch.into(),
        kind: ActivityKind::Checkout,
        ext_id: None,
        end_ts: None,
        summary: None,
    };
    storage::insert_activity_event(&conn, &checkout(1_000, "main")).unwrap();
    // Re-announced state on daemon restart must not stack a duplicate.
    storage::insert_activity_event(&conn, &checkout(2_000, "main")).unwrap();
    storage::insert_activity_event(&conn, &checkout(3_000, "ABC-1-x")).unwrap();
    let commit = ActivityEvent {
        ts: ms_to_ts(4_000),
        repo: "app".into(),
        branch: "ABC-1-x".into(),
        kind: ActivityKind::Commit,
        ext_id: Some("abc".into()),
        end_ts: None,
        summary: Some("feat: x".into()),
    };
    storage::insert_activity_event(&conn, &commit).unwrap();
    let all = storage::vcs_in_range(&conn, 0, 10_000).unwrap();
    assert_eq!(all.len(), 3, "duplicate checkout must be dropped");

    // Branch state strictly before a window: latest checkout per repo.
    let prior = storage::branch_state_before(&conn, 3_500).unwrap();
    assert_eq!(prior.len(), 1);
    assert_eq!(prior[0].branch, "ABC-1-x");

    // Anchor guard: first ref wins, no overwrite.
    let slots = [TaskSlot::New {
        label: "work".into(),
        project: None,
    }];
    let intervals = [NewInterval {
        slot: 0,
        start_ts: ms_to_ts(3_000),
        end_ts: ms_to_ts(5_000),
        confidence: 0.9,
    }];
    conn.execute(
        "INSERT INTO batches (start_ts, end_ts, status) VALUES (3000, 5000, 'running')",
        [],
    )
    .unwrap();
    let batch_id = conn.last_insert_rowid();
    let stored = storage::store_derivation(&mut conn, batch_id, &slots, &intervals).unwrap();
    assert_eq!(stored.len(), 1);
    let task_id = stored[0].0;
    storage::set_task_external_ref(&conn, task_id, "ABC-1").unwrap();
    storage::set_task_external_ref(&conn, task_id, "XYZ-9").unwrap();
    let got: String = conn
        .query_row(
            "SELECT external_ref FROM tasks WHERE id=?1",
            [task_id],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(got, "ABC-1", "first ref wins, no overwrite");

    // Commits inside the task's intervals surface as evidence.
    let commits = storage::commits_for_task(&conn, task_id).unwrap();
    assert_eq!(commits.len(), 1);
    assert_eq!(commits[0].summary.as_deref(), Some("feat: x"));
}
