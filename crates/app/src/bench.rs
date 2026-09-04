use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{Context, bail};
use chronicle_core::config::Config;
use jiff::{Timestamp, civil, tz::TimeZone};

use crate::derive::{AFK_SPLIT_MS, COALESCE_GAP_MIN, OPEN_CAP, afk_gaps_min, build_batch_digest};

// (name, digest, open tasks, expectations, AFK gaps ≥ 5 min in window
// minutes, sessionized spans — the last only needed by --scorer)
type Case = (
    String,
    String,
    Vec<chronicle_core::types::OpenTask>,
    Option<chronicle_core::eval::Expectations>,
    Vec<(i64, i64)>,
    Vec<chronicle_core::sessionizer::SpanDraft>,
);

/// M4 benchmark gate: run every downloaded preset over fixture streams and
/// real batches, print tasks + timing side by side. Fixtures with a
/// `<name>.expect.json` are scored deterministically (post-merge output);
/// judgment on the rest stays human.
#[allow(clippy::too_many_arguments)]
pub(crate) fn bench(
    data_dir: &Path,
    fixtures: &Path,
    batch_ids: &[i64],
    digest_only: bool,
    only: Option<&str>,
    model_filter: Option<&str>,
    no_mcp: bool,
    scorer: bool,
) -> anyhow::Result<()> {
    use chronicle_core::eval::Expectations;
    use chronicle_core::types::Event;
    use chronicle_core::{digest, sessionizer, storage};

    let config = Config::load(&data_dir.join("config.toml"))?;
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
                spans,
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
            cases.push((
                format!("batch:{id}"),
                bd.digest,
                bd.open,
                None,
                bd.gaps,
                spans,
            ));
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
    if scorer {
        return scorer_fixture_eval(&cases, &config);
    }

    let models = bench_models(data_dir, model_filter)?;

    for (name, path) in &models {
        let model = chronicle_derive::DeriveModel::load(path)?;
        let mut session = model.session()?;
        for (case, digest_text, open, expect, gaps, _spans) in &cases {
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

/// `chronicle bench --scorer` (fixture mode, m30 chunk 2): score the m30
/// evidence profiler against persona fixtures whose `.expect.json` carries
/// `groups` — no model load. Each fixture's groups become synthetic tasks
/// and their ranges are walked in time order, teaching the profiler the
/// ground truth after every range (as a user confirmation would): a range
/// on a group not taught yet must score "new" (`best: None`), one on a
/// group already taught must resolve back to that same task.
// (group index, minute range, ms range)
type FlatRange = (usize, (i64, i64), (i64, i64));

fn scorer_fixture_eval(cases: &[Case], config: &Config) -> anyhow::Result<()> {
    use chronicle_core::extract;
    use chronicle_core::profile::{self, Params, Segment};
    use chronicle_core::replay::{IntervalRow, TaskRow};
    use chronicle_core::sessionizer::SpanKind;
    use std::collections::HashSet;

    let re = regex::Regex::new(&config.ticket_regex).context("ticket_regex")?;
    let distractions = chronicle_core::evidence::compile_patterns(&config.distraction_patterns);
    let params = Params::default();
    let (mut fixtures_ok, mut fixtures_n) = (0usize, 0usize);

    for (case, _digest, _open, expect, _gaps, spans) in cases {
        let Some(exp) = expect else { continue };
        if exp.groups.is_empty() {
            continue;
        }
        println!("\n=== {case} [scorer] ({} groups)", exp.groups.len());
        // Ranges in `.expect.json` are minute offsets from the window start
        // (end-exclusive; see bench()'s fixture loop, which uses the same
        // origin for `afk_gaps_min`).
        let origin = spans.first().map_or(0, |s| s.start.as_millisecond());

        let mut aspans: Vec<profile::AnchoredSpan> = spans
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == SpanKind::Focus)
            .map(|(i, s)| profile::AnchoredSpan {
                id: i as i64,
                start_ts: s.start.as_millisecond(),
                end_ts: s.end.as_millisecond(),
                app: s.app.clone(),
                title: s.title.clone(),
                anchors: extract::extract(&s.app, &s.title, s.url.as_deref(), &re),
            })
            .collect();
        aspans.sort_by_key(|s| s.start_ts);

        let tasks: Vec<TaskRow> = exp
            .groups
            .iter()
            .enumerate()
            .map(|(gi, g)| {
                let earliest = g.ranges.iter().map(|r| r.0).min().unwrap_or(0);
                TaskRow {
                    id: (gi + 1) as i64,
                    label: g.name.clone(),
                    project: g.project.clone(),
                    declared: false,
                    closed: false,
                    created_ts: origin + earliest * 60_000,
                    closed_ts: None,
                }
            })
            .collect();

        // Flatten and walk in time order.
        let mut flat: Vec<FlatRange> = exp
            .groups
            .iter()
            .enumerate()
            .flat_map(|(gi, g)| {
                g.ranges
                    .iter()
                    .map(move |&r| (gi, r, (origin + r.0 * 60_000, origin + r.1 * 60_000)))
            })
            .collect();
        flat.sort_by_key(|(_, _, ms)| ms.0);

        let mut seen_intervals: Vec<IntervalRow> = Vec::new();
        let mut seen_groups: HashSet<usize> = HashSet::new();
        let (mut ok, mut n) = (0usize, 0usize);
        let (mut ok_c, mut n_c) = (0usize, 0usize);
        for (idx, (gi, min_range, ms_range)) in flat.iter().enumerate() {
            let task_id = (*gi + 1) as i64;
            let profiles = profile::build_profiles(
                &tasks,
                &seen_intervals,
                &aspans,
                &[],
                &re,
                ms_range.0,
                &params,
            );
            let seg = Segment::from_spans_skipping(&aspans, ms_range.0, ms_range.1, &distractions);
            let v = profile::score(&seg, &profiles, &params);
            let expected = seen_groups.contains(gi).then_some(task_id);
            let pass = v.best == expected;
            n += 1;
            if pass {
                ok += 1;
            }
            if v.confident {
                n_c += 1;
                if pass {
                    ok_c += 1;
                }
            }
            let verdict = if pass { "PASS" } else { "FAIL" };
            let show = |t: Option<i64>| t.map_or("new".to_string(), |id| id.to_string());
            println!(
                "  [{verdict}] {case} {} {:02}:{:02}\u{2013}{:02}:{:02}: best={} expected={} margin={:.2} {}",
                exp.groups[*gi].name,
                min_range.0 / 60,
                min_range.0 % 60,
                min_range.1 / 60,
                min_range.1 % 60,
                show(v.best),
                show(expected),
                v.margin,
                if v.confident { "confident" } else { "unsure" },
            );
            seen_intervals.push(IntervalRow {
                id: idx as i64,
                task_id,
                batch_id: None,
                start_ts: ms_range.0,
                end_ts: ms_range.1,
            });
            seen_groups.insert(*gi);
        }
        println!("  {case} scorer: {ok}/{n} (confident {ok_c}/{n_c})");
        fixtures_ok += ok;
        fixtures_n += n;
    }
    println!("\nscorer fixtures: {fixtures_ok}/{fixtures_n}");
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

/// Print per-check and total pass counts (strict, then lenient) under a
/// heading; return them as the JSON `totals` object.
fn print_totals(
    heading: &str,
    results: &[chronicle_core::replay::ProbeResult],
) -> serde_json::Map<String, serde_json::Value> {
    use chronicle_core::replay::Check;
    let mut totals = serde_json::Map::new();
    println!("\n=== {heading}");
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
    let kinds: Vec<String> = ["assign", "reassign", "merge", "rename", "eject"]
        .iter()
        .filter_map(|k| {
            let n = results.iter().filter(|r| r.kind == *k).count();
            let ok = results.iter().filter(|r| r.kind == *k && r.pass).count();
            (n > 0).then(|| format!("{k} {ok}/{n}"))
        })
        .collect();
    println!("  by kind: {}", kinds.join(", "));
    let lenient = results.iter().filter(|r| r.lenient).count();
    println!(
        "  total: {ok_all}/{} (lenient {lenient}/{})",
        results.len(),
        results.len()
    );
    totals.insert("total".into(), serde_json::json!([ok_all, results.len()]));
    totals.insert(
        "lenient".into(),
        serde_json::json!([lenient, results.len()]),
    );
    totals
}

/// `chronicle bench --replay`: re-derive every done batch a recent correction
/// touched, with the open-task list as it stood at the batch's end, and score
/// whether the corrected outcome comes out (m27 chunk 2). No MCP context: it
/// is live data and would make runs incomparable.
pub(crate) fn replay_eval(
    data_dir: &Path,
    since_days: u64,
    model_filter: Option<&str>,
    scorer: bool,
    out: Option<&Path>,
) -> anyhow::Result<()> {
    use chronicle_core::profile::{self, Params, Segment};
    use chronicle_core::replay::{self, Replayed};
    use chronicle_core::types::TaskSlot;
    use chronicle_core::{digest, merge, storage};
    use std::collections::HashMap;

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

    // The scorer pass never loads a model: run it up front (batches share
    // `open_tasks_at` with the model path below) and keep, per probe, both
    // its verdict (for the calibration block and `--out`) and its
    // `Replayed` (reused by the combined pass once models have run).
    let mut scorer_results: Vec<replay::ProbeResult> = Vec::new();
    let mut scorer_verdicts: Vec<(Option<i64>, f64, bool)> = Vec::new();
    let mut scorer_by_probe: HashMap<(i64, i64), (bool, Replayed)> = HashMap::new();
    let mut scorer_json: Option<serde_json::Value> = None;
    if scorer {
        let spans = storage::anchored_spans(&conn, 0, i64::MAX)?;
        let params = Params::default();
        let re = regex::Regex::new(&config.ticket_regex).context("ticket_regex")?;
        let distractions = chronicle_core::evidence::compile_patterns(&config.distraction_patterns);
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
            let profiles = profile::build_profiles(
                &rows.tasks,
                &rows.intervals,
                &spans,
                &rows.corrections,
                &re,
                batch.start_ts,
                &params,
            );
            for p in probes.iter().filter(|p| p.batch_id == bid) {
                let seg = Segment::from_spans_skipping(&spans, p.range.0, p.range.1, &distractions);
                let v = profile::score(&seg, &profiles, &params);
                let label = match v.best {
                    Some(id) => rows
                        .tasks
                        .iter()
                        .find(|t| t.id == id)
                        .map(|t| replay::label_at(t, batch.end_ts, &rows.corrections).0)
                        .unwrap_or_default(),
                    None => seg.describe(3),
                };
                let replayed = Replayed {
                    task_id: v.best,
                    label,
                    start_offset_min: (p.range.0 - batch.start_ts) / 60_000,
                    end_offset_min: (p.range.1 - batch.start_ts + 59_999) / 60_000,
                };
                let r = replay::score(p, batch.start_ts, std::slice::from_ref(&replayed), &open_at);
                let verdict = if r.pass { "PASS" } else { "FAIL" };
                println!(
                    "[{verdict}] scorer c{} {} {}: best={} margin={:.2} {} — {}",
                    r.correction_id,
                    r.kind,
                    r.check.name(),
                    v.best.map_or("new".to_string(), |id| id.to_string()),
                    v.margin,
                    if v.confident { "confident" } else { "unsure" },
                    r.detail
                );
                if !r.pass && std::env::var_os("CHRONICLE_SCORER_DEBUG").is_some() {
                    let rank = v.ranked.iter().position(|c| c.task_id == p.task_id);
                    let wanted = profiles.iter().find(|pr| pr.task_id == p.task_id);
                    let mut keys: Vec<(&profile::Key, f64)> =
                        seg.keys.iter().map(|(k, m)| (k, *m)).collect();
                    keys.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
                    let seg_keys: Vec<String> = keys
                        .iter()
                        .filter(|(k, _)| !k.is_term())
                        .take(8)
                        .map(|(k, m)| {
                            let sat = wanted.map_or(-1.0, |w| w.sat(k, &params));
                            format!("{}={} {:.0}m sat{:.1}", k.kind_str(), k.value(), m, sat)
                        })
                        .collect();
                    let top: Vec<String> = v
                        .ranked
                        .iter()
                        .take(3)
                        .map(|c| format!("{}:{:.2}", c.task_id, c.score))
                        .collect();
                    println!(
                        "      wanted={} rank={} profile={} seg={:.0}m top=[{}] keys=[{}]",
                        p.task_id,
                        rank.map_or("-".into(), |r| (r + 1).to_string()),
                        wanted.map_or("none".into(), |w| format!("{}keys", w.minutes.len())),
                        seg.minutes,
                        top.join(" "),
                        seg_keys.join("; ")
                    );
                }
                scorer_by_probe.insert((p.correction_id, p.batch_id), (v.confident, replayed));
                scorer_verdicts.push((v.best, v.margin, v.confident));
                scorer_results.push(r);
            }
        }
        let mut scorer_totals = print_totals("scorer replay score", &scorer_results);
        let n_conf = scorer_verdicts.iter().filter(|(.., c)| *c).count();
        let ok_conf = scorer_results
            .iter()
            .zip(&scorer_verdicts)
            .filter(|(_, (.., c))| *c)
            .filter(|(r, _)| r.pass)
            .count();
        let n_uns = scorer_verdicts.len() - n_conf;
        let ok_uns = scorer_results
            .iter()
            .zip(&scorer_verdicts)
            .filter(|(_, (.., c))| !*c)
            .filter(|(r, _)| r.pass)
            .count();
        println!("  confident: {n_conf} (pass {ok_conf}/{n_conf})");
        println!("  unsure: {n_uns} (pass {ok_uns}/{n_uns})");
        let new_verdicts = scorer_verdicts.iter().filter(|(b, ..)| b.is_none()).count();
        println!("  new-task verdicts: {new_verdicts}");
        scorer_totals.insert("confident".into(), serde_json::json!([ok_conf, n_conf]));
        scorer_totals.insert("unsure".into(), serde_json::json!([ok_uns, n_uns]));
        scorer_totals.insert("new_task_verdicts".into(), serde_json::json!(new_verdicts));
        scorer_json = Some(serde_json::json!({
            "totals": scorer_totals,
            "probes": scorer_results,
        }));
    }

    // `--scorer` without an explicit `--model` skips model loading entirely
    // (the fast path the scorer pass above is for); with `--model` given, or
    // without `--scorer` at all, behave as before.
    let models: Vec<(&'static str, PathBuf)> = if scorer && model_filter.is_none() {
        Vec::new()
    } else {
        bench_models(data_dir, model_filter)?
    };
    let combine = scorer && !models.is_empty();

    let mut report = Vec::new();
    let mut combined_by_model = serde_json::Map::new();
    for (name, path) in &models {
        let model = chronicle_derive::DeriveModel::load(path)?;
        let mut session = model.session()?;
        let mut results: Vec<replay::ProbeResult> = Vec::new();
        let mut combined_results: Vec<replay::ProbeResult> = Vec::new();
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
                        if combine {
                            let cr = match scorer_by_probe.get(&(p.correction_id, p.batch_id)) {
                                Some((true, sr)) => replay::score(
                                    p,
                                    batch.start_ts,
                                    std::slice::from_ref(sr),
                                    &open_at,
                                ),
                                _ => r.clone(),
                            };
                            combined_results.push(cr);
                        }
                        results.push(r);
                    }
                }
                Err(e) => {
                    println!("--- FAILED in {:.1}s: {e:#}", t0.elapsed().as_secs_f64());
                    for p in bprobes {
                        let fail = replay::ProbeResult {
                            correction_id: p.correction_id,
                            batch_id: p.batch_id,
                            kind: p.kind.clone(),
                            check: p.check,
                            pass: false,
                            lenient: false,
                            detail: format!("derive failed: {e:#}"),
                        };
                        if combine {
                            let cr = match scorer_by_probe.get(&(p.correction_id, p.batch_id)) {
                                Some((true, sr)) => replay::score(
                                    p,
                                    batch.start_ts,
                                    std::slice::from_ref(sr),
                                    &open_at,
                                ),
                                _ => fail.clone(),
                            };
                            combined_results.push(cr);
                        }
                        results.push(fail);
                    }
                }
            }
        }
        let totals = print_totals(&format!("{name} replay score"), &results);
        report.push(serde_json::json!({
            "model": name,
            "since_days": since_days,
            "generated_ts": Timestamp::now().as_millisecond(),
            "skipped": skipped,
            "totals": totals,
            "probes": results,
        }));

        if combine {
            let combined_totals = print_totals(
                &format!("combined (scorer when confident, else {name}) replay score"),
                &combined_results,
            );
            combined_by_model.insert(
                (*name).to_string(),
                serde_json::json!({
                    "totals": combined_totals,
                    "probes": combined_results,
                }),
            );
        }
    }
    if let Some(out) = out {
        let mut output = serde_json::json!({ "models": report });
        if let Some(sj) = scorer_json {
            output["scorer"] = sj;
        }
        if !combined_by_model.is_empty() {
            output["combined"] = serde_json::Value::Object(combined_by_model);
        }
        std::fs::write(out, serde_json::to_string_pretty(&output)?)
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
