use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, bail};
use chronicle_core::config::Config;
use jiff::{Timestamp, civil, tz::TimeZone};

use crate::derive::{AFK_SPLIT_MS, COALESCE_GAP_MIN, OPEN_CAP, afk_gaps_min, build_batch_digest};
/// M4 benchmark gate: run every downloaded preset over fixture streams and
/// real batches, print tasks + timing side by side. Fixtures with a
/// `<name>.expect.json` are scored deterministically (post-merge output);
/// judgment on the rest stays human.
pub(crate) fn bench(
    data_dir: &Path,
    fixtures: &Path,
    batch_ids: &[i64],
    digest_only: bool,
    only: Option<&str>,
    model_filter: Option<&str>,
    no_mcp: bool,
) -> anyhow::Result<()> {
    use chronicle_core::eval::Expectations;
    use chronicle_core::types::{Event, OpenTask};
    use chronicle_core::{digest, sessionizer, storage};

    let config = Config::load(&data_dir.join("config.toml"))?;
    // (name, digest, open tasks, expectations, AFK gaps ≥ 5 min in window minutes)
    type Case = (
        String,
        String,
        Vec<OpenTask>,
        Option<Expectations>,
        Vec<(i64, i64)>,
    );
    let mut cases: Vec<Case> = Vec::new();

    if fixtures.is_dir() {
        let mut paths: Vec<_> = std::fs::read_dir(fixtures)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
            .collect();
        paths.sort();
        for path in paths {
            let text = std::fs::read_to_string(&path)?;
            let events = text
                .lines()
                .filter(|l| !l.trim().is_empty())
                .map(serde_json::from_str::<Event>)
                .collect::<Result<Vec<_>, _>>()
                .with_context(|| format!("parsing {}", path.display()))?;
            let Some(end) = events.last().map(|e| e.ts) else {
                continue;
            };
            let spans = sessionizer::sessionize(&events, end, &config);
            let name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let expect_path = path.with_extension("expect.json");
            let expect: Option<Expectations> = match std::fs::read_to_string(&expect_path) {
                Ok(text) => Some(
                    serde_json::from_str(&text)
                        .with_context(|| format!("parsing {}", expect_path.display()))?,
                ),
                Err(_) => None,
            };
            let open = expect
                .as_ref()
                .map(|e| e.open_task_list())
                .unwrap_or_default();
            let origin = spans.first().map_or(0, |s| s.start.as_millisecond());
            let gaps = afk_gaps_min(&spans, origin);
            cases.push((
                format!("fixture:{name}"),
                digest::build_digest(
                    &spans,
                    &jiff::tz::TimeZone::UTC,
                    &open,
                    &[],
                    &[],
                    &[],
                    None,
                    None,
                    None,
                ),
                open,
                expect,
                gaps,
            ));
        }
    }

    if !batch_ids.is_empty() {
        let conn = storage::open(&data_dir.join("chronicle.db"))?;
        for &id in batch_ids {
            let spans = storage::batch_spans(&conn, id)?;
            if spans.is_empty() {
                println!("batch {id}: no spans, skipping");
                continue;
            }
            let Some(batch) = storage::batch_row(&conn, id)? else {
                println!("batch {id}: no such batch, skipping");
                continue;
            };
            let open = storage::open_tasks(&conn, OPEN_CAP)?;
            let bd = build_batch_digest(&conn, &config, data_dir, &batch, open, !no_mcp)?;
            cases.push((format!("batch:{id}"), bd.digest, bd.open, None, bd.gaps));
        }
    }
    if let Some(filter) = only {
        cases.retain(|(name, ..)| name.contains(filter));
    }
    if cases.is_empty() {
        bail!("nothing to bench: no matching fixtures and no --batch given");
    }
    if digest_only {
        for (case, digest_text, ..) in &cases {
            println!(
                "\n=== {case} (digest ~{} tokens)\n{digest_text}",
                digest::approx_tokens(digest_text)
            );
        }
        return Ok(());
    }

    let models = bench_models(data_dir, model_filter)?;

    for (name, path) in &models {
        let model = chronicle_derive::DeriveModel::load(path)?;
        let mut session = model.session()?;
        for (case, digest_text, open, expect, gaps) in &cases {
            println!(
                "\n=== {case} [{name}] (digest ~{} tokens)",
                digest::approx_tokens(digest_text)
            );
            let t0 = Instant::now();
            match session.infer(digest_text, &mut |_| {}) {
                Ok(run) => {
                    let drafts =
                        chronicle_core::merge::sanitize_intervals(run.intervals, open.len());
                    let (slots, linked) = chronicle_core::merge::link_intervals(&drafts, open);
                    let linked = chronicle_core::merge::coalesce(linked, gaps, COALESCE_GAP_MIN);
                    let resolved = chronicle_core::eval::resolve(&slots, &linked, open);
                    println!(
                        "--- {name}: {} intervals over {} tasks in {:.1}s (linked; {} prompt tokens, {} cached)",
                        resolved.len(),
                        slots.len(),
                        t0.elapsed().as_secs_f64(),
                        run.prompt_tokens,
                        run.cached_prefix_tokens
                    );
                    for t in &resolved {
                        let project = t.project.as_deref().unwrap_or("-");
                        println!(
                            "  {:>4}–{:<4} {:.2}  {}  [{project}]",
                            t.start_offset_min, t.end_offset_min, t.confidence, t.label
                        );
                    }
                    if let Some(exp) = expect {
                        let report = chronicle_core::eval::score(&resolved, exp);
                        for c in &report.checks {
                            let verdict = if c.pass { "PASS" } else { "FAIL" };
                            println!("  [{verdict}] {}: {}", c.name, c.detail);
                        }
                        println!("  score: {}", report.summary());
                    }
                }
                Err(e) => println!(
                    "--- {name}: FAILED in {:.1}s: {e:#}",
                    t0.elapsed().as_secs_f64()
                ),
            }
        }
    }
    Ok(())
}

