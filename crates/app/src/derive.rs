use std::path::Path;
use std::time::{Duration, Instant};

use crate::daemon::bump_day_counter_by;
use crate::status::init_logging;
use anyhow::bail;
use chronicle_core::config::Config;
use jiff::{Timestamp, tz::TimeZone};

/// One JSON object per line over the resident derive worker's stdio.
pub(crate) mod deriveproto {
    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    #[serde(tag = "t", rename_all = "snake_case")]
    pub enum Request {
        Derive {
            batch_id: i64,
        },
        /// Live tier (chunk 4): label the stretch `[lo, hi)` of the tail.
        Live {
            lo: i64,
            hi: i64,
        },
        /// Day tier (chunk 6): merge and rename the day's derived tasks.
        Consolidate {
            lo: i64,
            hi: i64,
        },
    }

    #[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize)]
    #[serde(tag = "t", rename_all = "snake_case")]
    pub enum Reply {
        /// Model loaded and prefix cached; requests accepted.
        Ready,
        /// What the model is saying so far, already resolved for humans
        /// ("<label being typed>…" or "linking to <open task>"); sent when
        /// it changes. The feed's "deriving…" row shows it.
        Progress { label: String },
        /// `batch_id` None = a live pass.
        Done {
            batch_id: Option<i64>,
            intervals: usize,
        },
        Err {
            batch_id: Option<i64>,
            message: String,
        },
    }
}

/// Accumulates the model's raw output and reports the human-readable
/// progress label whenever it changes.
#[derive(Default)]
pub(crate) struct ProgressRelay {
    raw: String,
    last: String,
}

impl ProgressRelay {
    fn push(
        &mut self,
        piece: &str,
        open: &[chronicle_core::types::OpenTask],
        on_progress: &mut dyn FnMut(&str),
    ) {
        self.raw.push_str(piece);
        if let Some(label) = progress_label(&self.raw, open)
            && label != self.last
        {
            self.last = label;
            on_progress(&self.last);
        }
    }
}

/// What the partial JSON says so far: the newest `label` being typed
/// (with an ellipsis while its string is still open), else "linking to
/// <task>" for the newest `ref`. None until the model has said either.
pub(crate) fn progress_label(
    partial: &str,
    open: &[chronicle_core::types::OpenTask],
) -> Option<String> {
    let label_at = partial.rfind("\"label\"");
    let ref_at = partial.rfind("\"ref\"");
    if let Some(at) = label_at
        && ref_at.is_none_or(|r| r < at)
    {
        let rest = partial[at + "\"label\"".len()..].trim_start_matches([':', ' ', '\n', '\t']);
        if let Some(body) = rest.strip_prefix('"') {
            let mut text = String::new();
            let mut chars = body.chars();
            let mut closed = false;
            while let Some(c) = chars.next() {
                match c {
                    '"' => {
                        closed = true;
                        break;
                    }
                    '\\' => {
                        if let Some(n) = chars.next() {
                            text.push(n);
                        }
                    }
                    c => text.push(c),
                }
            }
            if text.is_empty() {
                return None;
            }
            return Some(if closed {
                text
            } else {
                format!("{text}\u{2026}")
            });
        }
        // `null` (or the start of it) means the ref names the task.
        if (rest.starts_with("null") || "null".starts_with(rest))
            && let Some(r) = ref_at
        {
            return ref_label(&partial[r..], open);
        }
        return None;
    }
    ref_at.and_then(|r| ref_label(&partial[r..], open))
}

pub(crate) fn ref_label(
    from_ref: &str,
    open: &[chronicle_core::types::OpenTask],
) -> Option<String> {
    let rest = from_ref["\"ref\"".len()..].trim_start_matches([':', ' ', '\n', '\t']);
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    let n: usize = digits.parse().ok()?;
    let task = open.get(n.checked_sub(1)?)?;
    Some(format!("linking to {}", task.label))
}

