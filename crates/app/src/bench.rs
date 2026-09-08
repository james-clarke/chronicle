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
    segment: bool,
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
    if scorer && segment {
        return segment_fixture_eval(&cases, &config);
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

/// A fixture's focus spans with anchors extracted from their titles and
/// URLs (fixtures carry no collector events), sorted by start.
fn fixture_anchored_spans(
    spans: &[chronicle_core::sessionizer::SpanDraft],
    re: &regex::Regex,
) -> Vec<chronicle_core::profile::AnchoredSpan> {
    use chronicle_core::sessionizer::SpanKind;
    let mut out: Vec<chronicle_core::profile::AnchoredSpan> = spans
        .iter()
        .enumerate()
        .filter(|(_, s)| s.kind == SpanKind::Focus)
        .map(|(i, s)| chronicle_core::profile::AnchoredSpan {
            id: i as i64,
            start_ts: s.start.as_millisecond(),
            end_ts: s.end.as_millisecond(),
            app: s.app.clone(),
            title: s.title.clone(),
            anchors: chronicle_core::extract::extract(&s.app, &s.title, s.url.as_deref(), re),
            vec: None,
            quiet_ms: s.quiet_ms,
            wrote: false,
        })
        .collect();
    out.sort_by_key(|s| s.start_ts);
    out
}

/// `chronicle bench --scorer --segment` (m30 chunk 3): the live path, cold,
/// over each persona fixture — cut with the segmenter, score each segment
/// against profiles that grow from the segments already placed (as the
/// daemon's evidence refresh does), create a task for each new cluster.
/// A group passes when every one of its ranges resolves to one task and
/// no other group resolves to that task. Prints coverage and segment
/// length next to the group score.
fn segment_fixture_eval(cases: &[Case], config: &Config) -> anyhow::Result<()> {
    use chronicle_core::profile::{self, Params};
    use chronicle_core::replay::TaskRow;
    use chronicle_core::segmenter::{self, SegParams, Target};
    use std::collections::HashMap;

    let re = regex::Regex::new(&config.ticket_regex).context("ticket_regex")?;
    let distractions = chronicle_core::evidence::compile_patterns(&config.distraction_patterns);
    let params = Params::default();
    let sp = SegParams::from_config(config);
    let (mut groups_ok, mut groups_n) = (0usize, 0usize);

    for (case, _digest, open, expect, _gaps, spans) in cases {
        let Some(exp) = expect else { continue };
        if exp.groups.is_empty() {
            continue;
        }
        let aspans = fixture_anchored_spans(spans, &re);
        let Some(origin) = aspans.first().map(|s| s.start_ts) else {
            continue;
        };
        let end = aspans.last().map_or(origin, |s| s.end_ts);
        println!("\n=== {case} [segmenter] ({} groups)", exp.groups.len());

        // Cold: the declared tasks' label keys are all the profiles hold.
        let tasks: Vec<TaskRow> = open
            .iter()
            .map(|t| TaskRow {
                id: t.id,
                label: t.label.clone(),
                project: t.project.clone(),
                declared: t.declared,
                closed: false,
                created_ts: origin - 1,
                closed_ts: None,
            })
            .collect();
        let labels: HashMap<i64, String> = tasks.iter().map(|t| (t.id, t.label.clone())).collect();
        let profiles = profile::build_profiles(&tasks, &[], &aspans, &[], &re, origin, &params);
        let segs = segmenter::segment(&aspans, &distractions, &sp);
        let placements = segmenter::decide(
            &aspans,
            origin,
            end,
            &profiles,
            &labels,
            &distractions,
            &params,
            &sp,
        );
        // Clusters become synthetic tasks numbered after the declared ones.
        let base = tasks.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        let mut placed: Vec<(i64, i64, i64)> = Vec::new();
        let mut created: HashMap<usize, String> = HashMap::new();
        let hm = |ms: i64| {
            format!(
                "{:02}:{:02}",
                (ms - origin) / 3_600_000,
                (ms - origin) / 60_000 % 60
            )
        };
        for p in &placements {
            let (task_id, name) = match &p.target {
                Target::Existing(id) => (*id, labels.get(id).cloned().unwrap_or_default()),
                Target::New { label, cluster, .. } => {
                    created.insert(*cluster, label.clone());
                    (base + *cluster as i64, format!("new: {label}"))
                }
            };
            placed.push((p.lo, p.hi, task_id));
            println!(
                "  {}\u{2013}{} -> {name} [{task_id}] {} ({})",
                hm(p.lo),
                hm(p.hi),
                if p.confident { "confident" } else { "unsure" },
                p.reason
            );
        }

        let owner = |range: (i64, i64)| -> Option<i64> {
            let mut by_task: HashMap<i64, i64> = HashMap::new();
            for (lo, hi, t) in &placed {
                let ov = hi.min(&range.1) - lo.max(&range.0);
                if ov > 0 {
                    *by_task.entry(*t).or_insert(0) += ov;
                }
            }
            by_task
                .into_iter()
                .max_by_key(|(_, ms)| *ms)
                .map(|(t, _)| t)
        };
        let mut resolved: Vec<Option<i64>> = Vec::new();
        let mut details: Vec<String> = Vec::new();
        for g in &exp.groups {
            let owners: Vec<Option<i64>> = g
                .ranges
                .iter()
                .map(|&(a, b)| owner((origin + a * 60_000, origin + b * 60_000)))
                .collect();
            let first = owners.first().copied().flatten();
            let one = first.is_some() && owners.iter().all(|o| *o == first);
            resolved.push(if one { first } else { None });
            details.push(
                owners
                    .iter()
                    .map(|o| o.map_or("-".to_owned(), |t| t.to_string()))
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        let mut ok = 0;
        for (gi, g) in exp.groups.iter().enumerate() {
            let shared = resolved[gi].is_some()
                && resolved
                    .iter()
                    .enumerate()
                    .any(|(j, r)| j != gi && *r == resolved[gi]);
            let pass = resolved[gi].is_some() && !shared;
            if pass {
                ok += 1;
            }
            println!(
                "  [{}] {} -> {}{}",
                if pass { "PASS" } else { "FAIL" },
                g.name,
                details[gi],
                if shared {
                    " (shared with another group)"
                } else {
                    ""
                }
            );
        }
        let focus_ms: i64 = aspans.iter().map(|s| s.end_ts - s.start_ts).sum();
        let placed_ms: i64 = placed.iter().map(|(lo, hi, _)| hi - lo).sum();
        let mut lengths: Vec<f64> = segs.iter().map(|s| s.minutes).collect();
        lengths.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let median = lengths.get(lengths.len() / 2).copied().unwrap_or(0.0);
        println!(
            "  {case} segmenter: {ok}/{} groups; {} segments -> {} rows, coverage {:.0}%, median segment {:.0} min, {} tasks created",
            exp.groups.len(),
            segs.len(),
            placements.len(),
            placed_ms as f64 / focus_ms.max(1) as f64 * 100.0,
            median,
            created.len()
        );
        groups_ok += ok;
        groups_n += exp.groups.len();
    }
    println!("\nsegmenter fixtures: {groups_ok}/{groups_n} groups");
    Ok(())
}

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
                vec: None,
                quiet_ms: s.quiet_ms,
                wrote: false,
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
                origin_task_id: Some(task_id),
                pending: false,
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
    // Several corrections over one task (a run of merges into it) repeat
    // the same probe; count each distinct (batch, task, range, check) once.
    let mut unique: std::collections::BTreeMap<(i64, i64, (i64, i64), Check), bool> =
        std::collections::BTreeMap::new();
    for r in results {
        let e = unique
            .entry((r.batch_id, r.task_id, r.range_min, r.check))
            .or_insert(false);
        *e |= r.pass;
    }
    let unique_ok = unique.values().filter(|p| **p).count();
    println!(
        "  total: {ok_all}/{} (lenient {lenient}/{}; unique probes {unique_ok}/{})",
        results.len(),
        results.len(),
        unique.len()
    );
    totals.insert(
        "unique".into(),
        serde_json::json!([unique_ok, unique.len()]),
    );
    totals.insert("total".into(), serde_json::json!([ok_all, results.len()]));
    totals.insert(
        "lenient".into(),
        serde_json::json!([lenient, results.len()]),
    );
    totals
}

/// `chronicle backfill-embeddings` (m30 chunk 6): every focus span without
/// a vector, in batches, then the task centroids.
pub(crate) fn backfill_embeddings(data_dir: &Path) -> anyhow::Result<()> {
    use chronicle_core::storage;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let path = chronicle_derive::model::resolve_embed(config.embed_model.as_deref(), data_dir)
        .context("set `embed_model` in config.toml (e.g. `chronicle model pull bge-small` then `embed_model = \"bge-small\"`)")?;
    let embedder = chronicle_derive::embed::Embedder::load(&path)?;
    let mut conn = storage::open(&data_dir.join("chronicle.db"))?;
    let mut total = 0;
    loop {
        let pending = storage::spans_missing_embeddings(&conn, 500)?;
        if pending.is_empty() {
            break;
        }
        let texts: Vec<String> = pending
            .iter()
            .map(|(_, app, title)| format!("{app}: {title}"))
            .collect();
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let vecs = embedder.embed(&refs)?;
        let rows: Vec<(i64, Vec<f32>)> = pending.iter().map(|(id, ..)| *id).zip(vecs).collect();
        storage::store_span_embeddings(&mut conn, &rows)?;
        total += rows.len();
        println!("embedded {total} spans");
    }
    let n = storage::rebuild_task_embeddings(&mut conn, &[])?;
    println!("{total} spans embedded; {n} task centroids");
    Ok(())
}

/// `chronicle bench --embed <gguf>` (m30 chunk 6): the latency gate for the
/// soft tier. Embeds up to 500 recent distinct titles one at a time, as the
/// daemon would per span, and prints the percentiles against the 20 ms
/// target.
pub(crate) fn embed_bench(data_dir: &Path, model: &Path) -> anyhow::Result<()> {
    use chronicle_core::storage;
    let conn = storage::open(&data_dir.join("chronicle.db"))?;
    let titles = storage::recent_titles(&conn, 500)?;
    if titles.is_empty() {
        bail!("no focus titles stored yet");
    }
    let stats = chronicle_derive::embed::bench(model, &titles)?;
    println!(
        "{}: {} titles, dim {}, load {:.0} ms, p50 {:.1} ms, p95 {:.1} ms, mean {:.1} ms",
        model.display(),
        stats.n,
        stats.dim,
        stats.load_ms,
        stats.p50_ms,
        stats.p95_ms,
        stats.mean_ms
    );
    println!(
        "gate (p95 < 20 ms): {}",
        if stats.p95_ms < 20.0 { "PASS" } else { "FAIL" }
    );
    Ok(())
}

/// `chronicle bench --gaps` (m32 chunk 1 gate): re-sessionize the last
/// `since_days` of events twice — with the configured `quiet_secs` /
/// `away_secs` and with both at 0 (the m31 rule: any idle closes the span)
/// — and print each run's daytime (08–19 local) AFK gap histogram plus the
/// idle time folded into spans as quiet.
/// `bench --window START..END` (m32 chunk 3 gate): the window placed the
/// way `segmenter::reconcile` would place it now, unwritten. Each row with
/// its share and kind, then the window's wall time by project and by task
/// (shares applied), and the check that the shares add up to it.
pub(crate) fn window(data_dir: &Path, spec: &str) -> anyhow::Result<()> {
    use chronicle_core::segmenter::{self, Target};
    use chronicle_core::storage;
    use std::collections::HashMap;

    let (a, b) = spec.split_once("..").context(
        "--window wants START..END, local times like 2026-09-03T15:00..2026-09-03T16:44",
    )?;
    let tz = TimeZone::system();
    let parse = |s: &str| -> anyhow::Result<i64> {
        let dt: civil::DateTime = s.parse().with_context(|| format!("bad time {s:?}"))?;
        Ok(dt.to_zoned(tz.clone())?.timestamp().as_millisecond())
    };
    let (lo, hi) = (parse(a)?, parse(b)?);
    if hi <= lo {
        bail!("--window end is not after its start");
    }
    let hm = |ms: i64| {
        chronicle_core::types::ms_to_ts(ms)
            .to_zoned(tz.clone())
            .strftime("%H:%M")
            .to_string()
    };
    let config = Config::load(&data_dir.join("config.toml"))?;
    let conn = storage::open(&data_dir.join("chronicle.db"))?;
    let distractions = chronicle_core::evidence::compile_patterns(&config.distraction_patterns);
    let re = regex::Regex::new(&config.ticket_regex).context("ticket_regex")?;
    let params = segmenter::params(&config);
    // Profiles as of the window's start, the replay's way: tasks and
    // evidence that existed then, not what was learned since.
    let rows = storage::replay_rows(&conn, 0)?;
    let all_spans = storage::anchored_spans(&conn, 0, lo)?;
    let profiles = chronicle_core::profile::build_profiles(
        &rows.tasks,
        &rows.intervals,
        &all_spans,
        &rows.corrections,
        &re,
        lo,
        &params,
    );
    let labels: HashMap<i64, String> = rows.tasks.iter().map(|t| (t.id, t.label.clone())).collect();
    let projects: HashMap<i64, Option<String>> = rows
        .tasks
        .iter()
        .map(|t| (t.id, t.project.clone()))
        .collect();
    let placements = segmenter::place_dry(
        &conn,
        &config,
        lo,
        hi,
        &profiles,
        &labels,
        &projects,
        &distractions,
    )?;
    let mut tasks: HashMap<i64, (String, Option<String>)> = HashMap::new();
    let mut stmt = conn.prepare("SELECT id, label, project FROM tasks")?;
    for row in stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))? {
        let (id, label, project): (i64, String, Option<String>) = row?;
        tasks.insert(id, (label, project));
    }
    println!(
        "profiles as of {}: {} tasks with evidence",
        hm(lo),
        profiles.len()
    );
    for pr in &profiles {
        let mut keys: Vec<(&chronicle_core::profile::Key, f64)> =
            pr.minutes.iter().map(|(k, m)| (k, *m)).collect();
        keys.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        println!(
            "  [{}] {}: {}",
            pr.task_id,
            tasks.get(&pr.task_id).map_or("?", |(l, _)| l.as_str()),
            keys.iter()
                .take(6)
                .map(|(k, m)| format!("{}={:.0}", k.value(), m))
                .collect::<Vec<_>>()
                .join(" ")
        );
    }
    let mut by_project: Vec<(String, f64)> = Vec::new();
    let mut by_task: Vec<(String, f64)> = Vec::new();
    let mut placed = 0.0;
    let bump = |v: &mut Vec<(String, f64)>, key: String, ms: f64| match v
        .iter_mut()
        .find(|(k, _)| *k == key)
    {
        Some(e) => e.1 += ms,
        None => v.push((key, ms)),
    };
    println!(
        "{}–{} placed as reconcile would ({} rows):",
        hm(lo),
        hm(hi),
        placements.len()
    );
    for p in &placements {
        let (label, project) = match &p.target {
            Target::Existing(id) => tasks
                .get(id)
                .map(|(l, pr)| (format!("{l} [{id}]"), pr.clone()))
                .unwrap_or_else(|| (format!("task {id}"), None)),
            Target::New { label, project, .. } => (format!("new: {label}"), project.clone()),
        };
        let ms = (p.hi.min(hi) - p.lo.max(lo)).max(0) as f64 * p.share;
        placed += ms;
        bump(
            &mut by_project,
            project.clone().unwrap_or_else(|| "untagged".into()),
            ms,
        );
        bump(&mut by_task, label.clone(), ms);
        println!(
            "  {}–{}  {:>4.0}%  {:<10} {label}{}  ({}{})",
            hm(p.lo),
            hm(p.hi),
            p.share * 100.0,
            p.kind,
            project.map(|pr| format!(" [{pr}]")).unwrap_or_default(),
            p.reason,
            if p.confident { "" } else { ", unsure" }
        );
    }
    // The sessions the split saw, and where each one's own evidence lands.
    let spans = storage::anchored_spans(&conn, lo, hi)?;
    let sessions = storage::live_sessions(&conn, lo, hi, &re)?;
    println!("sessions live around the window ({}):", sessions.len());
    for s in &sessions {
        let owned: Vec<_> = spans
            .iter()
            .filter(|sp| {
                sp.anchors.iter().any(|a| {
                    a.kind == chronicle_core::extract::AnchorKind::Session && a.value == s.id
                })
            })
            .cloned()
            .collect();
        let focus: i64 = owned
            .iter()
            .map(|sp| sp.end_ts.min(hi) - sp.start_ts.max(lo))
            .sum();
        let task = segmenter::session_verdict(s, &owned, lo, hi, &profiles, &params, &distractions)
            .and_then(|v| v.best);
        println!(
            "  {:<12} writes {:>3}  prompts {:>3}  on screen {:>3} min  scope {}  -> {}",
            s.id.chars().take(12).collect::<String>(),
            s.writes.iter().filter(|t| (lo..=hi).contains(t)).count(),
            s.prompts.iter().filter(|t| (lo..=hi).contains(t)).count(),
            focus / 60_000,
            s.anchors
                .iter()
                .filter(|a| a.kind != chronicle_core::extract::AnchorKind::Session)
                .map(|a| a.value.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            task.map_or("new".to_owned(), |id| tasks
                .get(&id)
                .map_or(format!("task {id}"), |(l, _)| format!("{l} [{id}]")))
        );
    }
    let wall = (hi - lo) as f64;
    for (name, v) in [("project", &mut by_project), ("task", &mut by_task)] {
        v.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        println!("by {name}:");
        for (k, ms) in v.iter() {
            println!(
                "  {:>5.1}%  {:>4.0} min  {k}",
                ms / wall * 100.0,
                ms / 60_000.0
            );
        }
    }
    println!(
        "placed {:.0} of {:.0} min ({:.1}% of the window)",
        placed / 60_000.0,
        wall / 60_000.0,
        placed / wall * 100.0
    );
    Ok(())
}

pub(crate) fn gaps(data_dir: &Path, since_days: u64) -> anyhow::Result<()> {
    use chronicle_core::sessionizer::{SpanKind, sessionize_with};
    use chronicle_core::storage;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let conn = storage::open(&data_dir.join("chronicle.db"))?;
    let now = Timestamp::now();
    let since_ms = now.as_millisecond() - since_days as i64 * 86_400_000;
    let events = storage::load_events_from(&conn, since_ms)?;
    let live = storage::live_context(&conn, since_ms)?;
    let tz = TimeZone::system();
    let legacy = Config {
        quiet_secs: 0,
        away_secs: 0,
        ..config.clone()
    };
    println!(
        "{} events in the last {since_days} d; {} session writes, {} calls in context",
        events.len(),
        live.session_writes.len(),
        live.calls.len()
    );
    for (name, cfg) in [("m31 rule", &legacy), ("configured", &config)] {
        let spans = sessionize_with(&events, now, cfg, &live);
        // (count, total ms) for 2–5, 5–15 and 15+ minute gaps.
        let mut buckets = [(0usize, 0i64); 3];
        for s in spans.iter().filter(|s| s.kind == SpanKind::Afk) {
            let hour = s.start.to_zoned(tz.clone()).hour();
            if !(8..19).contains(&hour) {
                continue;
            }
            let mins = s.duration_ms() / 60_000;
            let bucket = match mins {
                ..2 => continue,
                2..5 => 0,
                5..15 => 1,
                _ => 2,
            };
            buckets[bucket].0 += 1;
            buckets[bucket].1 += s.duration_ms();
        }
        let quiet: i64 = spans.iter().map(|s| s.quiet_ms).sum();
        println!(
            "{name} (quiet_secs={} away_secs={}): {} spans",
            cfg.quiet_secs,
            cfg.away_secs,
            spans.len()
        );
        for (label, (n, ms)) in ["2–5 min", "5–15 min", "15+ min"].iter().zip(buckets) {
            println!("  daytime gaps {label}: {n} ({} min)", ms / 60_000);
        }
        println!("  idle folded into spans as quiet: {} min", quiet / 60_000);
    }
    Ok(())
}

/// `chronicle bench --calibrate` (m30 chunk 4): what the verdict log says
/// about the scorer's margins — per 0.05 bucket, how many placements the
/// user corrected — and the delta under which the worst tenth of placements
/// would read "to confirm".
pub(crate) fn calibrate(data_dir: &Path, since_days: u64) -> anyhow::Result<()> {
    use chronicle_core::storage;
    let conn = storage::open(&data_dir.join("chronicle.db"))?;
    let since_ms = Timestamp::now().as_millisecond() - since_days as i64 * 86_400_000;
    let mut rows = storage::verdict_outcomes(&conn, since_ms)?;
    if rows.is_empty() {
        println!(
            "no closed verdicts in the last {since_days} days (segmenter mode writes them on reconcile; corrections and a day's silence close them)"
        );
        return Ok(());
    }
    rows.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let wrong = |r: &(f64, bool, String)| r.2 == "wrong";
    println!(
        "{} closed verdicts, {} corrected",
        rows.len(),
        rows.iter().filter(|r| wrong(r)).count()
    );
    println!("margin     n   wrong  share");
    let mut lo = 0.0;
    while lo < 1.0 {
        let hi = lo + 0.05;
        let bucket: Vec<_> = rows
            .iter()
            .filter(|r| r.0 >= lo && (r.0 < hi || (hi >= 1.0 && r.0 <= 1.0)))
            .collect();
        if !bucket.is_empty() {
            let w = bucket.iter().filter(|r| wrong(r)).count();
            println!(
                "{lo:.2}-{hi:.2} {:4} {:6}  {:3.0}%",
                bucket.len(),
                w,
                w as f64 / bucket.len() as f64 * 100.0
            );
        }
        lo = hi;
    }
    // Delta so that the lowest-margin tenth is "to confirm".
    let tenth = rows.len().div_ceil(10);
    let suggested = rows.get(tenth.saturating_sub(1)).map_or(0.0, |r| r.0);
    let above: Vec<_> = rows.iter().filter(|r| r.0 >= suggested).collect();
    let wrong_above = above.iter().filter(|r| wrong(r)).count();
    println!(
        "delta {suggested:.2} puts the worst {tenth} of {} to confirm; above it {wrong_above} of {} were corrected ({:.0}%). Set `scorer_delta = {suggested:.2}` in config.toml to use it.",
        rows.len(),
        above.len(),
        if above.is_empty() {
            0.0
        } else {
            wrong_above as f64 / above.len() as f64 * 100.0
        }
    );
    Ok(())
}

/// One derive engine for the replay: the local session or a cloud backend
/// (m31 chunk 0). Both take the same digest and return the same `DeriveRun`.
enum ReplayEngine<'m> {
    Local(chronicle_derive::DeriveSession<'m>),
    Cloud(&'m dyn chronicle_derive::text::TextBackend),
}

impl ReplayEngine<'_> {
    /// The run plus its dollar cost (cloud only).
    fn infer(
        &mut self,
        digest: &str,
    ) -> anyhow::Result<(chronicle_derive::DeriveRun, Option<f64>)> {
        use chronicle_derive::runner::{Prompt, RunStats, finish_derive};
        use chronicle_derive::text::{JobKind, Request};
        match self {
            ReplayEngine::Local(s) => Ok((s.infer(digest, &mut |_| {})?, None)),
            ReplayEngine::Cloud(b) => {
                let rendered = Prompt::Batch.render(digest);
                let schema: serde_json::Value = serde_json::from_str(Prompt::Batch.schema_json())?;
                let req = Request {
                    job: JobKind::Derive,
                    system: None,
                    user: chronicle_derive::prompts::strip_no_think(&rendered),
                    history: &[],
                    schema: Some(&schema),
                    max_output: 0,
                };
                let c = b.complete(&req, &mut |_| {})?;
                let cost = chronicle_derive::cloud::cost_usd(b.model(), &c);
                let stats = RunStats {
                    prompt_tokens: c.input_tokens as usize,
                    cached_prefix_tokens: c.cache_read_tokens as usize,
                    gen_tokens: c.output_tokens as usize,
                    prompt_eval_ms: 0,
                    gen_ms: c.wall_ms,
                };
                Ok((finish_derive(c.text, stats)?, cost))
            }
        }
    }
}

/// `chronicle bench --replay`: re-derive every done batch a recent correction
/// touched, with the open-task list as it stood at the batch's end, and score
/// whether the corrected outcome comes out (m27 chunk 2). No MCP context: it
/// is live data and would make runs incomparable.
pub(crate) fn replay_eval(
    data_dir: &Path,
    since_days: u64,
    model_filter: Option<&str>,
    backend: Option<&str>,
    scorer: bool,
    segment: bool,
    probe_set: chronicle_core::replay::ProbeSet,
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
    let (probes, skipped) = replay::build_probes(&view, probe_set);
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
            // `--segment`: the batch cut the segmenter's way, each piece
            // scored on its own; a probe takes the minute-weighted majority
            // of the pieces under its range.
            let cuts = segment.then(|| {
                let bspans: Vec<profile::AnchoredSpan> = spans
                    .iter()
                    .filter(|s| s.end_ts > batch.start_ts && s.start_ts < batch.end_ts)
                    .cloned()
                    .collect();
                let sp = chronicle_core::segmenter::SegParams::from_config(&config);
                let segs = chronicle_core::segmenter::segment(&bspans, &distractions, &sp);
                (bspans, segs)
            });
            for p in probes.iter().filter(|p| p.batch_id == bid) {
                let seg = Segment::from_spans_skipping(&spans, p.range.0, p.range.1, &distractions);
                let mut v = profile::score(&seg, &profiles, &params);
                if let Some((bspans, segs)) = &cuts {
                    let mut tally: HashMap<Option<i64>, f64> = HashMap::new();
                    for sg in segs.iter().filter(|s| s.hi > p.range.0 && s.lo < p.range.1) {
                        let (lo, hi) = (sg.lo.max(p.range.0), sg.hi.min(p.range.1));
                        let piece = Segment::from_spans_skipping(bspans, lo, hi, &distractions);
                        if piece.keys.is_empty() {
                            continue;
                        }
                        let pv = profile::score(&piece, &profiles, &params);
                        *tally.entry(pv.best).or_insert(0.0) += piece.minutes;
                    }
                    if let Some((best, _)) = tally
                        .iter()
                        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
                    {
                        v.best = *best;
                    }
                }
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
    // m31 chunk 0: `--backend <name>` replays through a cloud backend from
    // models.toml instead of a downloaded model, same digest, same scoring.
    let cloud: Option<(String, Box<dyn chronicle_derive::text::TextBackend>)> = match backend {
        Some(name) => {
            let mc = chronicle_core::models_config::ModelsConfig::load(data_dir)?;
            let cfg = mc
                .backends
                .get(name)
                .with_context(|| format!("no [backends.{name}] in models.toml"))?;
            Some((name.to_owned(), chronicle_derive::cloud::build(name, cfg)?))
        }
        None => None,
    };
    let models: Vec<(String, Option<PathBuf>)> = if let Some((name, _)) = &cloud {
        vec![(name.clone(), None)]
    } else if scorer && model_filter.is_none() {
        Vec::new()
    } else {
        bench_models(data_dir, model_filter)?
            .into_iter()
            .map(|(n, p)| (n.to_owned(), Some(p)))
            .collect()
    };
    let combine = scorer && !models.is_empty();

    let mut report = Vec::new();
    let mut combined_by_model = serde_json::Map::new();
    for (name, path) in &models {
        let name = name.as_str();
        let local_model;
        let mut engine = match path {
            Some(path) => {
                local_model = chronicle_derive::DeriveModel::load(path)?;
                ReplayEngine::Local(local_model.session()?)
            }
            None => ReplayEngine::Cloud(cloud.as_ref().expect("cloud backend").1.as_ref()),
        };
        let mut spent_usd = 0.0;
        let mut spent_wall = 0.0;
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
            match engine.infer(&bd.digest) {
                Ok((run, cost)) => {
                    let cached = run.cached_prefix_tokens;
                    spent_wall += t0.elapsed().as_secs_f64();
                    if let Some(c) = cost {
                        spent_usd += c;
                        println!(
                            "--- {} prompt + {} gen tokens, ${c:.4}",
                            run.prompt_tokens, run.gen_tokens
                        );
                    }
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
                            task_id: p.task_id,
                            range_min: (
                                (p.range.0 - batch.start_ts) / 60_000,
                                (p.range.1 - batch.start_ts + 59_999) / 60_000,
                            ),
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
        if path.is_none() {
            println!(
                "{name}: {} batches in {spent_wall:.1}s wall, ${spent_usd:.4} total",
                batch_ids.len()
            );
        }
        report.push(serde_json::json!({
            "model": name,
            "since_days": since_days,
            "generated_ts": Timestamp::now().as_millisecond(),
            "skipped": skipped,
            "totals": totals,
            "wall_secs": spent_wall,
            "cost_usd": spent_usd,
            "probes": results,
        }));

        if combine {
            let combined_totals = print_totals(
                &format!("combined (scorer when confident, else {name}) replay score"),
                &combined_results,
            );
            combined_by_model.insert(
                name.to_string(),
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

/// `since` as a local day's start in UTC ms; `None` is the epoch.
fn since_ms(since: Option<&str>) -> anyhow::Result<i64> {
    Ok(match since {
        Some(day) => {
            let day: civil::Date = day.parse().with_context(|| format!("bad date {day:?}"))?;
            day.to_zoned(TimeZone::system())?
                .timestamp()
                .as_millisecond()
        }
        None => 0,
    })
}

/// `chronicle backfill-sessions`: re-read every transcript under
/// `ai_session_dirs` modified since `since` (a local day) and replace its
/// `ai_session` rows (m32 chunk 2). The running daemon re-upserts the
/// sessions it still tracks by the same ids, so the two agree.
pub(crate) fn backfill_sessions(data_dir: &Path, since: Option<&str>) -> anyhow::Result<()> {
    use chronicle_capture::ai_sessions::replay_transcripts;
    use chronicle_core::{config::expand_home, storage};
    let config = Config::load(&data_dir.join("config.toml"))?;
    let dirs: Vec<PathBuf> = config
        .ai_session_dirs
        .iter()
        .map(|p| expand_home(p))
        .collect();
    let lo = since_ms(since)?;
    let since = std::time::UNIX_EPOCH + std::time::Duration::from_millis(lo.max(0) as u64);
    let mut conn = storage::open(&data_dir.join("chronicle.db"))?;
    let mut sessions = 0;
    let mut rows = 0;
    for (id, events) in replay_transcripts(&dirs, since) {
        storage::replace_session(&mut conn, &id, &events)?;
        sessions += 1;
        rows += events.len();
    }
    println!("replaced {sessions} sessions ({rows} rows)");
    Ok(())
}

/// `chronicle backfill-notes`: one pass of the notes reader over every
/// `git_repos` entry, rows upserted by their `(kind, ext_id)` like the
/// daemon's own (m32 chunk 5).
pub(crate) fn backfill_notes(data_dir: &Path) -> anyhow::Result<()> {
    use chronicle_capture::notes::NotesProvider;
    use chronicle_core::{config::expand_home, storage};
    let config = Config::load(&data_dir.join("config.toml"))?;
    let repos: Vec<PathBuf> = config.git_repos.iter().map(|p| expand_home(p)).collect();
    let conn = storage::open(&data_dir.join("chronicle.db"))?;
    let mut n = 0;
    for e in NotesProvider::new(&repos).poll() {
        storage::insert_activity_event(&conn, &e)?;
        n += 1;
    }
    println!("upserted {n} notes");
    Ok(())
}

/// `chronicle backfill-anchors`: recompute anchors for every focus span
/// since `since` (a local day) or all of them.
pub(crate) fn backfill_anchors(data_dir: &Path, since: Option<&str>) -> anyhow::Result<()> {
    use chronicle_core::storage;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let re = regex::Regex::new(&config.ticket_regex).context("ticket_regex")?;
    let lo = since_ms(since)?;
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

/// `chronicle backfill-evidence`: recompute `task_evidence` for every task
/// from stored intervals, anchored spans and corrections.
pub(crate) fn backfill_evidence(data_dir: &Path) -> anyhow::Result<()> {
    use chronicle_core::{profile, storage};
    let config = Config::load(&data_dir.join("config.toml"))?;
    let re = regex::Regex::new(&config.ticket_regex).context("ticket_regex")?;
    let mut conn = storage::open(&data_dir.join("chronicle.db"))?;
    let now_ts = Timestamp::now().as_millisecond();
    let n = storage::rebuild_task_evidence(&mut conn, &re, &profile::Params::default(), now_ts)?;
    println!("rebuilt {n} task_evidence rows");
    Ok(())
}

/// `chronicle evidence`: a task's evidence rows, or a summary of the
/// strongest evidence across all tasks.
pub(crate) fn evidence_report(
    data_dir: &Path,
    task: Option<i64>,
    top: usize,
) -> anyhow::Result<()> {
    use chronicle_core::storage;
    let conn = storage::open(&data_dir.join("chronicle.db"))?;
    if let Some(task_id) = task {
        let mut rows = storage::task_evidence(&conn, task_id)?;
        rows.sort_by_key(|row| row.key.is_term());
        let total = rows.len();
        for row in rows.iter().take(60) {
            println!(
                "  {:8} {:>7.1}m  {:10} {}",
                row.key.kind_str(),
                row.minutes,
                row.source.as_str(),
                row.key.value()
            );
        }
        if total > 60 {
            println!("… and {} more", total - 60);
        }
    } else {
        let summary = storage::evidence_summary(&conn, top)?;
        for task in &summary {
            println!("#{} {} — {} rows", task.task_id, task.label, task.rows);
            for (kind, value, minutes) in &task.top {
                println!("  {kind}={value} {minutes:.0}m");
            }
        }
    }
    Ok(())
}