/// Ephemeral derivation worker: claim batch → digest → infer → write tasks →
/// exit. Any failure marks the batch failed (retry-once via attempts cap).
/// Downloaded model presets, optionally filtered by name substring.
pub(crate) fn bench_models(
    data_dir: &Path,
    model_filter: Option<&str>,
) -> anyhow::Result<Vec<(&'static str, PathBuf)>> {
    let models: Vec<_> = chronicle_derive::model::PRESETS
        .iter()
        .map(|p| {
            (
                p.name,
                chronicle_derive::model::models_dir(data_dir).join(p.file),
            )
        })
        .filter(|(name, path)| path.exists() && model_filter.is_none_or(|f| name.contains(f)))
        .collect();
    if models.is_empty() {
        bail!("no matching models downloaded; run `chronicle model pull`");
    }
    Ok(models)
}

/// `chronicle bench --replay`: re-derive every done batch a recent correction
/// touched, with the open-task list as it stood at the batch's end, and score
/// whether the corrected outcome comes out (m27 chunk 2). No MCP context: it
/// is live data and would make runs incomparable.
pub(crate) fn replay_eval(
    data_dir: &Path,
    since_days: u64,
    model_filter: Option<&str>,
    out: Option<&Path>,
) -> anyhow::Result<()> {
    use chronicle_core::replay::{self, Check};
    use chronicle_core::types::TaskSlot;
    use chronicle_core::{digest, merge, storage};

    let config = Config::load(&data_dir.join("config.toml"))?;
    let conn = storage::open(&data_dir.join("chronicle.db"))?;
    let since_ms = Timestamp::now().as_millisecond() - since_days as i64 * 86_400_000;
    let rows = storage::replay_rows(&conn, since_ms)?;
    let view = replay::Rows {
        corrections: &rows.corrections,
        intervals: &rows.intervals,
        tasks: &rows.tasks,
        batches: &rows.batches,
    };
    let (probes, skipped) = replay::build_probes(&view);
    for s in &skipped {
        println!("skip {s}");
    }
    if probes.is_empty() {
        bail!("no scorable corrections in the last {since_days} days");
    }
    let mut batch_ids: Vec<i64> = probes.iter().map(|p| p.batch_id).collect();
    batch_ids.sort_unstable();
    batch_ids.dedup();
    let corrections: std::collections::BTreeSet<i64> =
        probes.iter().map(|p| p.correction_id).collect();
    println!(
        "{} probes from {} corrections over {} batches ({} corrections skipped)",
        probes.len(),
        corrections.len(),
        batch_ids.len(),
        skipped.len()
    );
    let models = bench_models(data_dir, model_filter)?;

    let mut report = Vec::new();
    for (name, path) in &models {
        let model = chronicle_derive::DeriveModel::load(path)?;
        let mut session = model.session()?;
        let mut results: Vec<replay::ProbeResult> = Vec::new();
        for &bid in &batch_ids {
            let batch = storage::batch_row(&conn, bid)?
                .with_context(|| format!("batch {bid} vanished mid-replay"))?;
            let open_at = replay::open_tasks_at(
                &rows.tasks,
                &rows.intervals,
                &rows.corrections,
                batch.end_ts,
                8,
            );
            let bd = build_batch_digest(&conn, &config, data_dir, &batch, open_at.clone(), false)?;
            let bprobes: Vec<&replay::Probe> =
                probes.iter().filter(|p| p.batch_id == bid).collect();
            println!(
                "\n=== batch:{bid} [{name}] ({} probes, {} open tasks then, digest ~{} tokens)",
                bprobes.len(),
                open_at.len(),
                digest::approx_tokens(&bd.digest)
            );
            let t0 = Instant::now();
            match session.infer(&bd.digest, &mut |_| {}) {
                Ok(run) => {
                    let cached = run.cached_prefix_tokens;
                    let drafts = merge::sanitize_intervals(run.intervals, bd.open.len());
                    let (slots, linked) = merge::link_intervals(&drafts, &bd.open);
                    let linked = merge::coalesce(linked, &bd.gaps, COALESCE_GAP_MIN);
                    let replayed: Vec<replay::Replayed> = linked
                        .iter()
                        .filter_map(|iv| {
                            let (task_id, label) = match slots.get(iv.slot)? {
                                TaskSlot::Existing(id) => (
                                    Some(*id),
                                    bd.open
                                        .iter()
                                        .find(|t| t.id == *id)
                                        .map(|t| t.label.clone())
                                        .unwrap_or_default(),
                                ),
                                TaskSlot::New { label, .. } => (None, label.clone()),
                            };
                            Some(replay::Replayed {
                                task_id,
                                label,
                                start_offset_min: iv.start_offset_min,
                                end_offset_min: iv.end_offset_min,
                            })
                        })
                        .collect();
                    println!(
                        "--- {} intervals in {:.1}s ({cached} cached prefix tokens)",
                        replayed.len(),
                        t0.elapsed().as_secs_f64()
                    );
                    for r in &replayed {
                        let how = match r.task_id {
                            Some(id) => format!("#{id}"),
                            None => "new".into(),
                        };
                        println!(
                            "  {:>4}–{:<4} {how}  {}",
                            r.start_offset_min, r.end_offset_min, r.label
                        );
                    }
                    for p in bprobes {
                        let r = replay::score(p, batch.start_ts, &replayed, &open_at);
                        let verdict = if r.pass { "PASS" } else { "FAIL" };
                        println!(
                            "  [{verdict}] c{} {} {}: {}",
                            r.correction_id,
                            r.kind,
                            r.check.name(),
                            r.detail
                        );
                        results.push(r);
                    }
                }
                Err(e) => {
                    println!("--- FAILED in {:.1}s: {e:#}", t0.elapsed().as_secs_f64());
                    for p in bprobes {
                        results.push(replay::ProbeResult {
                            correction_id: p.correction_id,
                            batch_id: p.batch_id,
                            kind: p.kind.clone(),
                            check: p.check,
                            pass: false,
                            detail: format!("derive failed: {e:#}"),
                        });
                    }
                }
            }
        }
        let mut totals = serde_json::Map::new();
        println!("\n=== {name} replay score");
        let mut ok_all = 0;
        for check in [Check::Placed, Check::Label, Check::NotEjected] {
            let n = results.iter().filter(|r| r.check == check).count();
            let ok = results
                .iter()
                .filter(|r| r.check == check && r.pass)
                .count();
            ok_all += ok;
            if n > 0 {
                println!("  {}: {ok}/{n}", check.name());
            }
            totals.insert(check.name().into(), serde_json::json!([ok, n]));
        }
        println!("  total: {ok_all}/{}", results.len());
        totals.insert("total".into(), serde_json::json!([ok_all, results.len()]));
        report.push(serde_json::json!({
            "model": name,
            "since_days": since_days,
            "generated_ts": Timestamp::now().as_millisecond(),
            "skipped": skipped,
            "totals": totals,
            "probes": results,
        }));
    }
    if let Some(out) = out {
        std::fs::write(out, serde_json::to_string_pretty(&report)?)
            .with_context(|| format!("writing {}", out.display()))?;
        println!("wrote {}", out.display());
    }
    Ok(())
}