/// Day tier (m27 chunk 6): fold the day's orphans, then let the model merge
/// duplicates and rename fresh labels among today's derived tasks; every
/// suggestion passes `consolidate::guard`. One `consolidate` correction
/// records the run for undo. Returns the number of changes applied.
pub(crate) fn consolidate_day(
    conn: &mut rusqlite::Connection,
    config: &Config,
    data_dir: &Path,
    model: &chronicle_derive::DeriveModel,
    lo: i64,
    hi: i64,
    on_progress: &mut dyn FnMut(&str),
) -> anyhow::Result<usize> {
    use chronicle_core::consolidate::{self, Plan};
    use chronicle_core::storage;
    let day = chronicle_core::types::ms_to_ts(lo)
        .to_zoned(TimeZone::system())
        .date()
        .to_string();
    let tasks = storage::day_tasks(conn, lo, hi)?;
    let intervals = storage::day_intervals(conn, lo, hi)?;
    let mut plan = Plan {
        merges: consolidate::orphan_folds(&tasks, &intervals),
        renames: Vec::new(),
    };
    let folded: std::collections::BTreeSet<i64> = plan.merges.iter().map(|m| m.0).collect();
    let remaining: Vec<consolidate::DayTask> = tasks
        .iter()
        .filter(|t| !folded.contains(&t.id))
        .cloned()
        .collect();
    if remaining.iter().filter(|t| !t.locked).count() >= 2 {
        let mut input = consolidate::render_input(&remaining);
        // The user's own merges and renames of similar work (m36 chunk 2).
        {
            let text: String = remaining
                .iter()
                .filter(|t| !t.locked)
                .flat_map(|t| t.evidence.iter().take(3))
                .map(|(app, title, _)| format!("{app} {title}\n"))
                .collect();
            let examples =
                chronicle_derive::examples::Examples::open(config.embed_model.as_deref(), data_dir);
            match examples.nearest(conn, chronicle_derive::text::JobKind::Consolidate, &text, 4) {
                Ok(cs) => input.push_str(&chronicle_derive::examples::render_section(&cs)),
                Err(e) => tracing::warn!("consolidate examples failed: {e}"),
            }
        }
        // The heavy model, when configured, is loaded for this run only.
        let heavy = config
            .model_path_heavy
            .as_deref()
            .filter(|p| p.exists())
            .map(chronicle_derive::DeriveModel::load)
            .transpose()?;
        let m = heavy.as_ref().unwrap_or(model);
        let mut session = m.session_with(
            chronicle_derive::Prompt::Consolidate,
            chronicle_derive::N_CTX,
        )?;
        let mut relay = ProgressRelay::default();
        let run =
            session.infer_consolidate(&input, &mut |piece| relay.push(piece, &[], on_progress))?;
        storage::set_meta(conn, "consolidate_last_output", Some(&run.raw))?;
        let guarded = consolidate::guard(&remaining, &run.plan, consolidate::MAX_MERGES);
        tracing::info!(
            model_merges = run.plan.merges.len(),
            model_renames = run.plan.renames.len(),
            kept_merges = guarded.merges.len(),
            kept_renames = guarded.renames.len(),
            "consolidation guarded"
        );
        plan.merges.extend(guarded.merges);
        plan.renames = guarded.renames;
    }
    if plan.is_empty() {
        storage::stamp_consolidated(conn, &day, 0)?;
        tracing::info!(%day, "consolidation: nothing to change");
        return Ok(0);
    }
    let id = storage::consolidate_apply(conn, Timestamp::now(), &day, &plan)?;
    tracing::info!(%day, correction = id, merges = plan.merges.len(), renames = plan.renames.len(), "consolidation applied");
    Ok(plan.merges.len() + plan.renames.len())
}

