use std::cell::RefCell;
use std::path::Path;

use crate::daemon::bump_day_counter_by;
use crate::status::{fmt_secs, init_logging};
use anyhow::{Context, bail};
use chronicle_core::config::Config;
use chronicle_core::models_config::ModelsConfig;
use chronicle_core::sessionizer::SpanDraft;
use chronicle_core::types::SuggestedTask;
use chronicle_derive::cloud::{self, CloudError};
use chronicle_derive::describe::Describer;
use chronicle_derive::prompts::{self, non_empty, strip_no_think};
use chronicle_derive::text::{Completion, JobKind, Request, TextBackend};
use jiff::{Timestamp, Zoned, tz::TimeZone};

/// Ephemeral AI-job worker: claim job → kind-specific inference → store
/// result → exit. Any failure marks the job failed (retry-once via attempts
/// cap), mirroring the derive worker. A cloud route (m31) is tried first;
/// a cloud failure falls back to the local model when one is downloaded
/// and otherwise returns the job to the queue with the reason on it.
pub(crate) fn ai_job_worker(data_dir: &Path, job_id: i64) -> anyhow::Result<()> {
    use chronicle_core::storage;
    let _guard = init_logging(data_dir)?;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let conn = storage::open(&data_dir.join("chronicle.db"))?;
    // No unconditional model gate: fetch_context runs LLM-free; the LLM arms
    // bail individually when no model resolves.
    let model_path = chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir);
    let models = ModelsConfig::load(data_dir).unwrap_or_else(|e| {
        tracing::error!("models.toml unreadable, running local: {e:#}");
        ModelsConfig::default()
    });
    let Some(job) = storage::claim_ai_job(&conn, job_id)? else {
        bail!("ai job {job_id} is not eligible")
    };
    match run_routed(
        &conn,
        &config,
        data_dir,
        model_path.as_deref(),
        &models,
        &job,
    ) {
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
        Err(e) if e.is::<DeferJob>() => {
            tracing::warn!(job_id, kind = %job.kind, "ai job deferred: {e}");
            storage::defer_ai_job(&conn, job_id, &e.to_string())?;
            Ok(())
        }
        Err(e) => {
            tracing::error!(job_id, kind = %job.kind, "ai job failed: {e:#}");
            storage::fail_ai_job(&conn, job_id, &format!("{e:#}"))?;
            Err(e)
        }
    }
}

/// Route the job (m31): cloud when `models.toml` says so and the daily cap
/// has room, else local; a cloud failure falls back to local, or defers the
/// job when there is no local model.
fn run_routed(
    conn: &rusqlite::Connection,
    config: &Config,
    data_dir: &Path,
    model_path: Option<&Path>,
    models: &ModelsConfig,
    job: &chronicle_core::storage::AiJobRow,
) -> anyhow::Result<String> {
    use chronicle_core::storage;
    if job.kind == "fetch_context" {
        return run_ai_job(conn, config, data_dir, &Engine::None, job);
    }
    let mut cloud_reason: Option<String> = None;
    if let Some((name, cfg)) = models.route_for(&job.kind) {
        let day_start = day_start_ms();
        let spent = storage::cost_today(conn, day_start)?;
        if models.max_usd_per_day > 0.0 && spent >= models.max_usd_per_day {
            let today = Zoned::now().date().to_string();
            let _ = storage::set_meta(conn, &format!("cloud_cap_hit:{today}"), Some(&today));
            cloud_reason = Some(format!(
                "cloud: daily cap reached (${spent:.2} of ${:.2})",
                models.max_usd_per_day
            ));
        } else {
            match cloud::build(name, cfg) {
                Ok(backend) => {
                    let engine = Engine::Cloud {
                        name: name.to_owned(),
                        model: cfg.model.clone(),
                        backend,
                        usage: RefCell::new(None),
                    };
                    match run_ai_job(conn, config, data_dir, &engine, job) {
                        Ok(result) => {
                            if let Engine::Cloud { usage, .. } = &engine
                                && let Some(c) = usage.borrow().as_ref()
                            {
                                tracing::info!(
                                    job_id = job.id,
                                    backend = name,
                                    input = c.input_tokens,
                                    cache_read = c.cache_read_tokens,
                                    output = c.output_tokens,
                                    "cloud usage"
                                );
                                storage::record_ai_job_usage(
                                    conn,
                                    job.id,
                                    name,
                                    i64::from(c.input_tokens),
                                    i64::from(c.output_tokens),
                                    cloud::cost_usd(&cfg.model, c),
                                )?;
                                note_redactions(conn, &c.redactions);
                            }
                            return Ok(result);
                        }
                        // A cloud failure is about the wire, not the job;
                        // anything else (no evidence, bad payload) is final.
                        Err(e) if e.is::<CloudError>() => cloud_reason = Some(e.to_string()),
                        Err(e) => return Err(e),
                    }
                }
                Err(e) => cloud_reason = Some(format!("cloud: {e}")),
            }
        }
    }
    match (model_path, cloud_reason) {
        (Some(path), reason) => {
            if let Some(r) = &reason {
                tracing::warn!(job_id = job.id, "{r}; running local");
            }
            let engine = Engine::Local(Describer::load(path)?);
            run_ai_job(conn, config, data_dir, &engine, job)
        }
        (None, Some(reason)) => Err(DeferJob(reason).into()),
        (None, None) => Err(anyhow::anyhow!(
            "no model available; run `chronicle model pull` or add a cloud backend in Settings"
        )),
    }
}