/// One-shot in-process backfill (loads the model once, like `bench`); an
/// occasional operator command, not routed through the daemon queue where 50
/// jobs would starve derivation. Stop the daemon's unit first if it might
/// derive concurrently — two llama processes fight for the same cores.
pub(crate) fn backfill_descriptions(data_dir: &Path, limit: usize) -> anyhow::Result<()> {
    use chronicle_core::storage;
    use std::io::Write as _;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let conn = storage::open(&data_dir.join("chronicle.db"))?;
    let Some(model_path) = chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir)
    else {
        bail!("no model available; run `chronicle model pull`")
    };
    let tasks = storage::closed_tasks_missing_description(&conn, limit)?;
    if tasks.is_empty() {
        println!("nothing to backfill");
        return Ok(());
    }
    let describer = chronicle_derive::describe::Describer::load(&model_path)?;
    let total = tasks.len();
    let mut done = 0usize;
    let mut skipped = 0usize;
    for (i, t) in tasks.iter().enumerate() {
        print!("\x1b[2K\r{}/{total} {:.40}", i + 1, t.label);
        let _ = std::io::stdout().flush();
        let evidence = storage::task_evidence_text(&conn, t.id)?;
        if evidence.trim().is_empty() {
            skipped += 1;
            continue;
        }
        match describer.describe_task(&t.label, t.project.as_deref(), &evidence) {
            Ok(desc) => {
                storage::set_task_description(&conn, t.id, Some(&desc))?;
                done += 1;
            }
            Err(e) => {
                skipped += 1;
                eprintln!("\ntask {} failed: {e:#}", t.id);
            }
        }
    }
    println!("\x1b[2K\rdescribed {done}/{total} closed tasks ({skipped} skipped)");
    Ok(())
}