/// Live tier (m27 chunk 4): label the stretch `[lo, hi)` of the tail with
/// the live prompt and store one `source='live'` interval (confidence ×
/// 0.8). Returns the task it landed on, None when the window holds no spans.
pub(crate) fn live_pass(
    conn: &mut rusqlite::Connection,
    config: &Config,
    session: &mut chronicle_derive::DeriveSession<'_>,
    lo: i64,
    hi: i64,
    on_progress: &mut dyn FnMut(&str),
) -> anyhow::Result<Option<i64>> {
    use chronicle_core::types::IntervalDraft;
    use chronicle_core::{merge, storage};
    let spans = storage::spans_in_range(conn, lo, hi)?;
    if spans.is_empty() {
        return Ok(None);
    }
    let mut open = storage::open_tasks(conn, LIVE_OPEN_CAP)?;
    let hints = storage::prepass_hints(conn, lo, hi)?;
    for h in &hints {
        if !open.iter().any(|t| t.id == h.task_id)
            && let Some(t) = storage::open_task_by_id(conn, h.task_id)?
        {
            open.push(t);
        }
    }
    let activity = storage::activity_in_range(conn, lo, hi)?;
    let tz = TimeZone::system();
    let ticket_re = regex::Regex::new(&config.ticket_regex).ok();
    let mut digest = chronicle_core::digest::build_digest(
        &spans,
        &tz,
        &open,
        &[],
        &hints,
        &activity,
        None,
        ticket_re.as_ref(),
        None,
    );
    if let Some(prev) = storage::label_before(conn, lo)? {
        digest.push_str(&format!("\n## Previously\n{prev}\n"));
    }
    let mut relay = ProgressRelay::default();
    let run = session.infer_live(&digest, &mut |piece| relay.push(piece, &open, on_progress))?;
    let mins = ((hi - lo) / 60_000).max(1);
    let draft = IntervalDraft {
        task_ref: run.draft.task_ref,
        label: run.draft.label,
        project: run.draft.project,
        start_offset_min: 0,
        end_offset_min: mins,
        confidence: run.draft.confidence,
    };
    let drafts = merge::sanitize_intervals(vec![draft], open.len());
    let (slots, linked) = merge::link_intervals(&drafts, &open);
    let Some(iv) = linked.first() else {
        bail!("live output rejected: {}", run.raw.trim())
    };
    let slot = &slots[iv.slot];
    let confidence = (iv.confidence * LIVE_CONFIDENCE_SCALE).clamp(0.0, 1.0);
    let task_id = storage::insert_live_interval(conn, slot, lo, hi, confidence)?;
    tracing::info!(task_id, lo, hi, confidence, "live pass placed");
    Ok(Some(task_id))
}

/// Open tasks offered to the batch prompt; 8 → 16 in m27 chunk 5, paid for
/// by chunk 1's shorter output.
pub(crate) const OPEN_CAP: usize = 16;
/// Open tasks offered to the live prompt.
pub(crate) const LIVE_OPEN_CAP: usize = 16;
/// A live label is a guess over a short window; the batch tier confirms it.
pub(crate) const LIVE_CONFIDENCE_SCALE: f64 = 0.8;

/// Everything the model sees for one batch, plus what post-processing needs.
pub(crate) struct BatchDigest {
    pub(crate) digest: String,
    pub(crate) open: Vec<chronicle_core::types::OpenTask>,
    pub(crate) spans: Vec<chronicle_core::sessionizer::SpanDraft>,
    /// AFK gaps ≥ 5 min in window minutes (coalesce boundaries).
    pub(crate) gaps: Vec<(i64, i64)>,
    pub(crate) activity: Vec<chronicle_core::types::ActivityEvent>,
}

/// Build the batch prompt the way production builds it, so bench and the
/// replay eval score the same digest the worker would send (m27 chunk 2
/// parity). `open` is the caller's open-task list; pre-pass hints over the
/// window are appended so the model can link to them by ref.
pub(crate) fn build_batch_digest(
    conn: &rusqlite::Connection,
    config: &Config,
    data_dir: &Path,
    batch: &chronicle_core::storage::BatchRow,
    open: Vec<chronicle_core::types::OpenTask>,
    mcp: bool,
) -> anyhow::Result<BatchDigest> {
    let spans = chronicle_core::storage::batch_spans(conn, batch.id)?;
    build_window_digest(
        conn,
        config,
        data_dir,
        batch.start_ts,
        batch.end_ts,
        spans,
        open,
        mcp,
    )
}