/// Record which redaction classes fired on a cloud request today (m36
/// chunk 1), for the Settings egress line. Best effort: a meta write must
/// never fail the job.
pub(crate) fn note_redactions(
    conn: &rusqlite::Connection,
    classes: &[chronicle_derive::redact::Class],
) {
    if classes.is_empty() {
        return;
    }
    let labels: Vec<&str> = classes.iter().map(|c| c.label()).collect();
    let today = Zoned::now().date().to_string();
    if let Err(e) = chronicle_core::storage::note_redactions(conn, &today, &labels) {
        tracing::warn!("redaction note failed: {e}");
    }
}

/// How many past corrections a naming prompt sees (m36 chunk 2).
const EXAMPLES_K: usize = 4;

/// The past corrections nearest these spans' work (m36 chunk 2), for the
/// digest's "Past corrections" section. Best effort: a lookup failure is
/// a prompt without examples, never a failed job.
pub(crate) fn nearest_examples(
    conn: &rusqlite::Connection,
    config: &Config,
    data_dir: &Path,
    job: JobKind,
    spans: &[SpanDraft],
) -> Vec<chronicle_core::types::Correction> {
    let text = span_query_text(spans);
    if text.is_empty() {
        return Vec::new();
    }
    let examples =
        chronicle_derive::examples::Examples::open(config.embed_model.as_deref(), data_dir);
    match examples.nearest(conn, job, &text, EXAMPLES_K) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("example lookup failed: {e}");
            Vec::new()
        }
    }
}

