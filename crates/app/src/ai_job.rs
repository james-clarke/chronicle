use std::path::Path;

use crate::daemon::bump_day_counter_by;
use crate::status::{fmt_secs, init_logging};
use anyhow::{Context, bail};
use chronicle_core::config::Config;
use jiff::{Timestamp, tz::TimeZone};

/// Ephemeral AI-job worker: claim job → kind-specific inference → store
/// result → exit. Any failure marks the job failed (retry-once via attempts
/// cap), mirroring the derive worker.
pub(crate) fn ai_job_worker(data_dir: &Path, job_id: i64) -> anyhow::Result<()> {
    use chronicle_core::storage;
    let _guard = init_logging(data_dir)?;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let conn = storage::open(&data_dir.join("chronicle.db"))?;
    // No unconditional model gate: fetch_context runs LLM-free; the LLM arms
    // bail individually when no model resolves.
    let model_path = chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir);
    let Some(job) = storage::claim_ai_job(&conn, job_id)? else {
        bail!("ai job {job_id} is not eligible")
    };
    match run_ai_job(&conn, &config, data_dir, model_path.as_deref(), &job) {
        Ok(result) => {
            storage::complete_ai_job(&conn, job_id, &result)?;
            tracing::info!(job_id, kind = %job.kind, "ai job done");
            Ok(())
        }
        Err(e) if e.is::<SkipJob>() => {
            tracing::info!(job_id, kind = %job.kind, "ai job skipped: {e}");
            storage::skip_ai_job(&conn, job_id, &e.to_string())?;
            Ok(())
        }
        Err(e) => {
            tracing::error!(job_id, kind = %job.kind, "ai job failed: {e:#}");
            storage::fail_ai_job(&conn, job_id, &format!("{e:#}"))?;
            Err(e)
        }
    }
}

/// An AI job whose premise no longer holds: terminal but not a failure.
#[derive(Debug)]
pub(crate) struct SkipJob(String);

impl std::fmt::Display for SkipJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SkipJob {}