/// The tail digest as the worker would build it if the tail closed now —
/// the inspector's "show digest" (m27 chunk 7). No MCP fetch (it would
/// block the UI); "(no tail spans)" when nothing is unbatched.
pub(crate) fn tail_digest(
    conn: &rusqlite::Connection,
    config: &Config,
    data_dir: &Path,
) -> anyhow::Result<String> {
    use chronicle_core::storage;
    let now = Timestamp::now().as_millisecond();
    let lo = storage::latest_batch_end(conn)?.unwrap_or(now - 12 * 3_600_000);
    let spans = storage::spans_in_range(conn, lo, now)?;
    if spans.is_empty() {
        return Ok("(no tail spans)".to_owned());
    }
    let open = storage::open_tasks(conn, OPEN_CAP)?;
    Ok(build_window_digest(conn, config, data_dir, lo, now, spans, open, false)?.digest)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn build_window_digest(
    conn: &rusqlite::Connection,
    config: &Config,
    data_dir: &Path,
    lo: i64,
    hi: i64,
    spans: Vec<chronicle_core::sessionizer::SpanDraft>,
    mut open: Vec<chronicle_core::types::OpenTask>,
    mcp: bool,
) -> anyhow::Result<BatchDigest> {
    use chronicle_core::storage;
    let hints = storage::prepass_hints(conn, lo, hi)?;
    for h in &hints {
        if !open.iter().any(|t| t.id == h.task_id)
            && let Some(t) = storage::open_task_by_id(conn, h.task_id)?
        {
            open.push(t);
        }
    }
    let corrections = storage::similar_corrections(conn, &spans, 4)?;
    let tz = TimeZone::system();
    let mcp_context = if mcp {
        let (mcp_calls, ctx) = chronicle_mcp::gather_context(&config.mcp_path(data_dir));
        // Counted per call that left the machine, empty reply or not.
        bump_day_counter_by(conn, "fetches", mcp_calls);
        ctx
    } else {
        None
    };
    let activity = storage::activity_in_range(conn, lo, hi)?;
    let ticket_re = regex::Regex::new(&config.ticket_regex).ok();
    // The day's intent (m26): "## Plan" says what the user meant this
    // window to be, so the model links ambiguous work to the plan.
    let day = chronicle_core::types::ms_to_ts(lo)
        .to_zoned(tz.clone())
        .date()
        .to_string();
    let plan = chronicle_core::intent::plan_body(conn, &day)?;
    let digest = chronicle_core::digest::build_digest(
        &spans,
        &tz,
        &open,
        &corrections,
        &hints,
        &activity,
        mcp_context.as_deref(),
        ticket_re.as_ref(),
        plan.as_deref(),
    );
    let gaps = afk_gaps_min(&spans, lo);
    Ok(BatchDigest {
        digest,
        open,
        spans,
        gaps,
        activity,
    })
}

pub(crate) fn derive_worker(data_dir: &Path, batch_id: i64) -> anyhow::Result<()> {
    use chronicle_core::storage;
    let _guard = init_logging(data_dir)?;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let mut conn = storage::open(&data_dir.join("chronicle.db"))?;
    let Some(model_path) = chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir)
    else {
        bail!("no model available; run `chronicle model pull`")
    };
    let model = chronicle_derive::DeriveModel::load(&model_path)?;
    let mut session = model.session()?;
    derive_batch(
        &mut conn,
        &config,
        data_dir,
        &mut session,
        batch_id,
        &mut |_| {},
    )
    .map(|_| ())
}