/// One-off after the m27 chunk 1 deploy: apply `merge::coalesce` to stored
/// derived rows, batch by batch, in ms. AFK gaps come from the batch's spans;
/// the user's own rows and rows a correction points at block a join the
/// same way (and the latter stay put: `corrections.interval_id` is a
/// foreign key). Take a `.bak` of the DB
/// first; the daemon may run alongside (each batch is one transaction and a
/// re-derivation replaces the batch's rows anyway).
pub(crate) fn backfill_coalesce(data_dir: &Path, since: &str, dry_run: bool) -> anyhow::Result<()> {
    use chronicle_core::merge::{LinkedInterval, coalesce};
    use chronicle_core::storage::{self, StoredInterval};
    let tz = TimeZone::system();
    let day: civil::Date = since
        .parse()
        .with_context(|| format!("bad date {since:?}"))?;
    let since_ms = day.to_zoned(tz)?.timestamp().as_millisecond();
    let mut conn = storage::open(&data_dir.join("chronicle.db"))?;
    let batches = storage::derived_rows_by_batch(&conn, since_ms)?;
    let (mut before, mut after, mut changed) = (0usize, 0usize, 0usize);
    for (batch_id, lo, hi, rows) in batches {
        let spans = storage::batch_spans(&conn, batch_id)?;
        let mut blocks: Vec<(i64, i64)> = spans
            .iter()
            .filter(|s| {
                s.kind == chronicle_core::sessionizer::SpanKind::Afk
                    && s.duration_ms() >= AFK_SPLIT_MS
            })
            .map(|s| (s.start.as_millisecond(), s.end.as_millisecond()))
            .collect();
        blocks.extend(storage::user_ranges_in(&conn, lo, hi)?);
        let (pinned, rows): (Vec<_>, Vec<_>) = rows.into_iter().partition(|r| r.pinned);
        blocks.extend(pinned.iter().map(|r| (r.start_ts, r.end_ts)));
        let linked = rows
            .iter()
            .map(|r| LinkedInterval {
                slot: r.task_id as usize,
                start_offset_min: r.start_ts,
                end_offset_min: r.end_ts,
                confidence: r.confidence,
            })
            .collect();
        let out = coalesce(linked, &blocks, COALESCE_GAP_MIN * 60_000);
        before += rows.len();
        after += out.len();
        if out.len() == rows.len() {
            continue;
        }
        changed += 1;
        println!("batch {batch_id}: {} → {} rows", rows.len(), out.len());
        if dry_run {
            continue;
        }
        let rows: Vec<StoredInterval> = out
            .into_iter()
            .map(|iv| StoredInterval {
                task_id: iv.slot as i64,
                start_ts: iv.start_offset_min,
                end_ts: iv.end_offset_min,
                confidence: iv.confidence,
                pinned: false,
            })
            .collect();
        storage::replace_derived_rows(&mut conn, batch_id, &rows)?;
    }
    println!(
        "{}{changed} batches changed, {before} → {after} derived rows",
        if dry_run { "dry run: " } else { "" }
    );
    Ok(())
}