/// The distinct "app: title" lines of the focus spans, in first-seen
/// order: the same shape a correction's stored context has.
pub(crate) fn span_query_text(spans: &[SpanDraft]) -> String {
    use chronicle_core::sessionizer::SpanKind;
    let mut seen = std::collections::HashSet::new();
    let mut out = String::new();
    for s in spans
        .iter()
        .filter(|s| s.kind == SpanKind::Focus && !s.title.is_empty())
    {
        let line = format!("{} {}", s.app, s.title);
        if seen.insert(line.clone()) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// Local midnight, epoch ms: the daily cap's window.
pub(crate) fn day_start_ms() -> i64 {
    Zoned::now()
        .start_of_day()
        .map(|z| z.timestamp().as_millisecond())
        .unwrap_or(0)
}

/// Which engine answers the describer family for one job.
pub(crate) enum Engine {
    Local(Describer),
    Cloud {
        name: String,
        model: String,
        backend: Box<dyn TextBackend>,
        /// The one completion this job made, for `ai_jobs` usage columns.
        usage: RefCell<Option<Completion>>,
    },
    /// No model at all (fetch_context needs none; any LLM arm bails).
    None,
}

impl Engine {
    fn complete(&self, job: JobKind, prompt: &str) -> anyhow::Result<String> {
        match self {
            Engine::Local(d) => d.complete(job, prompt),
            Engine::Cloud { backend, usage, .. } => {
                let schema: Option<serde_json::Value> = match job {
                    JobKind::Checkpoint => Some(serde_json::from_str(prompts::CHECKPOINT_SCHEMA)?),
                    JobKind::SuggestTask | JobKind::NameTask => {
                        Some(serde_json::from_str(prompts::SUGGEST_SCHEMA)?)
                    }
                    _ => None,
                };
                // The frozen prefix rides in `system` so the provider
                // caches it (m36 chunk 1); the digest is the user turn.
                let prompt = strip_no_think(prompt);
                let split = prompts::split_prefix(job, prompt);
                let req = Request {
                    job,
                    system: split.as_ref().map(|s| s.system.as_str()),
                    user: split.as_ref().map_or(prompt, |s| s.user.as_str()),
                    history: &[],
                    schema: schema.as_ref(),
                    max_output: 0,
                };
                let c = backend.complete(&req, &mut |_| {})?;
                let text = c.text.clone();
                *usage.borrow_mut() = Some(c);
                Ok(text)
            }
            Engine::None => bail!(
                "no model available; run `chronicle model pull` or add a cloud backend in Settings"
            ),
        }
    }

    /// Where the job ran, for logs.
    pub(crate) fn label(&self) -> String {
        match self {
            Engine::Local(_) => "local".into(),
            Engine::Cloud { name, model, .. } => format!("{name} ({model})"),
            Engine::None => "none".into(),
        }
    }

    fn describe_task(
        &self,
        label: &str,
        project: Option<&str>,
        evidence: &str,
    ) -> anyhow::Result<String> {
        let prompt = prompts::render_description(label, project, evidence);
        non_empty(
            self.complete(JobKind::TaskDescription, &prompt)?,
            "description",
        )
    }

    fn journal_entry(
        &self,
        label: &str,
        project: Option<&str>,
        context: &str,
        truth: &str,
        evidence: &str,
    ) -> anyhow::Result<String> {
        let prompt = prompts::render_journal(label, project, context, truth, evidence);
        non_empty(self.complete(JobKind::Journal, &prompt)?, "journal entry")
    }

    fn narrative(&self, digest: &str) -> anyhow::Result<String> {
        let prompt = prompts::render_narrative(digest);
        non_empty(self.complete(JobKind::Narrative, &prompt)?, "narrative")
    }

    fn standup(&self, digest: &str) -> anyhow::Result<String> {
        let prompt = prompts::render_standup(digest);
        non_empty(self.complete(JobKind::Standup, &prompt)?, "standup draft")
    }

    fn checkpoint(
        &self,
        label: &str,
        project: Option<&str>,
        context: &str,
        journal: &str,
    ) -> anyhow::Result<(String, String)> {
        let prompt = prompts::render_checkpoint(label, project, context, journal);
        let (state, next) =
            prompts::parse_checkpoint(&self.complete(JobKind::Checkpoint, &prompt)?)?;
        Ok((clip(state, 300), clip(next, 300)))
    }

    /// `job` is `SuggestTask` or `NameTask`; both use the suggestion prompt.
    fn suggest_task(&self, job: JobKind, digest: &str) -> anyhow::Result<SuggestedTask> {
        let prompt = prompts::render_suggest(digest);
        let mut s = prompts::parse_suggest(&self.complete(job, &prompt)?)?;
        // Glyphs quoted from a terminal title must not become a label.
        s.label = clip(
            chronicle_core::evidence::strip_glyphs(&s.label).to_owned(),
            160,
        );
        s.project = s.project.map(|p| clip(p, 160));
        s.description = s.description.map(|d| clip(d, 160));
        Ok(s)
    }
}

/// The grammar's per-field cap, applied after the parse so a cloud model
/// (schema only, no length bound) lands where the local one does.
fn clip(s: String, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s;
    }
    s.chars().take(max_chars).collect()
}

/// A cloud route failed and no local model can take the job: back to the
/// queue with the reason, attempt refunded (m31).
#[derive(Debug)]
pub(crate) struct DeferJob(String);

impl std::fmt::Display for DeferJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DeferJob {}

/// An AI job whose premise no longer holds: terminal but not a failure.
#[derive(Debug)]
pub(crate) struct SkipJob(String);

impl std::fmt::Display for SkipJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SkipJob {}

/// The spans that may name a new task (m32 chunk 4): every span with
/// identifying evidence (an item, change, branch, session, event or place),
/// every span with no anchors at all, and a document or site span only
/// when its document or site holds [`profile::NAMING_SHARE`] of the
/// range's focus. The page glanced at for two minutes of an hour must not
/// name the hour. Nothing is dropped when that would leave no focus span.
fn naming_spans(
    spans: Vec<SpanDraft>,
    anchored: &[chronicle_core::profile::AnchoredSpan],
    lo: i64,
    hi: i64,
) -> Vec<SpanDraft> {
    use chronicle_core::extract::AnchorKind;
    use chronicle_core::profile::NAMING_SHARE;
    let overlap =
        |s: &chronicle_core::profile::AnchoredSpan| (s.end_ts.min(hi) - s.start_ts.max(lo)).max(0);
    let total: i64 = anchored.iter().map(overlap).sum();
    if total <= 0 {
        return spans;
    }
    let mut minor: std::collections::HashMap<(AnchorKind, &str), i64> =
        std::collections::HashMap::new();
    for s in anchored {
        for a in &s.anchors {
            if matches!(a.kind, AnchorKind::Doc | AnchorKind::Domain) {
                *minor.entry((a.kind, a.value.as_str())).or_default() += overlap(s);
            }
        }
    }
    let floor = (total as f64 * NAMING_SHARE) as i64;
    let dropped: std::collections::HashSet<(i64, i64)> = anchored
        .iter()
        .filter(|s| {
            let identifying = s.anchors.iter().any(|a| {
                !matches!(
                    a.kind,
                    AnchorKind::Doc | AnchorKind::Domain | AnchorKind::People
                )
            });
            let named = s
                .anchors
                .iter()
                .filter(|a| matches!(a.kind, AnchorKind::Doc | AnchorKind::Domain))
                .map(|a| minor.get(&(a.kind, a.value.as_str())).copied().unwrap_or(0))
                .max();
            !identifying && named.is_some_and(|m| m < floor)
        })
        .map(|s| (s.start_ts, s.end_ts))
        .collect();
    if dropped.is_empty() {
        return spans;
    }
    let kept: Vec<SpanDraft> = spans
        .iter()
        .filter(|s| {
            s.kind != chronicle_core::sessionizer::SpanKind::Focus
                || !dropped.contains(&(s.start.as_millisecond(), s.end.as_millisecond()))
        })
        .cloned()
        .collect();
    if kept
        .iter()
        .any(|s| s.kind == chronicle_core::sessionizer::SpanKind::Focus)
    {
        kept
    } else {
        spans
    }
}

/// The ground truth under every interval of a task (m32 chunk 5): commit
/// subjects, sessions, notes, PRs and meetings, rendered with source tags.
fn task_truth(conn: &rusqlite::Connection, task_id: i64, tz: &TimeZone) -> anyhow::Result<String> {
    let (lo, hi): (Option<i64>, Option<i64>) = conn.query_row(
        "SELECT MIN(start_ts), MAX(end_ts) FROM intervals WHERE task_id=?1",
        [task_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    let (Some(lo), Some(hi)) = (lo, hi) else {
        return Ok(String::new());
    };
    let rows = chronicle_core::storage::activity_for_task_in_range(conn, task_id, lo, hi)?;
    Ok(chronicle_core::digest::ground_truth_within(
        &rows,
        tz,
        TRUTH_CHARS,
    ))
}

/// Ground truth in a description or journal prompt, in chars: the local
/// window has room for the window titles (2000 chars) and this.
const TRUTH_CHARS: usize = 1600;

/// The standup digest as a whole, in chars (m32 chunk 5): the local
/// model's 4096-token window minus the template, a 512-token reply and
/// headroom, at the digest's 8/3 chars per token. Shared out per task so
/// the last task of a busy day is not the one the tokenizer cuts.
const STANDUP_DIGEST_CHARS: usize = chronicle_core::digest::max_chars(2600);
/// No task's share of the standup digest goes under this.
const STANDUP_TASK_MIN_CHARS: usize = 600;
/// Journal entries per task in the standup digest, and their length: the
/// model copies them, so they are what the reply's length follows.
const STANDUP_ENTRIES: usize = 3;
const STANDUP_ENTRY_CHARS: usize = 220;

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
    engine: &Engine,
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
    tracing::debug!(job_id = job.id, kind = %job.kind, engine = %engine.label(), "ai job engine");
    match job.kind.as_str() {
        "task_description" => {
            let task_id = payload["task_id"]
                .as_i64()
                .context("payload lacks task_id")?;
            let (label, project) = task_label_project(conn, task_id)?;
            // Window titles first, then what cannot be wrong; a task with
            // neither is skipped, never failed (m32 chunk 5).
            let spans = storage::task_evidence_text(conn, task_id)?;
            let truth = task_truth(conn, task_id, &TimeZone::system())?;
            let evidence = match (spans.trim().is_empty(), truth.trim().is_empty()) {
                (true, true) => {
                    return Err(SkipJob(format!("no evidence for task {task_id}")).into());
                }
                (false, true) => spans,
                (true, false) => format!("(no window titles)\nGround truth:\n{truth}"),
                (false, false) => format!("{spans}Ground truth:\n{truth}"),
            };
            let desc = engine.describe_task(&label, project.as_deref(), &evidence)?;
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
            let examples = nearest_examples(conn, config, data_dir, JobKind::SuggestTask, &spans);
            let digest = chronicle_core::digest::build_digest(
                &spans,
                &tz,
                &[],
                &examples,
                &[],
                &[],
                None,
                None,
                None,
            );
            let s = engine.suggest_task(JobKind::SuggestTask, &digest)?;
            Ok(serde_json::to_string(&s)?)
        }
        "name_task" => {
            // The segmenter created a task for a new cluster under a
            // placeholder label (m30 chunk 3); name it from the stretch's
            // own spans. A label the user changed meanwhile stays.
            let task_id = payload["task_id"]
                .as_i64()
                .context("payload lacks task_id")?;
            let lo = payload["lo"].as_i64().context("payload lacks lo")?;
            let hi = payload["hi"].as_i64().context("payload lacks hi")?;
            let placeholder = payload["placeholder"].as_str().unwrap_or("").to_owned();
            let spans = naming_spans(
                storage::spans_in_range(conn, lo, hi)?,
                &storage::anchored_spans(conn, lo, hi)?,
                lo,
                hi,
            );
            if spans
                .iter()
                .all(|s| s.kind != chronicle_core::sessionizer::SpanKind::Focus)
            {
                bail!("no focus activity to name task {task_id} from");
            }
            let tz = TimeZone::system();
            let examples = nearest_examples(conn, config, data_dir, JobKind::NameTask, &spans);
            let digest = chronicle_core::digest::build_digest(
                &spans,
                &tz,
                &[],
                &examples,
                &[],
                &[],
                None,
                None,
                None,
            );
            let s = engine.suggest_task(JobKind::NameTask, &digest)?;
            let n = conn.execute(
                "UPDATE tasks SET label=?1, project=COALESCE(project, ?2)
                 WHERE id=?3 AND source='derived' AND label=?4",
                rusqlite::params![s.label, s.project, task_id, placeholder],
            )?;
            if n == 0 {
                bail!("task {task_id} was renamed or removed before naming");
            }
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
            let tz = TimeZone::system();
            // The batch's ground truth (m32 chunk 5) can carry an entry on
            // its own; only a batch with neither it nor window titles bails.
            let truth = chronicle_core::digest::ground_truth_within(
                &storage::activity_for_task_in_range(conn, task_id, lo, hi)?,
                &tz,
                TRUTH_CHARS,
            );
            if evidence.trim().is_empty() && truth.trim().is_empty() {
                bail!("no evidence for task {task_id} in batch {batch_id}");
            }
            let (label, project) = task_label_project(conn, task_id)?;
            // Head of the ticket context only: the prompt budget belongs to
            // the session evidence, which truncation cuts from the tail.
            let context: String = storage::task_context(conn, task_id)?
                .map(|(_, c)| c.chars().take(1200).collect())
                .unwrap_or_default();
            let entry =
                engine.journal_entry(&label, project.as_deref(), &context, &truth, &evidence)?;
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
                engine.checkpoint(&label, project.as_deref(), &context, &journal)?;
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
            let text = engine.narrative(&digest)?;
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
            let mut rows = storage::standup_digest(conn, lo, hi)?;
            // Each task's ground truth for the day (m32 chunk 5); a task
            // with truth but no journal still gets a block.
            let mut truth: std::collections::HashMap<
                i64,
                Vec<chronicle_core::types::ActivityEvent>,
            > = std::collections::HashMap::new();
            for row in &rows {
                let events = storage::activity_for_task_in_range(conn, row.task_id, lo, hi)?;
                truth.insert(row.task_id, events);
            }
            let mut seen: std::collections::HashSet<i64> = truth.keys().copied().collect();
            for t in storage::tasks_in_range(conn, lo, hi)? {
                if !seen.insert(t.id) {
                    continue;
                }
                let events = storage::activity_for_task_in_range(conn, t.id, lo, hi)?;
                if chronicle_core::digest::ground_truth(&events, &tz)
                    .trim()
                    .is_empty()
                {
                    continue;
                }
                truth.insert(t.id, events);
                rows.push(storage::StandupDigestRow {
                    task_id: t.id,
                    label: t.label,
                    project: t.project,
                    external_ref: t.external_ref,
                    entries: Vec::new(),
                    checkpoint: None,
                });
            }
            // Project-major (m35 chunk 4): the draft's blocks come out
            // grouped as the DATA is ordered — configured projects first,
            // then the rest by name, no project last.
            let order: Vec<String> = config
                .projects_effective()
                .iter()
                .map(|p| p.name.clone())
                .collect();
            rows.sort_by_key(|r| match &r.project {
                Some(p) => (
                    order.iter().position(|o| o == p).unwrap_or(order.len()),
                    p.clone(),
                ),
                None => (usize::MAX, String::new()),
            });
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
                let digest = standup_digest_text(&rows, &tz, plan.as_deref(), &truth);
                tracing::debug!(job_id = job.id, "standup digest:\n{digest}");
                let draft = engine.standup(&digest)?;
                // A claim without a source is not a claim (m32 chunk 5).
                let Some(text) = prompts::keep_sourced(&draft) else {
                    bail!("standup draft for {day} carried no sourced claim: {draft}");
                };
                if text.lines().count() < draft.lines().filter(|l| !l.trim().is_empty()).count() {
                    tracing::debug!(
                        job_id = job.id,
                        "standup draft before the source check:\n{draft}"
                    );
                }
                text
            };
            storage::upsert_standup_draft(conn, day, Timestamp::now(), &text)?;
            Ok(text)
        }
        other => bail!("unknown ai job kind {other}"),
    }
}

/// Render the standup digest rows for the prompt: the day's plan when one
/// was set (m26), then per task the day's journal tail (last
/// [`STANDUP_ENTRIES`], clipped) plus the fresh checkpoint if one exists,
/// then as much of the task's ground truth as its share of
/// [`STANDUP_DIGEST_CHARS`] holds (m32 chunk 5). Every line ends in the
/// source tag a claim built on it must carry.
pub(crate) fn standup_digest_text(
    rows: &[chronicle_core::storage::StandupDigestRow],
    tz: &TimeZone,
    plan: Option<&str>,
    truth: &std::collections::HashMap<i64, Vec<chronicle_core::types::ActivityEvent>>,
) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    if let Some(plan) = plan.map(str::trim).filter(|s| !s.is_empty()) {
        let _ = writeln!(out, "## Plan\n{plan}");
    }
    let share = (STANDUP_DIGEST_CHARS / rows.len().max(1)).max(STANDUP_TASK_MIN_CHARS);
    for row in rows {
        let from = out.chars().count();
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
        let skip = row.entries.len().saturating_sub(STANDUP_ENTRIES);
        for e in &row.entries[skip..] {
            let hm = chronicle_core::types::ms_to_ts(e.start_ts)
                .to_zoned(tz.clone())
                .strftime("%H:%M");
            let _ = writeln!(
                out,
                "- [{hm}] {} [journal {hm}]",
                clip_prose(&e.entry, STANDUP_ENTRY_CHARS)
            );
        }
        if let Some(c) = &row.checkpoint {
            let _ = writeln!(
                out,
                "Checkpoint: {} [checkpoint]",
                clip_prose(&c.state, STANDUP_ENTRY_CHARS)
            );
            let _ = writeln!(
                out,
                "Next steps: {} [checkpoint]",
                clip_prose(&c.next_steps, STANDUP_ENTRY_CHARS)
            );
        }
        let left = share.saturating_sub(out.chars().count() - from);
        if let Some(events) = truth.get(&row.task_id)
            && left > 0
        {
            let g = chronicle_core::digest::ground_truth_within(events, tz, left);
            if !g.trim().is_empty() {
                let _ = write!(out, "Ground truth:\n{g}");
            }
        }
        out.push('\n');
    }
    out
}