/// Resident derive worker (m27 chunk 3): the model loads once, the session
/// keeps the instruction prefix in its KV cache, and each `derive` request
/// on stdin decodes only its digest. Exits on stdin EOF or after
/// `worker_idle_secs` without a request.
pub(crate) fn derive_resident(data_dir: &Path) -> anyhow::Result<()> {
    use std::io::{BufRead, Write};

    use chronicle_core::storage;
    use crossbeam_channel::RecvTimeoutError;
    use deriveproto::{Reply, Request};

    let _guard = init_logging(data_dir)?;
    let mut stdout = std::io::stdout();
    let mut send = move |msg: &Reply| {
        let mut line = serde_json::to_string(msg).expect("reply serializes");
        line.push('\n');
        // A dead pipe means the daemon is gone; exiting quietly is correct.
        if stdout
            .write_all(line.as_bytes())
            .and_then(|()| stdout.flush())
            .is_err()
        {
            std::process::exit(0);
        }
    };
    let config = Config::load(&data_dir.join("config.toml"))?;
    let mut conn = storage::open(&data_dir.join("chronicle.db"))?;
    let Some(model_path) = chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir)
    else {
        send(&Reply::Err {
            batch_id: None,
            message: "no model available; run `chronicle model pull`".into(),
        });
        bail!("no model available")
    };
    let model = match chronicle_derive::DeriveModel::load(&model_path) {
        Ok(m) => m,
        Err(e) => {
            send(&Reply::Err {
                batch_id: None,
                message: format!("model load failed: {e:#}"),
            });
            return Err(e);
        }
    };
    let mut session = model.session()?;
    let mut live_session =
        model.session_with(chronicle_derive::Prompt::Live, chronicle_derive::LIVE_N_CTX)?;

    let (tx, rx) = crossbeam_channel::unbounded::<Request>();
    std::thread::Builder::new()
        .name("stdin".into())
        .spawn(move || {
            for line in std::io::stdin().lock().lines() {
                let Ok(line) = line else { break };
                if let Ok(req) = serde_json::from_str::<Request>(&line)
                    && tx.send(req).is_err()
                {
                    break;
                }
            }
        })?;
    send(&Reply::Ready);
    tracing::info!("derive worker ready");

    let idle = Duration::from_secs(u64::from(config.worker_idle_secs).max(1));
    loop {
        let req = match rx.recv_timeout(idle) {
            Ok(req) => req,
            Err(RecvTimeoutError::Timeout) => {
                tracing::info!("derive worker idle, exiting");
                return Ok(());
            }
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        };
        match req {
            Request::Derive { batch_id } => {
                let result = derive_batch(
                    &mut conn,
                    &config,
                    data_dir,
                    &mut session,
                    batch_id,
                    &mut |label| {
                        send(&Reply::Progress {
                            label: label.into(),
                        })
                    },
                );
                match result {
                    Ok(intervals) => send(&Reply::Done {
                        batch_id: Some(batch_id),
                        intervals,
                    }),
                    Err(e) => send(&Reply::Err {
                        batch_id: Some(batch_id),
                        message: format!("{e:#}"),
                    }),
                }
            }
            Request::Consolidate { lo, hi } => {
                let result =
                    consolidate_day(&mut conn, &config, data_dir, &model, lo, hi, &mut |label| {
                        send(&Reply::Progress {
                            label: label.into(),
                        })
                    });
                match result {
                    Ok(changes) => send(&Reply::Done {
                        batch_id: None,
                        intervals: changes,
                    }),
                    Err(e) => send(&Reply::Err {
                        batch_id: None,
                        message: format!("{e:#}"),
                    }),
                }
            }
            Request::Live { lo, hi } => {
                let result = live_pass(
                    &mut conn,
                    &config,
                    &mut live_session,
                    lo,
                    hi,
                    &mut |label| {
                        send(&Reply::Progress {
                            label: label.into(),
                        })
                    },
                );
                match result {
                    Ok(placed) => send(&Reply::Done {
                        batch_id: None,
                        intervals: usize::from(placed.is_some()),
                    }),
                    Err(e) => send(&Reply::Err {
                        batch_id: None,
                        message: format!("{e:#}"),
                    }),
                }
            }
        }
        if config.worker_idle_secs == 0 {
            return Ok(()); // one request per process
        }
    }
}

