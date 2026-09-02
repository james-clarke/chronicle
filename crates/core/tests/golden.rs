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
    let task_with = |project: Option<&str>, ext: Option<&str>| {
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

#[test]
fn unassigned_runs_fold_and_assign_claims_per_batch() {
    use chronicle_core::storage;
    use chronicle_core::types::ms_to_ts;

    let db = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("m23_triage.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(db.with_extension(format!("db{suffix}")));
    }
    let mut conn = storage::open(&db).unwrap();
    for (lo, hi) in [(0, 10_000), (10_000, 100_000)] {
        conn.execute(
            "INSERT INTO batches (start_ts, end_ts, status) VALUES (?1, ?2, 'done')",
            rusqlite::params![lo, hi],
        )
        .unwrap();
    }
    // Run A straddles the batch edge (spans 1: 8000-9500, 2: 9600-12000);
    // run B starts after a long gap and is unbatched (resolved by start ts);
    // a covered span and an afk span stay out.
    let spans = [
        (
            8_000,
            9_500,
            "Code",
            "chronicle — main.rs",
            "focus",
            Some(1),
        ),
        (
            9_600,
            12_000,
            "Code",
            "chronicle — storage.rs",
            "focus",
            Some(2),
        ),
        (12_000, 12_500, "Firefox", "afk", "afk", Some(2)),
        (40_000, 45_000, "Firefox", "ACME-1 review", "focus", None),
        (51_000, 52_000, "Slack", "covered", "focus", Some(2)),
    ];
    for (s, e, app, title, kind, batch) in spans {
        conn.execute(
            "INSERT INTO spans (start_ts, end_ts, app, title, kind, batch_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![s, e, app, title, kind, batch],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO tasks (label, project, status, created_ts) VALUES ('Chronicle triage', 'chronicle', 'open', 0)",
        [],
    )
    .unwrap();
    let task = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence) VALUES (?1, 2, 50000, 60000, 0.9)",
        [task],
    )
    .unwrap();

    let runs = storage::unassigned_runs(&conn, 0, 100_000, 10_000).unwrap();
    assert_eq!(runs.len(), 2, "{runs:?}");
    assert_eq!(
        (runs[0].start_ts, runs[0].end_ts, runs[0].ms),
        (8_000, 12_000, 3_900)
    );
    assert_eq!(
        runs[0].lines[0].1, "chronicle — storage.rs",
        "largest line first"
    );
    assert_eq!(
        runs[1].lines,
        vec![("Firefox".into(), "ACME-1 review".into(), 5_000)]
    );

    let claimed = storage::assign_unassigned(
        &mut conn,
        ms_to_ts(70_000),
        runs[0].start_ts,
        runs[0].end_ts,
        task,
    )
    .unwrap();
    assert_eq!(
        claimed, 3_900,
        "one interval per batch, span bounds within each"
    );
    let intervals: Vec<(i64, i64, i64)> = conn
        .prepare("SELECT batch_id, start_ts, end_ts FROM intervals WHERE task_id=?1 AND confidence=1.0 ORDER BY start_ts")
        .unwrap()
        .query_map([task], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(intervals, vec![(1, 8_000, 9_500), (2, 9_600, 12_000)]);
    let after = storage::unassigned_runs(&conn, 0, 100_000, 10_000).unwrap();
    assert_eq!(after.len(), 1, "{after:?}");
    assert_eq!(after[0].start_ts, 40_000);

    // The claim is teaching data: the run's app/title mix now suggests the task.
    let kind: String = conn
        .query_row(
            "SELECT kind FROM corrections WHERE task_id=?1 ORDER BY id LIMIT 1",
            [task],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kind, "assign");
    let hint = storage::suggest_correction(&conn, "Code chronicle storage.rs").unwrap();
    assert_eq!(
        hint.map(|c| c.new_label).as_deref(),
        Some("Chronicle triage")
    );
    assert!(
        storage::suggest_correction(&conn, "Zoom standup")
            .unwrap()
            .is_none()
    );

    // Once enough corrections exist, terms common to most of them (the
    // terminal's prompt) stop matching on their own; a distinctive one still does.
    for label in ["Alpha", "Beta", "Gamma", "Delta"] {
        conn.execute(
            "INSERT INTO tasks (label, status, created_ts) VALUES (?1, 'open', 0)",
            [label],
        )
        .unwrap();
        let id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO corrections (ts, task_id, old_label, new_label, ctx, kind)
             VALUES (0, ?1, 'x', ?2, ?3, 'rename')",
            rusqlite::params![
                id,
                label,
                format!("Terminator sam@host:~/dev/{}", label.to_lowercase())
            ],
        )
        .unwrap();
    }
    assert!(
        storage::suggest_correction(&conn, "Terminator sam@host:~/dev/other")
            .unwrap()
            .is_none(),
        "prompt-only text must not suggest"
    );
    assert_eq!(
        storage::suggest_correction(&conn, "Terminator sam@host:~/dev/gamma")
            .unwrap()
            .map(|c| c.new_label)
            .as_deref(),
        Some("Gamma")
    );

    // Unbatched span resolves to the batch its start falls in.
    let claimed =
        storage::assign_unassigned(&mut conn, ms_to_ts(70_500), 40_000, 45_000, task).unwrap();
    assert_eq!(claimed, 5_000);
    let batch: i64 = conn
        .query_row(
            "SELECT batch_id FROM intervals WHERE start_ts=40000",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(batch, 2);

    // Assigning where nothing is unassigned claims nothing and logs nothing.
    let none =
        storage::assign_unassigned(&mut conn, ms_to_ts(71_000), 50_000, 60_000, task).unwrap();
    assert_eq!(none, 0);
}

#[test]
fn prepass_places_runs_and_derive_keeps_user_rows() {
    use chronicle_core::prepass;
    use chronicle_core::storage;
    use chronicle_core::types::{ActivityEvent, ActivityKind, NewInterval, TaskSlot, ms_to_ts};

    let db = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("m24_prepass.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(db.with_extension(format!("db{suffix}")));
    }
    let mut conn = storage::open(&db).unwrap();
    let config = Config::default();
    let task = |conn: &rusqlite::Connection,
                label: &str,
                project: Option<&str>,
                source: &str,
                ext: Option<&str>| {
        conn.execute(
            "INSERT INTO tasks (label, project, status, source, created_ts, external_ref) VALUES (?1, ?2, 'open', ?3, 0, ?4)",
            rusqlite::params![label, project, source, ext],
        )
        .unwrap();
        conn.last_insert_rowid()
    };
    let ticket = task(&conn, "Sending plans", Some("pb"), "user", Some("ACME-7"));
    let chron_old = task(
        &conn,
        "Chronicle timeline",
        Some("chronicle"),
        "derived",
        None,
    );
    let chron_new = task(&conn, "Chronicle feed", Some("chronicle"), "derived", None);
    let board = task(&conn, "Board triage", None, "user", None);
    // Recency decides between two open tasks on one repo.
    for (t, end) in [(chron_old, 50_000), (chron_new, 90_000)] {
        conn.execute(
            "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence) VALUES (?1, NULL, ?2, ?3, 0.9)",
            rusqlite::params![t, end - 10_000, end],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO corrections (ts, task_id, old_label, new_label, ctx, kind)
         VALUES (1, ?1, '(unassigned)', 'Board triage', 'Firefox Jira board ACME', 'assign')",
        [board],
    )
    .unwrap();
    let event = |ts: i64,
                 end: Option<i64>,
                 repo: &str,
                 branch: &str,
                 kind: ActivityKind,
                 ext: Option<&str>| ActivityEvent {
        ts: ms_to_ts(ts),
        end_ts: end.map(ms_to_ts),
        repo: repo.into(),
        branch: branch.into(),
        kind,
        ext_id: ext.map(str::to_owned),
        summary: None,
    };
    storage::insert_activity_event(
        &conn,
        &event(
            100_000,
            None,
            "pb",
            "ACME-7-fix",
            ActivityKind::Checkout,
            None,
        ),
    )
    .unwrap();
    storage::insert_activity_event(
        &conn,
        &event(
            1_020_000,
            Some(1_080_000),
            "chronicle",
            "main",
            ActivityKind::AiSession,
            Some("s1"),
        ),
    )
    .unwrap();
    // A: ticketed branch; B: chronicle session; C: past correction;
    // D: too short to settle; E: nothing matches.
    let spans = [
        (100_000, 200_000, "Code", "pb — plans.rs"),
        (1_000_000, 1_100_000, "Code", "chronicle — feed.rs"),
        (1_600_000, 1_700_000, "Firefox", "Jira board ACME"),
        (2_200_000, 2_230_000, "Code", "pb — plans.rs"),
        (2_800_000, 2_900_000, "Zoom", "standup"),
    ];
    for (s, e, app, title) in spans {
        conn.execute(
            "INSERT INTO spans (start_ts, end_ts, app, title, kind, batch_id) VALUES (?1, ?2, ?3, ?4, 'focus', NULL)",
            rusqlite::params![s, e, app, title],
        )
        .unwrap();
    }

    let placed = prepass::run(&mut conn, &config, ms_to_ts(3_500_000)).unwrap();
    let got: Vec<(i64, i64, i64, &str)> = placed
        .iter()
        .map(|p| (p.task_id, p.start_ts, p.end_ts, p.reason.as_str()))
        .collect();
    assert_eq!(
        got,
        vec![
            (ticket, 100_000, 200_000, "branch ACME-7"),
            (chron_new, 1_000_000, 1_100_000, "repo chronicle"),
            (board, 1_600_000, 1_700_000, "past correction"),
        ]
    );
    type Row = (i64, Option<i64>, i64, f64, String, Option<String>);
    let rows = |conn: &rusqlite::Connection| -> Vec<Row> {
        conn.prepare("SELECT task_id, batch_id, start_ts, confidence, source, reason FROM intervals WHERE start_ts >= 100000 ORDER BY start_ts")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    let first = rows(&conn);
    assert_eq!(first.len(), 3, "{first:?}");
    assert_eq!(
        (
            first[0].1,
            first[0].3,
            first[0].4.as_str(),
            first[0].5.as_deref()
        ),
        (None, 0.5, "prepass", Some("branch ACME-7"))
    );
    // Same tick again: rewritten in place, no duplicates.
    prepass::run(&mut conn, &config, ms_to_ts(3_500_000)).unwrap();
    assert_eq!(rows(&conn), first);

    // Eject B from its task: the next pre-pass leaves B alone (hard
    // negative), the other placements stand.
    let b_id: i64 = conn
        .query_row("SELECT id FROM intervals WHERE start_ts=1000000", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(
        storage::split_interval(&mut conn, ms_to_ts(3_600_000), b_id, 1_000_000, 1_100_000)
            .unwrap(),
        100_000
    );
    // The eject names one task: the repo rule falls through to the other
    // open chronicle task rather than giving up on the block.
    let placed = prepass::run(&mut conn, &config, ms_to_ts(3_700_000)).unwrap();
    let b = placed.iter().find(|p| p.start_ts == 1_000_000).unwrap();
    assert_eq!(
        (b.task_id, b.reason.as_str()),
        (chron_old, "repo chronicle")
    );
    // The user moves B under the ticket by hand: a user row over the tail.
    let b_id: i64 = conn
        .query_row("SELECT id FROM intervals WHERE start_ts=1000000", [], |r| {
            r.get(0)
        })
        .unwrap();
    storage::reassign_intervals(&mut conn, ms_to_ts(3_800_000), &[b_id], ticket).unwrap();
    let (b_batch, b_source): (Option<i64>, String) = conn
        .query_row(
            "SELECT batch_id, source FROM intervals WHERE start_ts=1000000",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((b_batch, b_source.as_str()), (None, "user"));

    // The sessionizer closes a batch over A–C: tail rows attach to it.
    conn.execute(
        "INSERT INTO batches (start_ts, end_ts, status) VALUES (0, 2000000, 'pending')",
        [],
    )
    .unwrap();
    let batch = conn.last_insert_rowid();
    assert_eq!(storage::attach_tail_intervals(&conn).unwrap(), 5);
    // Derive claims the whole batch for one task: pre-pass rows go, the
    // user's B survives and the model's interval is clipped around it.
    conn.execute("UPDATE batches SET status='running' WHERE id=?1", [batch])
        .unwrap();
    let stored = storage::store_derivation(
        &mut conn,
        batch,
        &[TaskSlot::Existing(chron_new)],
        &[NewInterval {
            slot: 0,
            start_ts: ms_to_ts(100_000),
            end_ts: ms_to_ts(2_000_000),
            confidence: 0.8,
        }],
    )
    .unwrap();
    assert_eq!(
        stored,
        vec![
            (chron_new, 100_000, 1_000_000),
            (chron_new, 1_100_000, 2_000_000)
        ]
    );
    let after = rows(&conn);
    let summary: Vec<(i64, i64, &str)> = after.iter().map(|r| (r.0, r.2, r.4.as_str())).collect();
    assert_eq!(
        summary,
        vec![
            (chron_new, 100_000, "derived"),
            (ticket, 1_000_000, "user"),
            (chron_new, 1_100_000, "derived"),
        ]
    );
    // The window now opens after the derived batch: E is still unmatched
    // and nothing older is touched.
    assert_eq!(
        storage::latest_done_batch_end(&conn).unwrap(),
        Some(2_000_000)
    );
    assert!(
        prepass::run(&mut conn, &config, ms_to_ts(3_900_000))
            .unwrap()
            .is_empty()
    );
    assert_eq!(rows(&conn).len(), 3);
}

#[test]
fn eject_splits_interval_and_blocks_suggestion() {
    use chronicle_core::storage;
    use chronicle_core::types::ms_to_ts;

    let db = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("m24_eject.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(db.with_extension(format!("db{suffix}")));
    }
    let mut conn = storage::open(&db).unwrap();
    conn.execute(
        "INSERT INTO batches (start_ts, end_ts, status) VALUES (0, 100000, 'done')",
        [],
    )
    .unwrap();
    let spans = [
        (10_000, 20_000, "Code", "chronicle — storage.rs"),
        (20_000, 30_000, "Firefox", "Jira ACME-7 board"),
        (30_000, 60_000, "Code", "chronicle — digest.rs"),
    ];
    for (s, e, app, title) in spans {
        conn.execute(
            "INSERT INTO spans (start_ts, end_ts, app, title, kind, batch_id) VALUES (?1, ?2, ?3, ?4, 'focus', 1)",
            rusqlite::params![s, e, app, title],
        )
        .unwrap();
    }
    conn.execute(
        "INSERT INTO tasks (label, project, status, created_ts) VALUES ('Chronicle', 'chronicle', 'open', 0)",
        [],
    )
    .unwrap();
    let task = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence) VALUES (?1, 1, 10000, 60000, 0.9)",
        [task],
    )
    .unwrap();
    let interval = conn.last_insert_rowid();
    let source: String = conn
        .query_row(
            "SELECT source FROM intervals WHERE id=?1",
            [interval],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(source, "derived", "migration default");

    // A block outside the interval ejects nothing and logs nothing.
    let none =
        storage::split_interval(&mut conn, ms_to_ts(70_000), interval, 60_000, 70_000).unwrap();
    assert_eq!(none, 0);
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM corrections", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);

    // Middle block out: left piece keeps the id, right piece is a new row of
    // the same task/batch/confidence/source, the block is unassigned again.
    let ejected =
        storage::split_interval(&mut conn, ms_to_ts(70_000), interval, 20_000, 30_000).unwrap();
    assert_eq!(ejected, 10_000);
    let pieces: Vec<(i64, i64, i64, f64, String)> = conn
        .prepare("SELECT id, start_ts, end_ts, confidence, source FROM intervals WHERE task_id=?1 ORDER BY start_ts")
        .unwrap()
        .query_map([task], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(pieces.len(), 2, "{pieces:?}");
    assert_eq!(
        (
            pieces[0].0,
            pieces[0].1,
            pieces[0].2,
            pieces[0].3,
            pieces[0].4.as_str()
        ),
        (interval, 10_000, 20_000, 0.9, "derived")
    );
    assert_eq!(
        (pieces[1].1, pieces[1].2, pieces[1].3, pieces[1].4.as_str()),
        (30_000, 60_000, 0.9, "derived")
    );
    assert_ne!(pieces[1].0, interval);
    let runs = storage::unassigned_runs(&conn, 0, 100_000, 1_000).unwrap();
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!((runs[0].start_ts, runs[0].end_ts), (20_000, 30_000));
    let (kind, old_label, new_label, old_project, interval_id, ctx): (
        String,
        String,
        String,
        Option<String>,
        Option<i64>,
        String,
    ) = conn
        .query_row(
            "SELECT kind, old_label, new_label, old_project, interval_id, ctx FROM corrections WHERE task_id=?1",
            [task],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
        )
        .unwrap();
    assert_eq!(
        (
            kind.as_str(),
            old_label.as_str(),
            new_label.as_str(),
            old_project.as_deref(),
            interval_id
        ),
        (
            "eject",
            "Chronicle",
            "(unassigned)",
            Some("chronicle"),
            Some(interval)
        )
    );
    assert_eq!(
        ctx, "Firefox Jira ACME-7 board\n",
        "only the block's spans"
    );

    // The eject is a negative: even after the user assigns similar work to
    // the task elsewhere, that text no longer suggests it, and the digest's
    // few-shot set keeps only the positive.
    conn.execute(
        "INSERT INTO corrections (ts, task_id, old_label, new_label, old_project, new_project, ctx, kind)
         VALUES (1, ?1, '(unassigned)', 'Chronicle', NULL, 'chronicle', 'Firefox Jira ACME-7 board', 'assign')",
        [task],
    )
    .unwrap();
    assert!(
        storage::suggest_correction(&conn, "Firefox Jira ACME-7 board")
            .unwrap()
            .is_none(),
        "ejected task must not be suggested"
    );
    let spans = storage::spans_in_range(&conn, 20_000, 30_000).unwrap();
    let few_shot = storage::similar_corrections(&conn, &spans, 4).unwrap();
    assert_eq!(few_shot.len(), 1, "{few_shot:?}");
    assert_eq!(few_shot[0].kind, "assign");
    // A different task with the same evidence is still suggested.
    conn.execute(
        "INSERT INTO tasks (label, project, status, created_ts) VALUES ('Board triage', NULL, 'open', 0)",
        [],
    )
    .unwrap();
    let other = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO corrections (ts, task_id, old_label, new_label, ctx, kind)
         VALUES (2, ?1, '(unassigned)', 'Board triage', 'Firefox Jira ACME-7 board', 'assign')",
        [other],
    )
    .unwrap();
    assert_eq!(
        storage::suggest_correction(&conn, "Firefox Jira ACME-7 board")
            .unwrap()
            .map(|c| c.new_label)
            .as_deref(),
        Some("Board triage")
    );

    // Ejecting a whole interval removes the row (references cleared) and a
    // derived task left with nothing is gone; a user task stays.
    conn.execute(
        "INSERT INTO tasks (label, status, source, created_ts) VALUES ('Derived only', 'open', 'derived', 0)",
        [],
    )
    .unwrap();
    let derived = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence, source) VALUES (?1, 1, 60000, 70000, 0.8, 'prepass')",
        [derived],
    )
    .unwrap();
    let whole = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO corrections (ts, task_id, old_label, new_label, ctx, kind, interval_id)
         VALUES (3, ?1, 'x', 'Derived only', '', 'rename', ?2)",
        rusqlite::params![derived, whole],
    )
    .unwrap();
    let ejected = storage::split_interval(&mut conn, ms_to_ts(80_000), whole, 0, 100_000).unwrap();
    assert_eq!(ejected, 10_000);
    let left: i64 = conn
        .query_row("SELECT COUNT(*) FROM intervals WHERE id=?1", [whole], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(left, 0);
    let refs: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM corrections WHERE interval_id=?1",
            [whole],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(refs, 0, "dangling interval refs cleared");
    let alive: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks WHERE id=?1", [derived], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(alive, 1, "a task with corrections is kept for its history");

    // Reassign by hand marks the moved interval as the user's.
    storage::reassign_intervals(&mut conn, ms_to_ts(90_000), &[interval], other).unwrap();
    let source: String = conn
        .query_row(
            "SELECT source FROM intervals WHERE id=?1",
            [interval],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(source, "user");
}

#[test]
fn feed_lists_blocks_newest_first_and_keep_survives_derive() {
    use chronicle_core::prepass;
    use chronicle_core::storage;
    use chronicle_core::types::{NewInterval, TaskSlot, ms_to_ts};

    let db = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("m24_feed.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(db.with_extension(format!("db{suffix}")));
    }
    let mut conn = storage::open(&db).unwrap();
    let config = Config::default();
    conn.execute(
        "INSERT INTO tasks (label, project, status, source, created_ts, external_ref) VALUES ('Sending plans', 'pb', 'open', 'user', 0, 'ACME-7')",
        [],
    )
    .unwrap();
    let ticket = conn.last_insert_rowid();
    conn.execute(
        "INSERT INTO tasks (label, project, status, source, created_ts) VALUES ('Board triage', NULL, 'open', 'user', 0)",
        [],
    )
    .unwrap();
    let board = conn.last_insert_rowid();
    // An older derived batch covering a model interval, then an unmatched
    // run, then the live tail with a ticketed-branch run.
    conn.execute(
        "INSERT INTO batches (start_ts, end_ts, status) VALUES (0, 600000, 'done')",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence, source) VALUES (?1, 1, 100000, 200000, 0.8, 'derived')",
        [board],
    )
    .unwrap();
    storage::insert_activity_event(
        &conn,
        &chronicle_core::types::ActivityEvent {
            ts: ms_to_ts(1_000_000),
            end_ts: None,
            repo: "pb".into(),
            branch: "ACME-7-fix".into(),
            kind: chronicle_core::types::ActivityKind::Checkout,
            ext_id: None,
            summary: None,
        },
    )
    .unwrap();
    let spans = [
        (100_000, 150_000, "Firefox", "Jira board", 1),
        (150_000, 200_000, "Slack", "triage", 1),
        (300_000, 400_000, "Zoom", "standup", 1),
        (1_000_000, 1_100_000, "Code", "pb — plans.rs", 0),
    ];
    for (s, e, app, title, batch) in spans {
        conn.execute(
            "INSERT INTO spans (start_ts, end_ts, app, title, kind, batch_id) VALUES (?1, ?2, ?3, ?4, 'focus', NULLIF(?5, 0))",
            rusqlite::params![s, e, app, title, batch],
        )
        .unwrap();
    }
    prepass::run(&mut conn, &config, ms_to_ts(1_500_000)).unwrap();

    let feed = storage::feed_blocks(&conn, 0, 2_000_000, prepass::RUN_GAP_MS, 12).unwrap();
    type Row<'a> = (i64, i64, Option<(&'a str, &'a str)>, bool, &'a str);
    let summary: Vec<Row> = feed
        .iter()
        .map(|b| {
            (
                b.start_ts,
                b.ms,
                b.claim
                    .as_ref()
                    .map(|c| (c.source.as_str(), c.reason.as_deref().unwrap_or(""))),
                b.derived,
                b.lines[0].1.as_str(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                1_000_000,
                100_000,
                Some(("prepass", "branch ACME-7")),
                false,
                "pb — plans.rs"
            ),
            (300_000, 100_000, None, true, "standup"),
            (100_000, 100_000, Some(("derived", "")), false, "Jira board"),
        ]
    );
    assert_eq!(feed[2].lines.len(), 2);
    // Cap keeps the newest.
    assert_eq!(
        storage::feed_blocks(&conn, 0, 2_000_000, prepass::RUN_GAP_MS, 1)
            .unwrap()
            .len(),
        1
    );

    // Keep the provisional block: a user row with a teaching correction,
    // and the next pre-pass leaves it alone.
    let id = feed[0].claim.as_ref().unwrap().interval_id;
    storage::keep_interval(&mut conn, ms_to_ts(1_600_000), id).unwrap();
    let (source, confidence, reason): (String, f64, Option<String>) = conn
        .query_row(
            "SELECT source, confidence, reason FROM intervals WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (source.as_str(), confidence, reason.as_deref()),
        ("user", 1.0, Some("branch ACME-7"))
    );
    let (kind, new_label, ctx): (String, String, String) = conn
        .query_row(
            "SELECT kind, new_label, ctx FROM corrections WHERE interval_id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        (kind.as_str(), new_label.as_str()),
        ("assign", "Sending plans")
    );
    assert!(ctx.contains("plans.rs"), "{ctx}");
    assert!(
        prepass::run(&mut conn, &config, ms_to_ts(1_700_000))
            .unwrap()
            .is_empty()
    );
    // Derive over the tail: the kept row survives, the model is clipped.
    conn.execute(
        "INSERT INTO batches (start_ts, end_ts, status) VALUES (600000, 1200000, 'running')",
        [],
    )
    .unwrap();
    let batch = conn.last_insert_rowid();
    storage::attach_tail_intervals(&conn).unwrap();
    let stored = storage::store_derivation(
        &mut conn,
        batch,
        &[TaskSlot::Existing(board)],
        &[NewInterval {
            slot: 0,
            start_ts: ms_to_ts(900_000),
            end_ts: ms_to_ts(1_200_000),
            confidence: 0.7,
        }],
    )
    .unwrap();
    assert_eq!(
        stored,
        vec![(board, 900_000, 1_000_000), (board, 1_100_000, 1_200_000)]
    );
    let feed = storage::feed_blocks(&conn, 0, 2_000_000, prepass::RUN_GAP_MS, 12).unwrap();
    let kept = feed.iter().find(|b| b.start_ts == 1_000_000).unwrap();
    assert_eq!(
        (
            kept.claim.as_ref().unwrap().task_id,
            kept.claim.as_ref().unwrap().source.as_str()
        ),
        (ticket, "user")
    );
    assert_eq!(feed.len(), 5);
}

#[test]
fn proposals_cluster_name_accept_and_dismiss() {
    use chronicle_core::proposals;
    use chronicle_core::storage;
    use chronicle_core::types::ms_to_ts;

    let db = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("m24_proposals.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(db.with_extension(format!("db{suffix}")));
    }
    let mut conn = storage::open(&db).unwrap();
    // "Claude" is in most titles of the day: not distinctive. A and B share
    // "roadmap"; C is a lone 12-minute tab; D is too short to propose; E
    // and F link by repo only.
    let spans = [
        (0, 60_000, "Terminator", "Claude Code — chronicle"),
        (600_000, 1_020_000, "Firefox", "Notion roadmap doc — Claude"),
        (1_500_000, 1_800_000, "Firefox", "Roadmap sync notes"),
        (2_400_000, 3_120_000, "Firefox", "YouTube — Claude"),
        (4_000_000, 4_060_000, "Zoom", "standup"),
        (5_000_000, 5_360_000, "Code", "lib.rs — Claude"),
        (6_000_000, 6_300_000, "Code", "main.rs"),
    ];
    for (s, e, app, title) in spans {
        conn.execute(
            "INSERT INTO spans (start_ts, end_ts, app, title, kind, batch_id) VALUES (?1, ?2, ?3, ?4, 'focus', NULL)",
            rusqlite::params![s, e, app, title],
        )
        .unwrap();
    }
    // The first span is already claimed.
    conn.execute(
        "INSERT INTO tasks (label, status, source, created_ts) VALUES ('Chronicle', 'open', 'user', 0)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence, source) VALUES (1, NULL, 0, 60000, 1.0, 'user')",
        [],
    )
    .unwrap();
    for (ts, end) in [(5_000_000, 5_360_000), (6_000_000, 6_300_000)] {
        storage::insert_activity_event(
            &conn,
            &chronicle_core::types::ActivityEvent {
                ts: ms_to_ts(ts),
                end_ts: Some(ms_to_ts(end)),
                repo: "pb".into(),
                branch: "main".into(),
                kind: chronicle_core::types::ActivityKind::AiSession,
                ext_id: None,
                summary: None,
            },
        )
        .unwrap();
    }

    let heads = proposals::refresh(&mut conn, ms_to_ts(7_000_000)).unwrap();
    assert_eq!(heads, vec![600_000, 2_400_000, 5_000_000]);
    let open = proposals::open_proposals(&conn, 0, 7_000_000).unwrap();
    type Row<'a> = (i64, i64, usize, Option<&'a str>, bool, &'a str);
    let summary: Vec<Row> = open
        .iter()
        .map(|p| {
            (
                p.start_ts,
                p.ms,
                p.runs.len(),
                p.project.as_deref(),
                p.naming,
                p.lines[0].1.as_str(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (5_000_000, 660_000, 2, Some("pb"), true, "lib.rs — Claude"),
            (2_400_000, 720_000, 1, None, true, "YouTube — Claude"),
            (
                600_000,
                720_000,
                2,
                None,
                true,
                "Notion roadmap doc — Claude"
            ),
        ]
    );
    // One naming job per proposal, bounded to the cluster.
    let jobs: Vec<(String, String)> = conn
        .prepare("SELECT kind, payload FROM ai_jobs ORDER BY id")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(jobs.len(), 3);
    assert_eq!(
        jobs[0],
        (
            "suggest_task".to_owned(),
            "{\"lo\":600000,\"hi\":1800000}".to_owned()
        )
    );
    // Same tick again: no new jobs, rows rewritten in place.
    proposals::refresh(&mut conn, ms_to_ts(7_100_000)).unwrap();
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM ai_jobs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 3);
    // The roadmap job lands: the name is copied in.
    conn.execute(
        "UPDATE ai_jobs SET status='done', result='{\"label\":\"Roadmap review\",\"project\":\"planning\",\"description\":\"Reading the roadmap.\"}' WHERE id=1",
        [],
    )
    .unwrap();
    proposals::refresh(&mut conn, ms_to_ts(7_200_000)).unwrap();
    let open = proposals::open_proposals(&conn, 0, 7_000_000).unwrap();
    let roadmap = open.iter().find(|p| p.start_ts == 600_000).unwrap();
    assert_eq!(
        (
            roadmap.label.as_deref(),
            roadmap.project.as_deref(),
            roadmap.naming
        ),
        (Some("Roadmap review"), Some("planning"), false)
    );
    // Dismiss YouTube: it stays out on later ticks.
    let yt = open.iter().find(|p| p.start_ts == 2_400_000).unwrap().id;
    proposals::dismiss(&conn, yt).unwrap();
    proposals::refresh(&mut conn, ms_to_ts(7_300_000)).unwrap();
    let open = proposals::open_proposals(&conn, 0, 7_000_000).unwrap();
    assert_eq!(open.len(), 2);
    assert!(open.iter().all(|p| p.start_ts != 2_400_000));
    // Accept the roadmap: a declared task with the description, both runs
    // claimed, and the proposal leaves the feed.
    let (task_id, claimed) =
        proposals::accept(&mut conn, ms_to_ts(7_400_000), roadmap.id, "Roadmap review").unwrap();
    assert_eq!(claimed, 720_000);
    let (label, project, source, description): (String, Option<String>, String, Option<String>) =
        conn.query_row(
            "SELECT label, project, source, description FROM tasks WHERE id=?1",
            [task_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        (
            label.as_str(),
            project.as_deref(),
            source.as_str(),
            description.as_deref()
        ),
        (
            "Roadmap review",
            Some("planning"),
            "user",
            Some("Reading the roadmap.")
        )
    );
    proposals::refresh(&mut conn, ms_to_ts(7_500_000)).unwrap();
    let open = proposals::open_proposals(&conn, 0, 7_000_000).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].start_ts, 5_000_000);
    // A pre-pass claim on a cluster's head drops its row; what is left of
    // the cluster (5 min) is under the threshold, so nothing replaces it.
    conn.execute(
        "INSERT INTO intervals (task_id, batch_id, start_ts, end_ts, confidence, source, reason) VALUES (1, NULL, 5000000, 5360000, 0.5, 'prepass', 'repo pb')",
        [],
    )
    .unwrap();
    proposals::refresh(&mut conn, ms_to_ts(7_600_000)).unwrap();
    let open = proposals::open_proposals(&conn, 0, 7_000_000).unwrap();
    assert!(open.is_empty(), "{open:?}");
}