/// Prose cut to `max_chars` at its last sentence end past two fifths of
/// the way, else at its last space: the model copies these lines, so a cut
/// mid-word would be copied too.
fn clip_prose(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_owned();
    }
    let head: String = s.chars().take(max_chars).collect();
    let floor = head.len() * 2 / 5;
    let at = head
        .rfind(". ")
        .map(|i| i + 1)
        .filter(|&i| i >= floor)
        .or_else(|| head.rfind(' '))
        .unwrap_or(head.len());
    head[..at].trim_end().to_owned()
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
        let dur = t.weigh(t.end_ts.as_millisecond().min(hi) - t.start_ts.as_millisecond().max(lo));
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

#[cfg(test)]
mod tests {
    use super::*;
    use chronicle_core::extract::{Anchor, AnchorKind};
    use chronicle_core::profile::AnchoredSpan;
    use chronicle_core::sessionizer::SpanKind;
    use chronicle_core::types::ms_to_ts;

    const MIN: i64 = 60_000;

    fn draft(lo: i64, hi: i64, title: &str) -> SpanDraft {
        SpanDraft {
            start: ms_to_ts(lo),
            end: ms_to_ts(hi),
            app: "code".into(),
            title: title.into(),
            kind: SpanKind::Focus,
            url: None,
            quiet_ms: 0,
        }
    }