/// Claim → digest → infer → store → anchor → journal jobs for one batch.
/// Any failure marks the batch failed (retry-once via attempts cap). Returns
/// the stored interval count.
pub(crate) fn derive_batch(
    conn: &mut rusqlite::Connection,
    config: &Config,
    data_dir: &Path,
    session: &mut chronicle_derive::DeriveSession<'_>,
    batch_id: i64,
    on_progress: &mut dyn FnMut(&str),
) -> anyhow::Result<usize> {
    use chronicle_core::storage;
    let Some(batch) = storage::claim_batch(conn, batch_id)? else {
        bail!("batch {batch_id} is not eligible for derivation")
    };
    let result = (|| -> anyhow::Result<usize> {
        let t0 = Instant::now();
        let open = storage::open_tasks(conn, OPEN_CAP)?;
        let BatchDigest {
            digest,
            open,
            spans,
            gaps,
            activity,
        } = build_batch_digest(conn, config, data_dir, &batch, open, true)?;
        let mut relay = ProgressRelay::default();
        let run = session.infer(&digest, &mut |piece| relay.push(piece, &open, on_progress))?;
        let drafts = chronicle_core::merge::sanitize_intervals(run.intervals, open.len());
        let (slots, linked) = chronicle_core::merge::link_intervals(&drafts, &open);
        let linked = chronicle_core::merge::coalesce(linked, &gaps, COALESCE_GAP_MIN);
        let intervals = clamp_intervals(linked, &spans, batch.start_ts, batch.end_ts);
        let n = intervals.len();
        let stored = storage::store_derivation(conn, batch_id, &slots, &intervals)?;
        storage::record_derive_metrics(
            conn,
            &storage::DeriveMetrics {
                batch_id,
                derived_ts: Timestamp::now().as_millisecond(),
                derive_ms: t0.elapsed().as_millis() as i64,
                prompt_tokens: run.prompt_tokens as i64,
                gen_tokens: run.gen_tokens as i64,
            },
        )?;
        // The inspector's "last output" (chunk 7) reads these back.
        storage::set_meta(conn, "derive_last_output", Some(&run.raw))?;
        storage::set_meta(conn, "derive_last_batch", Some(&batch_id.to_string()))?;
        // Deterministic anchoring: majority branch names the ticket key.
        match regex::Regex::new(&config.ticket_regex) {
            Ok(re) => {
                let prior = storage::branch_state_before(conn, batch.start_ts)?;
                // Git kinds plus PR markers (title carries the key): a
                // session's `gitBranch` is not vcs activity. Cwd and shell
                // rows say which PR rows are strong (m32 chunk 4).
                let vcs: Vec<_> = activity
                    .iter()
                    .filter(|e| {
                        e.kind.is_vcs()
                            || e.kind.is_pr()
                            || matches!(
                                e.kind,
                                chronicle_core::types::ActivityKind::Cwd
                                    | chronicle_core::types::ActivityKind::Shell
                            )
                    })
                    .cloned()
                    .collect();
                // A ref comes from the task's own place: each placed task's
                // declared project, when it has one.
                let mut projects: std::collections::HashMap<i64, String> =
                    std::collections::HashMap::new();
                for (task_id, _, _) in &stored {
                    if projects.contains_key(task_id) {
                        continue;
                    }
                    let project: Option<String> = conn
                        .query_row("SELECT project FROM tasks WHERE id=?1", [task_id], |r| {
                            r.get(0)
                        })
                        .unwrap_or(None);
                    if let Some(p) = project.filter(|p| !p.trim().is_empty()) {
                        projects.insert(*task_id, p);
                    }
                }
                for (task_id, key) in
                    chronicle_core::anchor::anchor_tasks(&stored, &prior, &vcs, &re, &projects)
                {
                    // One open task per key: while another holds it the work
                    // belongs there, and a second anchor is what let a
                    // mailer ticket name a chronicle task (m27).
                    if let Some(owner) = storage::open_tasks_by_ref(conn, &key)?
                        .into_iter()
                        .find(|t| t.id != task_id)
                    {
                        tracing::warn!(
                            task_id,
                            owner = owner.id,
                            %key,
                            "ticket key already anchored to an open task; skipping"
                        );
                        continue;
                    }
                    // Newly anchored → fetch external context in the background
                    // (acceptance: context lands without touching a terminal).
                    if storage::set_task_external_ref(conn, task_id, &key)? {
                        storage::enqueue_ai_job(
                            conn,
                            Timestamp::now(),
                            "fetch_context",
                            0,
                            &storage::task_description_payload(task_id),
                        )?;
                    }
                }
            }
            Err(e) => tracing::warn!("bad ticket_regex, skipping anchoring: {e}"),
        }
        // One journal entry per task touched by this batch (m16); the job's
        // upsert keeps a re-derived batch idempotent.
        let touched: std::collections::BTreeSet<i64> =
            stored.iter().map(|(task_id, _, _)| *task_id).collect();
        for task_id in touched {
            if !storage::pending_journal_job(conn, task_id, batch_id)? {
                storage::enqueue_ai_job(
                    conn,
                    Timestamp::now(),
                    "journal",
                    0,
                    &storage::journal_payload(task_id, batch_id),
                )?;
            }
        }
        Ok(n)
    })();
    match result {
        Ok(n) => {
            tracing::info!(batch_id, tasks = n, "derivation done");
            Ok(n)
        }
        Err(e) => {
            tracing::error!(batch_id, "derivation failed: {e:#}");
            storage::fail_batch(conn, batch_id)?;
            Err(e)
        }
    }
}