/// `chronicle backfill-anchors`: recompute anchors for every focus span
/// since `since` (a local day) or all of them.
pub(crate) fn backfill_anchors(data_dir: &Path, since: Option<&str>) -> anyhow::Result<()> {
    use chronicle_core::storage;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let re = regex::Regex::new(&config.ticket_regex).context("ticket_regex")?;
    let lo = match since {
        Some(day) => {
            let day: civil::Date = day.parse().with_context(|| format!("bad date {day:?}"))?;
            day.to_zoned(TimeZone::system())?
                .timestamp()
                .as_millisecond()
        }
        None => 0,
    };
    let mut conn = storage::open(&data_dir.join("chronicle.db"))?;
    let n = storage::anchor_spans(&mut conn, lo, i64::MAX, &re)?;
    println!("anchored {n} spans");
    Ok(())
}

/// `chronicle anchors`: coverage of focus time by anchor strength and the
/// values that cover the most time.
pub(crate) fn anchor_report(data_dir: &Path, days: u32, top: usize) -> anyhow::Result<()> {
    use chronicle_core::storage;
    let conn = storage::open(&data_dir.join("chronicle.db"))?;
    let hi = Timestamp::now().as_millisecond();
    let lo = hi - i64::from(days) * 86_400_000;
    let cov = storage::anchor_coverage(&conn, lo, hi, top)?;
    let pct = |ms: i64| {
        if cov.focus_ms == 0 {
            0.0
        } else {
            100.0 * ms as f64 / cov.focus_ms as f64
        }
    };
    let none = cov.focus_ms - cov.strong_ms - cov.medium_ms - cov.weak_ms;
    println!(
        "focus {:>7.1} min over {days} days",
        cov.focus_ms as f64 / 60_000.0
    );
    println!(
        "strong {:>6.1}%  medium {:>6.1}%  weak {:>6.1}%  none {:>6.1}%",
        pct(cov.strong_ms),
        pct(cov.medium_ms),
        pct(cov.weak_ms),
        pct(none)
    );
    for (kind, value, ms) in &cov.top {
        println!("{:>7.1} min  {kind:<8} {value}", *ms as f64 / 60_000.0);
    }
    Ok(())
}