    fn anchored(
        id: i64,
        lo: i64,
        hi: i64,
        title: &str,
        anchors: &[(AnchorKind, &str)],
    ) -> AnchoredSpan {
        AnchoredSpan {
            id,
            start_ts: lo,
            end_ts: hi,
            app: "code".into(),
            title: title.into(),
            anchors: anchors
                .iter()
                .map(|(k, v)| Anchor {
                    kind: *k,
                    value: (*v).to_owned(),
                })
                .collect(),
            vec: None,
            quiet_ms: 0,
            wrote: false,
            project: None,
        }
    }

    #[test]
    fn prose_is_cut_at_a_sentence_or_a_word() {
        assert_eq!(clip_prose("short", 10), "short");
        let two = "Merged the branch. Then ran the tests and the build again.";
        assert_eq!(clip_prose(two, 40), "Merged the branch.");
        let one = "Merged the branch and then ran every test in the workspace twice";
        assert_eq!(clip_prose(one, 40), "Merged the branch and then ran every");
        // A sentence end before the halfway mark is too short to keep.
        let early = "Ok. Then a very long second sentence that keeps going on and on";
        assert_eq!(clip_prose(early, 30), "Ok. Then a very long second");
    }

    #[test]
    fn a_minor_page_does_not_name_the_task() {
        let drafts = vec![
            draft(0, 50 * MIN, "billing.rs - app"),
            draft(50 * MIN, 55 * MIN, "Q4 pricing - Notion"),
            draft(55 * MIN, 60 * MIN, "scratch"),
        ];
        let spans = vec![
            anchored(
                1,
                0,
                50 * MIN,
                "billing.rs - app",
                &[(AnchorKind::Place, "app")],
            ),
            anchored(
                2,
                50 * MIN,
                55 * MIN,
                "Q4 pricing - Notion",
                &[(AnchorKind::Doc, "Q4 pricing")],
            ),
            anchored(3, 55 * MIN, 60 * MIN, "scratch", &[]),
        ];
        // The five-minute page goes; the place span and the anchorless
        // span stay.
        let kept = naming_spans(drafts.clone(), &spans, 0, 60 * MIN);
        let titles: Vec<&str> = kept.iter().map(|s| s.title.as_str()).collect();
        assert_eq!(titles, ["billing.rs - app", "scratch"]);
        // Over its own twenty minutes the page holds a quarter and stays.
        let kept = naming_spans(drafts.clone(), &spans, 40 * MIN, 60 * MIN);
        assert_eq!(kept.len(), 3);
        // Dropping everything drops nothing.
        let only = vec![draft(50 * MIN, 55 * MIN, "Q4 pricing - Notion")];
        let kept = naming_spans(only.clone(), &spans, 0, 60 * MIN);
        assert_eq!(kept, only);
    }
}