/// An AFK span this long is a hard boundary: it splits time intervals and
/// blocks coalescing across it (M5 time honesty).
pub(crate) const AFK_SPLIT_MS: i64 = 5 * 60_000;
/// Unlabelled space two same-task pieces may join across.
pub(crate) const COALESCE_GAP_MIN: i64 = 2;

/// AFK gaps ≥ 5 min as `(start, end)` in whole minutes from `origin_ms`,
/// widened outward so a gap never looks shorter than it is.
pub(crate) fn afk_gaps_min(
    spans: &[chronicle_core::sessionizer::SpanDraft],
    origin_ms: i64,
) -> Vec<(i64, i64)> {
    use chronicle_core::sessionizer::SpanKind;
    spans
        .iter()
        .filter(|s| s.kind == SpanKind::Afk && s.duration_ms() >= AFK_SPLIT_MS)
        .map(|s| {
            let lo = (s.start.as_millisecond() - origin_ms).div_euclid(60_000);
            let hi = (s.end.as_millisecond() - origin_ms + 59_999).div_euclid(60_000);
            (lo, hi)
        })
        .collect()
}

/// Offsets are minutes from batch start, untrusted model output: clamp into
/// the batch window, drop empty/inverted intervals, and split any interval
/// the model stretched across a long AFK gap (small models ignore the prompt
/// rule). Pieces keep their task slot, so an AFK split no longer fragments
/// the task's identity.
pub(crate) fn clamp_intervals(
    drafts: Vec<chronicle_core::merge::LinkedInterval>,
    spans: &[chronicle_core::sessionizer::SpanDraft],
    start_ms: i64,
    end_ms: i64,
) -> Vec<chronicle_core::types::NewInterval> {
    use chronicle_core::sessionizer::SpanKind;
    use chronicle_core::types::{NewInterval, ms_to_ts};
    const MIN_PIECE_MS: i64 = 60_000;
    let gaps: Vec<(i64, i64)> = spans
        .iter()
        .filter(|s| s.kind == SpanKind::Afk && s.duration_ms() >= AFK_SPLIT_MS)
        .map(|s| (s.start.as_millisecond(), s.end.as_millisecond()))
        .collect();
    let mut out = Vec::new();
    for d in drafts {
        let s = (start_ms + d.start_offset_min * 60_000).clamp(start_ms, end_ms);
        let e = (start_ms + d.end_offset_min * 60_000).clamp(start_ms, end_ms);
        if e <= s {
            continue;
        }
        let mut pieces = Vec::new();
        let mut cur = s;
        for &(gap_start, gap_end) in &gaps {
            if gap_end <= cur || gap_start >= e {
                continue;
            }
            if gap_start > cur {
                pieces.push((cur, gap_start));
            }
            cur = gap_end.max(cur);
        }
        if cur < e {
            pieces.push((cur, e));
        }
        for (piece_start, piece_end) in pieces {
            if piece_end - piece_start < MIN_PIECE_MS {
                continue;
            }
            out.push(NewInterval {
                slot: d.slot,
                start_ts: ms_to_ts(piece_start),
                end_ts: ms_to_ts(piece_end),
                confidence: d.confidence.clamp(0.0, 1.0),
            });
        }
    }
    // Intervals must not overlap; when the model overlaps anyway, the earlier
    // start (higher confidence on ties) wins and the later one is trimmed.
    out.sort_by(|a, b| {
        a.start_ts
            .cmp(&b.start_ts)
            .then(b.confidence.total_cmp(&a.confidence))
    });
    let mut last_end = None;
    out.retain_mut(|t| {
        if let Some(le) = last_end
            && t.start_ts < le
        {
            if t.end_ts <= le {
                return false;
            }
            t.start_ts = le;
        }
        last_end = Some(t.end_ts);
        true
    });
    out
}