pub(crate) fn task_label_project(
    conn: &rusqlite::Connection,
    task_id: i64,
) -> anyhow::Result<(String, Option<String>)> {
    Ok(conn.query_row(
        "SELECT label, project FROM tasks WHERE id=?1",
        [task_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

pub(crate) fn run_ai_job(
    conn: &rusqlite::Connection,
    config: &Config,
    data_dir: &Path,
    model_path: Option<&Path>,
    job: &chronicle_core::storage::AiJobRow,
) -> anyhow::Result<String> {
    use chronicle_core::{insights, report, storage};
    let payload: serde_json::Value = serde_json::from_str(&job.payload)?;
    if job.kind == "fetch_context" {
        let task_id = payload["task_id"]
            .as_i64()
            .context("payload lacks task_id")?;
        let ext_ref: Option<String> = conn.query_row(
            "SELECT external_ref FROM tasks WHERE id=?1",
            [task_id],
            |r| r.get(0),
        )?;
        let Some(ext_ref) = ext_ref else {
            bail!("task {task_id} has no external_ref")
        };
        let mcp_path = config.mcp_path(data_dir);
        let (calls, content) = chronicle_mcp::fetch_context(&mcp_path, &ext_ref);
        bump_day_counter_by(conn, "fetches", calls);
        let Some(content) = content else {
            bail!("no fetch_calls configured or every call failed")
        };
        storage::upsert_task_context(conn, task_id, "mcp", Timestamp::now(), &content)?;
        return Ok(content);
    }
    let model_path = model_path.context("no model available; run `chronicle model pull`")?;
    let describer = chronicle_derive::describe::Describer::load(model_path)?;
    match job.kind.as_str() {
        "task_description" => {
            let task_id = payload["task_id"]
                .as_i64()
                .context("payload lacks task_id")?;
            let (label, project) = task_label_project(conn, task_id)?;
            let evidence = storage::task_evidence_text(conn, task_id)?;
            if evidence.trim().is_empty() {
                bail!("no span evidence for task {task_id}");
            }
            let desc = describer.describe_task(&label, project.as_deref(), &evidence)?;
            storage::set_task_description(conn, task_id, Some(&desc))?;
            Ok(desc)
        }
        "suggest_task" => {
            // A proposal names its own cluster (explicit bounds); the Home
            // "suggest" button looks back from now.
            let lookback_min = payload["lookback_min"].as_i64().unwrap_or(15);
            let hi = payload["hi"]
                .as_i64()
                .unwrap_or_else(|| Timestamp::now().as_millisecond());
            let lo = payload["lo"].as_i64().unwrap_or(hi - lookback_min * 60_000);
            let spans = storage::spans_in_range(conn, lo, hi)?;
            if spans
                .iter()
                .all(|s| s.kind != chronicle_core::sessionizer::SpanKind::Focus)
            {
                bail!("no recent focus activity to suggest from");
            }
            let tz = TimeZone::system();
            let digest = chronicle_core::digest::build_digest(
                &spans,
                &tz,
                &[],
                &[],
                &[],
                &[],
                None,
                None,
                None,
            );
            let s = describer.suggest_task(&digest)?;
            Ok(serde_json::to_string(&s)?)
        }
        "journal" => {
            let task_id = payload["task_id"]
                .as_i64()
                .context("payload lacks task_id")?;
            let batch_id = payload["batch_id"]
                .as_i64()
                .context("payload lacks batch_id")?;
            let Some((evidence, interval_ids, lo, hi)) =
                storage::task_batch_evidence(conn, task_id, batch_id)?
            else {
                // The batch was re-derived under other tasks since this job
                // was queued; the re-derive queued its own journal jobs.
                return Err(SkipJob(format!(
                    "batch {batch_id} holds no intervals for task {task_id}"
                ))
                .into());
            };
            if evidence.trim().is_empty() {
                bail!("no span evidence for task {task_id} in batch {batch_id}");
            }
            let (label, project) = task_label_project(conn, task_id)?;
            // Head of the ticket context only: the prompt budget belongs to
            // the session evidence, which truncation cuts from the tail.
            let context: String = storage::task_context(conn, task_id)?
                .map(|(_, c)| c.chars().take(1200).collect())
                .unwrap_or_default();
            let tz = TimeZone::system();
            let mut git = String::new();
            for v in storage::activity_for_task_in_range(conn, task_id, lo, hi)? {
                use std::fmt::Write as _;
                let _ = writeln!(
                    git,
                    "{}",
                    chronicle_core::digest::activity_line(&v, &tz, 120)
                );
            }
            let entry =
                describer.journal_entry(&label, project.as_deref(), &context, &git, &evidence)?;
            storage::insert_journal_entry(
                conn,
                task_id,
                batch_id,
                lo,
                hi,
                &entry,
                &serde_json::to_string(&interval_ids)?,
            )?;
            Ok(entry)
        }
        "checkpoint" => {
            let task_id = payload["task_id"]
                .as_i64()
                .context("payload lacks task_id")?;
            let (label, project) = task_label_project(conn, task_id)?;
            let tail = storage::journal_tail(conn, task_id, 15)?;
            if tail.is_empty() {
                bail!("no journal entries for task {task_id} to checkpoint");
            }
            let tz = TimeZone::system();
            let mut journal = String::new();
            for e in &tail {
                use std::fmt::Write as _;
                let hm = chronicle_core::types::ms_to_ts(e.start_ts)
                    .to_zoned(tz.clone())
                    .strftime("%m-%d %H:%M");
                let _ = writeln!(journal, "- [{hm}] {}", e.entry);
            }
            let context: String = storage::task_context(conn, task_id)?
                .map(|(_, c)| c.chars().take(2400).collect())
                .unwrap_or_default();
            let (state, next_steps) =
                describer.checkpoint(&label, project.as_deref(), &context, &journal)?;
            storage::upsert_checkpoint(conn, task_id, Timestamp::now(), &state, &next_steps)?;
            Ok(format!("{state}\n{next_steps}"))
        }
        "narrative" => {
            let lo = payload["lo"].as_i64().context("payload lacks lo")?;
            let hi = payload["hi"].as_i64().context("payload lacks hi")?;
            let tz = TimeZone::system();
            let days = civil_days(lo, hi, &tz)?;
            let tasks = storage::tasks_in_range(conn, lo, hi)?;
            let r = report::build(&tasks, days.clone(), &tz)?;
            if r.grand_total_ms == 0 {
                bail!("no activity in range to narrate");
            }
            let sessions = insights::sessions_from_tasks(&tasks, lo, hi);
            let metrics = insights::focus_metrics(&sessions, &tz);
            let spans = storage::spans_in_range(conn, lo, hi)?;
            let apps = insights::top_apps(&spans, lo, hi, 5);
            let delta = insights::prior_period(&days).and_then(|pd| {
                let plo = pd
                    .first()?
                    .to_zoned(tz.clone())
                    .ok()?
                    .timestamp()
                    .as_millisecond();
                let ptasks = storage::tasks_in_range(conn, plo, lo).ok()?;
                let pr = report::build(&ptasks, pd, &tz).ok()?;
                (pr.grand_total_ms > 0).then(|| insights::delta(&r, &pr))
            });
            let digest = insights::narrative_digest(&r, &metrics, &apps, delta.as_ref());
            let text = describer.narrative(&digest)?;
            let hash = insights::report_data_hash(&r);
            storage::upsert_narrative(conn, lo, hi, hash, Timestamp::now(), &text)?;
            Ok(text)
        }
        "standup" => {
            let day = payload["day"].as_str().context("payload lacks day")?;
            let tz = TimeZone::system();
            let date: jiff::civil::Date = day.parse().context("bad day in payload")?;
            let start = date.to_zoned(tz.clone())?;
            let lo = start.timestamp().as_millisecond();
            let hi = start
                .checked_add(jiff::Span::new().days(1))?
                .timestamp()
                .as_millisecond();
            let rows = storage::standup_digest(conn, lo, hi)?;
            let text = if rows.is_empty() {
                // Journals only exist once tasks run long enough to batch;
                // fall back to a plain activity summary so day one still
                // drafts something. Stored verbatim, no LLM pass: with only
                // durations and app names as input, the model invents
                // outcomes and next steps (seen live even with a
                // don't-invent instruction in the digest).
                let Some(fallback) = standup_activity_fallback(conn, config, lo, hi)? else {
                    bail!("no journal entries or task activity on {day} to draft a standup from");
                };
                fallback
            } else {
                let plan = chronicle_core::intent::plan_body(conn, day)?;
                describer.standup(&standup_digest_text(&rows, &tz, plan.as_deref()))?
            };
            storage::upsert_standup_draft(conn, day, Timestamp::now(), &text)?;
            Ok(text)
        }
        other => bail!("unknown ai job kind {other}"),
    }
}

/// Render the standup digest rows for the prompt: the day's plan when one
/// was set (m26), then per task the day's journal tail (last 5 entries keeps
/// multi-task days inside the prompt budget) plus the fresh checkpoint if
/// one exists.
pub(crate) fn standup_digest_text(
    rows: &[chronicle_core::storage::StandupDigestRow],
    tz: &TimeZone,
    plan: Option<&str>,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if let Some(plan) = plan.map(str::trim).filter(|s| !s.is_empty()) {
        let _ = writeln!(out, "## Plan\n{plan}");
    }
    for row in rows {
        let project = row
            .project
            .as_deref()
            .map(|p| format!(" [{p}]"))
            .unwrap_or_default();
        let anchor = row
            .external_ref
            .as_deref()
            .map(|r| format!(" ({r})"))
            .unwrap_or_default();
        let _ = writeln!(out, "Task: {}{project}{anchor}", row.label);
        let skip = row.entries.len().saturating_sub(5);
        for e in &row.entries[skip..] {
            let hm = chronicle_core::types::ms_to_ts(e.start_ts)
                .to_zoned(tz.clone())
                .strftime("%H:%M");
            let _ = writeln!(out, "- [{hm}] {}", e.entry);
        }
        if let Some(c) = &row.checkpoint {
            let _ = writeln!(out, "Checkpoint: {}", c.state);
            let _ = writeln!(out, "Next steps: {}", c.next_steps);
        }
        out.push('\n');
    }
    out
}

/// Journal-free standup draft, stored verbatim (no LLM pass): per task, the
/// day's focused total and top apps. `None` when the day holds no task
/// activity. Background-scale tasks
/// (short, undeclared, unanchored — see `Config::background_minutes`) are
/// left out unless they are all there is.
pub(crate) fn standup_activity_fallback(
    conn: &rusqlite::Connection,
    config: &Config,
    lo: i64,
    hi: i64,
) -> anyhow::Result<Option<String>> {
    use chronicle_core::storage;
    use std::fmt::Write as _;
    struct Agg {
        label: String,
        project: Option<String>,
        external_ref: Option<String>,
        declared: bool,
        total_ms: i64,
    }
    let mut order: Vec<i64> = Vec::new();
    let mut aggs: std::collections::HashMap<i64, Agg> = std::collections::HashMap::new();
    for t in storage::tasks_in_range(conn, lo, hi)? {
        let dur = t.end_ts.as_millisecond().min(hi) - t.start_ts.as_millisecond().max(lo);
        let agg = aggs.entry(t.id).or_insert_with(|| {
            order.push(t.id);
            Agg {
                label: t.label,
                project: t.project,
                external_ref: t.external_ref,
                declared: t.declared,
                total_ms: 0,
            }
        });
        agg.total_ms += dur.max(0);
    }
    if order.is_empty() {
        return Ok(None);
    }
    let background_ms = i64::from(config.background_minutes) * 60_000;
    let foreground =
        |a: &Agg| a.declared || a.external_ref.is_some() || a.total_ms >= background_ms;
    if order.iter().any(|id| foreground(&aggs[id])) {
        order.retain(|id| foreground(&aggs[id]));
    }
    // Top apps per task, largest first (evidence_in_range returns per-title
    // rows in overlap-descending order; sum per app).
    let mut apps: std::collections::HashMap<i64, Vec<(String, i64)>> =
        std::collections::HashMap::new();
    for row in storage::evidence_in_range(conn, lo, hi)? {
        let list = apps.entry(row.task_id).or_default();
        match list.iter_mut().find(|(a, _)| *a == row.app) {
            Some((_, ms)) => *ms += row.ms,
            None => list.push((row.app, row.ms)),
        }
    }
    let mut out = String::from(
        "No journal entries were written this day; this is a focus-time summary \
         from activity capture.\n\n",
    );
    for id in &order {
        let a = &aggs[id];
        let project = a
            .project
            .as_deref()
            .map(|p| format!(" [{p}]"))
            .unwrap_or_default();
        let anchor = a
            .external_ref
            .as_deref()
            .map(|r| format!(" ({r})"))
            .unwrap_or_default();
        let _ = writeln!(out, "Task: {}{project}{anchor}", a.label);
        let _ = writeln!(out, "- focused {}", fmt_secs((a.total_ms / 1000) as u64));
        if let Some(list) = apps.get_mut(id) {
            list.sort_by_key(|(_, ms)| -*ms);
            let tops: Vec<String> = list
                .iter()
                .take(3)
                .map(|(app, ms)| format!("{app} {}", fmt_secs((*ms / 1000) as u64)))
                .collect();
            let _ = writeln!(out, "- mainly in: {}", tops.join(", "));
        }
        out.push('\n');
    }
    Ok(Some(out))
}

/// Local civil dates covering `[lo, hi)` in `tz`.
pub(crate) fn civil_days(
    lo: i64,
    hi: i64,
    tz: &TimeZone,
) -> anyhow::Result<Vec<jiff::civil::Date>> {
    let mut days = Vec::new();
    let mut d = chronicle_core::types::ms_to_ts(lo)
        .to_zoned(tz.clone())
        .date();
    let last = chronicle_core::types::ms_to_ts((hi - 1).max(lo))
        .to_zoned(tz.clone())
        .date();
    while d <= last {
        days.push(d);
        d = d.checked_add(jiff::Span::new().days(1))?;
    }
    Ok(days)
}
