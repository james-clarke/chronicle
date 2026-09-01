mod rotate;
mod ui;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use chronicle_core::config::Config;
use chronicle_core::types::CaptureEvent;
use clap::{Parser, Subcommand, ValueEnum};
use crossbeam_channel::Sender;
use jiff::{Timestamp, ToSpan, Zoned, civil, tz::TimeZone};
use regex::Regex;

#[derive(Parser)]
#[command(name = "chronicle", version, about = "Local-first activity tracker")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the daemon (default).
    Run,
    /// Internal: egui window process, spawned by the daemon.
    #[command(hide = true)]
    Ui,
    /// Internal: ephemeral derivation worker.
    #[command(hide = true)]
    Derive {
        #[arg(long)]
        batch: i64,
    },
    /// Internal: warm chat inference worker.
    #[command(hide = true)]
    ChatWorker {
        /// Conversation whose history seeds the model context.
        #[arg(long)]
        conversation: i64,
    },
    /// Internal: ephemeral AI-job worker (descriptions, suggestions, narratives).
    #[command(hide = true)]
    AiJob {
        #[arg(long)]
        id: i64,
    },
    /// Generate descriptions for closed tasks that lack one, newest first.
    BackfillDescriptions {
        /// Max tasks to describe this run; rerun to continue.
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Show or hide the UI of the running daemon.
    Toggle,
    /// Report daemon health and recent activity.
    Status {
        /// Machine-readable JSON instead of the human summary.
        #[arg(long)]
        json: bool,
    },
    /// Print stored data, optionally for one local civil day.
    Dump {
        #[arg(long, value_name = "YYYY-MM-DD")]
        day: Option<String>,
    },
    /// Print a timesheet report (CSV or markdown) to stdout.
    Report {
        /// One local civil day.
        #[arg(long, value_name = "YYYY-MM-DD", conflicts_with = "week")]
        day: Option<String>,
        /// The Mon–Sun week containing this date; default: current week.
        #[arg(long, value_name = "YYYY-MM-DD", conflicts_with = "day")]
        week: Option<String>,
        #[arg(long, value_enum, default_value_t = ReportFormat::Csv)]
        format: ReportFormat,
    },
    /// Manage local LLM models.
    Model {
        #[command(subcommand)]
        cmd: ModelCmd,
    },
    /// Run the allowlisted MCP context calls and print what derivation would inject.
    McpCheck,
    /// Internal: benchmark downloaded models on fixtures and/or real batches.
    #[command(hide = true)]
    Bench {
        /// Directory of fixture JSONL event streams.
        #[arg(long, default_value = "fixtures")]
        fixtures: PathBuf,
        /// Real batch ids from the live DB (repeatable).
        #[arg(long)]
        batch: Vec<i64>,
        /// Print each case's digest instead of running inference.
        #[arg(long)]
        digest: bool,
        /// Only run cases whose name contains this substring.
        #[arg(long)]
        only: Option<String>,
        /// Only run models whose preset name contains this substring.
        #[arg(long)]
        model: Option<String>,
    },
}

#[derive(Clone, Copy, ValueEnum)]
enum ReportFormat {
    Csv,
    Md,
}

#[derive(Subcommand)]
enum ModelCmd {
    /// Download a model preset (resumable, SHA-256 verified).
    Pull {
        /// Preset name; defaults to qwen3-4b.
        preset: Option<String>,
    },
    /// List presets and their download state.
    List,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let data_dir = chronicle_core::data_dir().context("could not resolve a data directory")?;
    match cli.cmd.unwrap_or(Cmd::Run) {
        Cmd::Run => run(&data_dir),
        Cmd::Dump { day } => dump(&data_dir, day.as_deref()),
        Cmd::Ui => ui::run(&data_dir),
        Cmd::Toggle => {
            if send_ctrl(&socket_path(&data_dir), "toggle") {
                Ok(())
            } else {
                bail!("chronicle daemon is not running")
            }
        }
        Cmd::Status { json } => status(&data_dir, json),
        Cmd::Report { day, week, format } => {
            report(&data_dir, day.as_deref(), week.as_deref(), format)
        }
        Cmd::Derive { batch } => derive_worker(&data_dir, batch),
        Cmd::McpCheck => mcp_check(&data_dir),
        Cmd::Model { cmd } => model_cmd(&data_dir, cmd),
        Cmd::Bench {
            fixtures,
            batch,
            digest,
            only,
            model,
        } => bench(
            &data_dir,
            &fixtures,
            &batch,
            digest,
            only.as_deref(),
            model.as_deref(),
        ),
        Cmd::ChatWorker { conversation } => chat_worker(&data_dir, conversation),
        Cmd::AiJob { id } => ai_job_worker(&data_dir, id),
        Cmd::BackfillDescriptions { limit } => backfill_descriptions(&data_dir, limit),
    }
}

/// One JSON object per line, both directions, over the chat worker's stdio.
pub(crate) mod chatproto {
    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(tag = "t", rename_all = "snake_case")]
    pub enum ClientMsg {
        Ask {
            ask: String,
        },
        /// Drop model history and re-seed from another conversation.
        Switch {
            conversation_id: i64,
        },
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    #[serde(tag = "t", rename_all = "snake_case")]
    pub enum WorkerMsg {
        /// Model loaded; worker accepts questions.
        Ready,
        Tok {
            text: String,
        },
        Done,
        Err {
            message: String,
        },
    }
}

/// Warm chat worker, spawned by the UI when the chat panel opens and killed
/// when it closes. The model stays resident between questions; retrieval is
/// local-DB only (time-ref range or FTS, in chronicle_core::chat).
fn chat_worker(data_dir: &Path, mut conversation_id: i64) -> anyhow::Result<()> {
    use std::io::{BufRead, Write};

    use chatproto::{ClientMsg, WorkerMsg};
    use chronicle_core::storage;

    let _guard = init_logging(data_dir)?;
    let mut stdout = std::io::stdout();
    let mut send = move |msg: &WorkerMsg| {
        let mut line = serde_json::to_string(msg).expect("worker msg serializes");
        line.push('\n');
        // A dead pipe means the panel is gone; exiting quietly is correct.
        if stdout
            .write_all(line.as_bytes())
            .and_then(|()| stdout.flush())
            .is_err()
        {
            std::process::exit(0);
        }
    };

    let config = Config::load(&data_dir.join("config.toml"))?;
    let conn = storage::open(&data_dir.join("chronicle.db"))?;
    let Some(model_path) = chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir)
    else {
        send(&WorkerMsg::Err {
            message: "no model available; run `chronicle model pull`".into(),
        });
        bail!("no model available")
    };
    let model = match chronicle_derive::ChatModel::load(&model_path) {
        Ok(m) => m,
        Err(e) => {
            send(&WorkerMsg::Err {
                message: format!("model load failed: {e:#}"),
            });
            return Err(e);
        }
    };

    // Conversation tail so follow-up questions keep working across reopens.
    let seed_history = |conversation_id: i64| -> Vec<(String, String)> {
        let mut history = Vec::new();
        if let Ok(messages) = storage::recent_chat_messages(&conn, conversation_id, 2 * 3) {
            let mut pending_user: Option<String> = None;
            for (role, content) in messages {
                match role.as_str() {
                    "user" => pending_user = Some(content),
                    _ => {
                        if let Some(q) = pending_user.take() {
                            history.push((q, content));
                        }
                    }
                }
            }
        }
        history
    };
    let mut history = seed_history(conversation_id);
    send(&WorkerMsg::Ready);
    tracing::info!("chat worker ready");

    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        let ask = match serde_json::from_str::<ClientMsg>(&line) {
            Ok(ClientMsg::Ask { ask }) => ask,
            Ok(ClientMsg::Switch {
                conversation_id: id,
            }) => {
                conversation_id = id;
                history = seed_history(id);
                continue;
            }
            Err(_) => continue,
        };
        let ask = ask.trim().to_owned();
        if ask.is_empty() {
            continue;
        }
        if let Err(e) =
            storage::insert_chat_message(&conn, Timestamp::now(), conversation_id, "user", &ask)
        {
            tracing::error!("chat message insert failed: {e}");
        }
        let context = match chronicle_core::chat::build_context(&conn, &ask, &Zoned::now()) {
            Ok(ctx) => ctx,
            Err(e) => {
                send(&WorkerMsg::Err {
                    message: format!("retrieval failed: {e}"),
                });
                continue;
            }
        };
        let t0 = Instant::now();
        match model.answer(&history, &context, &ask, &mut |piece| {
            send(&WorkerMsg::Tok { text: piece.into() });
        }) {
            Ok(answer) => {
                send(&WorkerMsg::Done);
                tracing::info!(secs = t0.elapsed().as_secs_f64(), "chat answer done");
                if let Err(e) = storage::insert_chat_message(
                    &conn,
                    Timestamp::now(),
                    conversation_id,
                    "assistant",
                    &answer,
                ) {
                    tracing::error!("chat message insert failed: {e}");
                }
                history.push((ask, answer));
            }
            Err(e) => {
                tracing::error!("chat inference failed: {e:#}");
                send(&WorkerMsg::Err {
                    message: format!("inference failed: {e:#}"),
                });
            }
        }
    }
    Ok(())
}

fn model_cmd(data_dir: &Path, cmd: ModelCmd) -> anyhow::Result<()> {
    use chronicle_derive::model;
    match cmd {
        ModelCmd::Pull { preset } => {
            let name = preset.as_deref().unwrap_or(model::default_preset().name);
            let Some(spec) = model::preset(name) else {
                bail!(
                    "unknown preset {name:?}; available: {}",
                    model::PRESETS
                        .iter()
                        .map(|p| p.name)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            };
            println!("pulling {} ({}/{})", spec.name, spec.repo, spec.file);
            let mut last_pct = u64::MAX;
            let path = model::pull(data_dir, spec, &mut |done, total| {
                let pct = done * 100 / total.max(1);
                if pct != last_pct {
                    last_pct = pct;
                    print!("\r{pct:3}% of {} MiB", total >> 20);
                    let _ = std::io::Write::flush(&mut std::io::stdout());
                }
            })?;
            println!("\rverified {} ", path.display());
            Ok(())
        }
        ModelCmd::List => {
            let config = Config::load(&data_dir.join("config.toml"))?;
            for spec in model::PRESETS {
                let path = model::models_dir(data_dir).join(spec.file);
                let state = if path.exists() {
                    "downloaded"
                } else {
                    "not downloaded"
                };
                println!("{:12} {state}  {}", spec.name, path.display());
            }
            match model::resolve(config.model_path.as_deref(), data_dir) {
                Some(p) => println!("active: {}", p.display()),
                None => println!("active: none (run `chronicle model pull`)"),
            }
            Ok(())
        }
    }
}

/// M4 benchmark gate: run every downloaded preset over fixture streams and
/// real batches, print tasks + timing side by side. Fixtures with a
/// `<name>.expect.json` are scored deterministically (post-merge output);
/// judgment on the rest stays human.
fn bench(
    data_dir: &Path,
    fixtures: &Path,
    batch_ids: &[i64],
    digest_only: bool,
    only: Option<&str>,
    model_filter: Option<&str>,
) -> anyhow::Result<()> {
    use chronicle_core::eval::Expectations;
    use chronicle_core::types::{Event, OpenTask};
    use chronicle_core::{digest, sessionizer, storage};

    let config = Config::load(&data_dir.join("config.toml"))?;
    let mut cases: Vec<(String, String, Vec<OpenTask>, Option<Expectations>)> = Vec::new();

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
            cases.push((
                format!("fixture:{name}"),
                digest::build_digest(&spans, &jiff::tz::TimeZone::UTC, &open, &[], None),
                open,
                expect,
            ));
        }
    }

    if !batch_ids.is_empty() {
        let conn = storage::open(&data_dir.join("chronicle.db"))?;
        let tz = TimeZone::system();
        for &id in batch_ids {
            let spans = storage::batch_spans(&conn, id)?;
            if spans.is_empty() {
                println!("batch {id}: no spans, skipping");
                continue;
            }
            let open = storage::open_tasks(&conn, 8)?;
            let corrections = storage::similar_corrections(&conn, &spans, 4)?;
            cases.push((
                format!("batch:{id}"),
                digest::build_digest(&spans, &tz, &open, &corrections, None),
                open,
                None,
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

    for (case, digest_text, open, expect) in &cases {
        println!(
            "\n=== {case} (digest ~{} tokens)",
            digest::approx_tokens(digest_text)
        );
        for (name, path) in &models {
            let t0 = Instant::now();
            match chronicle_derive::infer_intervals(path, digest_text) {
                Ok(raw) => {
                    let drafts = chronicle_core::merge::sanitize_intervals(raw, open.len());
                    let (slots, linked) = chronicle_core::merge::link_intervals(&drafts, open);
                    let resolved = chronicle_core::eval::resolve(&slots, &linked, open);
                    println!(
                        "--- {name}: {} intervals over {} tasks in {:.1}s (linked)",
                        resolved.len(),
                        slots.len(),
                        t0.elapsed().as_secs_f64()
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
fn derive_worker(data_dir: &Path, batch_id: i64) -> anyhow::Result<()> {
    use chronicle_core::storage;
    let _guard = init_logging(data_dir)?;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let mut conn = storage::open(&data_dir.join("chronicle.db"))?;
    let Some(model_path) = chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir)
    else {
        bail!("no model available; run `chronicle model pull`")
    };
    let Some(batch) = storage::claim_batch(&conn, batch_id)? else {
        bail!("batch {batch_id} is not eligible for derivation")
    };
    let result = (|| -> anyhow::Result<usize> {
        let spans = storage::batch_spans(&conn, batch_id)?;
        let open = storage::open_tasks(&conn, 8)?;
        let corrections = storage::similar_corrections(&conn, &spans, 4)?;
        let tz = TimeZone::system();
        let mcp_path = config
            .mcp_config
            .clone()
            .unwrap_or_else(|| data_dir.join("mcp.toml"));
        let mcp_context = chronicle_mcp::gather_context(&mcp_path);
        let digest = chronicle_core::digest::build_digest(
            &spans,
            &tz,
            &open,
            &corrections,
            mcp_context.as_deref(),
        );
        let raw = chronicle_derive::infer_intervals(&model_path, &digest)?;
        let drafts = chronicle_core::merge::sanitize_intervals(raw, open.len());
        let (slots, linked) = chronicle_core::merge::link_intervals(&drafts, &open);
        let intervals = clamp_intervals(linked, &spans, batch.start_ts, batch.end_ts);
        let n = intervals.len();
        storage::store_derivation(&mut conn, batch_id, &slots, &intervals)?;
        Ok(n)
    })();
    match result {
        Ok(n) => {
            tracing::info!(batch_id, tasks = n, "derivation done");
            Ok(())
        }
        Err(e) => {
            tracing::error!(batch_id, "derivation failed: {e:#}");
            storage::fail_batch(&conn, batch_id)?;
            Err(e)
        }
    }
}

/// Ephemeral AI-job worker: claim job → kind-specific inference → store
/// result → exit. Any failure marks the job failed (retry-once via attempts
/// cap), mirroring the derive worker.
fn ai_job_worker(data_dir: &Path, job_id: i64) -> anyhow::Result<()> {
    use chronicle_core::storage;
    let _guard = init_logging(data_dir)?;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let conn = storage::open(&data_dir.join("chronicle.db"))?;
    let Some(model_path) = chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir)
    else {
        bail!("no model available; run `chronicle model pull`")
    };
    let Some(job) = storage::claim_ai_job(&conn, job_id)? else {
        bail!("ai job {job_id} is not eligible")
    };
    match run_ai_job(&conn, &model_path, &job) {
        Ok(result) => {
            storage::complete_ai_job(&conn, job_id, &result)?;
            tracing::info!(job_id, kind = %job.kind, "ai job done");
            Ok(())
        }
        Err(e) => {
            tracing::error!(job_id, kind = %job.kind, "ai job failed: {e:#}");
            storage::fail_ai_job(&conn, job_id, &format!("{e:#}"))?;
            Err(e)
        }
    }
}

fn run_ai_job(
    conn: &rusqlite::Connection,
    model_path: &Path,
    job: &chronicle_core::storage::AiJobRow,
) -> anyhow::Result<String> {
    use chronicle_core::{insights, report, storage};
    let payload: serde_json::Value = serde_json::from_str(&job.payload)?;
    let describer = chronicle_derive::describe::Describer::load(model_path)?;
    match job.kind.as_str() {
        "task_description" => {
            let task_id = payload["task_id"]
                .as_i64()
                .context("payload lacks task_id")?;
            let (label, project): (String, Option<String>) = conn.query_row(
                "SELECT label, project FROM tasks WHERE id=?1",
                [task_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let evidence = storage::task_evidence_text(conn, task_id)?;
            if evidence.trim().is_empty() {
                bail!("no span evidence for task {task_id}");
            }
            let desc = describer.describe_task(&label, project.as_deref(), &evidence)?;
            storage::set_task_description(conn, task_id, Some(&desc))?;
            Ok(desc)
        }
        "suggest_task" => {
            let lookback_min = payload["lookback_min"].as_i64().unwrap_or(15);
            let hi = Timestamp::now().as_millisecond();
            let lo = hi - lookback_min * 60_000;
            let spans = storage::spans_in_range(conn, lo, hi)?;
            if spans.iter().all(|s| s.kind != chronicle_core::sessionizer::SpanKind::Focus) {
                bail!("no recent focus activity to suggest from");
            }
            let tz = TimeZone::system();
            let digest = chronicle_core::digest::build_digest(&spans, &tz, &[], &[], None);
            let s = describer.suggest_task(&digest)?;
            Ok(serde_json::to_string(&s)?)
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
        other => bail!("unknown ai job kind {other}"),
    }
}

/// Local civil dates covering `[lo, hi)` in `tz`.
fn civil_days(lo: i64, hi: i64, tz: &TimeZone) -> anyhow::Result<Vec<jiff::civil::Date>> {
    let mut days = Vec::new();
    let mut d = chronicle_core::types::ms_to_ts(lo).to_zoned(tz.clone()).date();
    let last = chronicle_core::types::ms_to_ts((hi - 1).max(lo))
        .to_zoned(tz.clone())
        .date();
    while d <= last {
        days.push(d);
        d = d.checked_add(jiff::Span::new().days(1))?;
    }
    Ok(days)
}

/// One-shot in-process backfill (loads the model once, like `bench`); an
/// occasional operator command, not routed through the daemon queue where 50
/// jobs would starve derivation. Stop the daemon's unit first if it might
/// derive concurrently — two llama processes fight for the same cores.
fn backfill_descriptions(data_dir: &Path, limit: usize) -> anyhow::Result<()> {
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

/// Offsets are minutes from batch start, untrusted model output: clamp into
/// the batch window, drop empty/inverted intervals, and split any interval
/// the model stretched across a long AFK gap (small models ignore the prompt
/// rule). Pieces keep their task slot, so an AFK split no longer fragments
/// the task's identity.
fn clamp_intervals(
    drafts: Vec<chronicle_core::merge::LinkedInterval>,
    spans: &[chronicle_core::sessionizer::SpanDraft],
    start_ms: i64,
    end_ms: i64,
) -> Vec<chronicle_core::types::NewInterval> {
    use chronicle_core::sessionizer::SpanKind;
    use chronicle_core::types::{NewInterval, ms_to_ts};
    const AFK_SPLIT_MS: i64 = 5 * 60_000;
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

pub(crate) fn socket_path(data_dir: &Path) -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| data_dir.to_path_buf())
        .join("chronicle.sock")
}

pub(crate) fn send_ctrl(sock: &Path, msg: &str) -> bool {
    use std::io::Write;
    match std::os::unix::net::UnixStream::connect(sock) {
        Ok(mut stream) => stream.write_all(format!("{msg}\n").as_bytes()).is_ok(),
        Err(_) => false,
    }
}

enum CtrlMsg {
    Toggle,
    DeriveNow,
    /// One-shot reply channel; the listener writes the JSON back to the client.
    Status(Sender<String>),
    /// Only in-process senders (signal thread, tray "Quit") — not part of the
    /// socket protocol, so a stray client can't stop the daemon.
    Shutdown,
}

#[derive(Debug, PartialEq)]
enum CtrlCmd {
    Toggle,
    DeriveNow,
    Status,
}

fn parse_ctrl_cmd(line: &str) -> Option<CtrlCmd> {
    match line.trim() {
        "toggle" => Some(CtrlCmd::Toggle),
        "derive" => Some(CtrlCmd::DeriveNow),
        "status" => Some(CtrlCmd::Status),
        _ => None,
    }
}

fn spawn_ctrl_listener(
    listener: std::os::unix::net::UnixListener,
    tx: Sender<CtrlMsg>,
) -> anyhow::Result<()> {
    use std::io::{BufRead, Write};
    std::thread::Builder::new()
        .name("ctrl".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut line = String::new();
                if std::io::BufReader::new(&stream)
                    .read_line(&mut line)
                    .is_err()
                {
                    continue;
                }
                match parse_ctrl_cmd(&line) {
                    Some(CtrlCmd::Toggle) => {
                        if tx.send(CtrlMsg::Toggle).is_err() {
                            return;
                        }
                    }
                    Some(CtrlCmd::DeriveNow) => {
                        if tx.send(CtrlMsg::DeriveNow).is_err() {
                            return;
                        }
                    }
                    Some(CtrlCmd::Status) => {
                        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
                        if tx.send(CtrlMsg::Status(reply_tx)).is_err() {
                            return;
                        }
                        // Bounded wait: a wedged main loop must not hang the
                        // listener; no reply reads as "unresponsive" client-side.
                        if let Ok(json) = reply_rx.recv_timeout(Duration::from_secs(1)) {
                            let _ = writeln!(stream, "{json}");
                        }
                    }
                    None => continue,
                }
            }
        })?;
    Ok(())
}

fn spawn_signal_handler(ctrl_tx: Sender<CtrlMsg>) -> anyhow::Result<()> {
    use signal_hook::consts::{SIGINT, SIGTERM};
    let mut signals = signal_hook::iterator::Signals::new([SIGTERM, SIGINT])?;
    std::thread::Builder::new()
        .name("signal".into())
        .spawn(move || {
            let mut requested = false;
            for _ in signals.forever() {
                if requested {
                    tracing::warn!("second shutdown signal; forcing exit");
                    std::process::exit(1);
                }
                requested = true;
                tracing::info!("shutdown signal received");
                if ctrl_tx.send(CtrlMsg::Shutdown).is_err() {
                    std::process::exit(1); // main loop already gone
                }
            }
        })?;
    Ok(())
}

/// StatusNotifierItem tray icon: left-click / "Show/Hide" toggles the UI
/// child, "Quit" shuts the daemon down (same paths as socket + signals).
struct ChronicleTray {
    ctrl_tx: Sender<CtrlMsg>,
}

impl ksni::Tray for ChronicleTray {
    fn id(&self) -> String {
        "chronicle".into()
    }

    fn title(&self) -> String {
        "Chronicle".into()
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        let _ = self.ctrl_tx.send(CtrlMsg::Toggle);
    }

    fn icon_pixmap(&self) -> Vec<ksni::Icon> {
        vec![tray_icon()]
    }

    fn menu(&self) -> Vec<ksni::MenuItem<Self>> {
        use ksni::menu::StandardItem;
        vec![
            StandardItem {
                label: "Show/Hide".into(),
                enabled: true,
                visible: true,
                activate: Box::new(|t: &mut Self| {
                    let _ = t.ctrl_tx.send(CtrlMsg::Toggle);
                }),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Quit".into(),
                enabled: true,
                visible: true,
                activate: Box::new(|t: &mut Self| {
                    let _ = t.ctrl_tx.send(CtrlMsg::Shutdown);
                }),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// Procedural icon (filled circle, UI accent blue) — no image asset/dep.
fn tray_icon() -> ksni::Icon {
    const SIZE: i32 = 22;
    let (r, g, b) = (0x5e_u8, 0x87_u8, 0xea_u8);
    let c = (SIZE - 1) as f32 / 2.0;
    let radius = c - 1.0;
    let mut data = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let d = ((x as f32 - c).powi(2) + (y as f32 - c).powi(2)).sqrt();
            // 1px soft edge; ARGB32 in network byte order.
            let a = ((radius + 0.5 - d).clamp(0.0, 1.0) * 255.0) as u8;
            data.extend_from_slice(&[a, r, g, b]);
        }
    }
    ksni::Icon {
        width: SIZE,
        height: SIZE,
        data,
    }
}

/// Host the tray's D-Bus service on a background thread. No tray host running
/// is non-fatal: log and continue, like the AW endpoint port conflict.
fn spawn_tray(ctrl_tx: Sender<CtrlMsg>) -> anyhow::Result<()> {
    std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread()
                .enable_io()
                .enable_time()
                .build()
            {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!("tray runtime failed: {e}");
                    return;
                }
            };
            rt.block_on(async move {
                use ksni::TrayMethods;
                match (ChronicleTray { ctrl_tx }).spawn().await {
                    // Keep the reactor (and the D-Bus service) alive for the
                    // daemon's lifetime; dropping the handle removes the icon.
                    Ok(_handle) => std::future::pending::<()>().await,
                    Err(e) => tracing::warn!("tray unavailable: {e}"),
                }
            });
        })?;
    Ok(())
}

fn toggle_ui(slot: &mut Option<Child>) {
    use std::io::Write;
    if let Some(child) = slot {
        let alive = matches!(child.try_wait(), Ok(None));
        if alive
            && let Some(stdin) = child.stdin.as_mut()
            && stdin
                .write_all(b"toggle\n")
                .and_then(|()| stdin.flush())
                .is_ok()
        {
            return;
        }
        *slot = None; // exited or pipe broken — respawn
    }
    match spawn_ui_child() {
        Ok(child) => *slot = Some(child),
        Err(e) => tracing::error!("failed to spawn ui child: {e}"),
    }
}

/// Own binary path for spawning children. When the binary is replaced while
/// the daemon runs (dev rebuild, upgrade), /proc/self/exe reads
/// "<path> (deleted)"; fall back to the current binary at the same path —
/// mild version skew beats a spawn failure.
pub(crate) fn own_exe() -> std::io::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    if !exe.exists()
        && let Some(stripped) = exe.to_str().and_then(|s| s.strip_suffix(" (deleted)"))
    {
        let replaced = PathBuf::from(stripped);
        if replaced.exists() {
            return Ok(replaced);
        }
    }
    Ok(exe)
}

fn spawn_ui_child() -> std::io::Result<Child> {
    Command::new(own_exe()?)
        .arg("ui")
        .stdin(Stdio::piped())
        .spawn()
}

/// llama/ggml noise goes to the log file, not the UI's terminal.
pub(crate) fn spawn_chat_worker(conversation_id: i64) -> std::io::Result<Child> {
    Command::new(own_exe()?)
        .args([
            "chat-worker",
            "--conversation",
            &conversation_id.to_string(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
}

fn reap_ui(slot: &mut Option<Child>) {
    if let Some(child) = slot
        && matches!(child.try_wait(), Ok(Some(_)))
    {
        *slot = None;
    }
}

fn run(data_dir: &Path) -> anyhow::Result<()> {
    // Single instance: a second `chronicle` just toggles the running daemon.
    let sock = socket_path(data_dir);
    if send_ctrl(&sock, "toggle") {
        println!("chronicle daemon already running \u{2014} toggled UI");
        return Ok(());
    }
    let _ = std::fs::remove_file(&sock); // stale socket from an unclean exit
    let listener = std::os::unix::net::UnixListener::bind(&sock)
        .with_context(|| format!("failed to bind {}", sock.display()))?;

    let _guard = init_logging(data_dir)?;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let filters = Filters::new(&config)?;
    let mut conn = chronicle_core::storage::open(&data_dir.join("chronicle.db"))?;
    // Daemon downtime must not read as focus time: mark a gap as AFK-from-the-
    // last-event; the AFK poller's initial state announcement closes it.
    if let Some(last_ms) = chronicle_core::storage::latest_event_ts(&conn)?
        && Timestamp::now().as_millisecond() - last_ms > GAP_MARKER_SECS * 1000
    {
        let ts = chronicle_core::types::ms_to_ts(last_ms + 1);
        chronicle_core::storage::insert_event(&conn, &CaptureEvent::Afk { idle: true, ts })?;
    }
    // A `running` batch with no live worker (unclean daemon exit) burns its
    // attempt and falls back to `failed` so the retry cap still holds.
    chronicle_core::storage::reset_stale_running(&conn)?;
    let started = Instant::now();
    let (tx, rx) = crossbeam_channel::unbounded();
    let (ctrl_tx, ctrl_rx) = crossbeam_channel::unbounded();
    spawn_signal_handler(ctrl_tx.clone())?;
    spawn_tray(ctrl_tx.clone())?;
    spawn_ctrl_listener(listener, ctrl_tx)?;
    spawn_capture(&config, tx.clone())?;
    // Port taken (a real aw-server?) must not kill capture: log, warn in UI.
    let server_error = match chronicle_server::spawn(&config, tx) {
        Ok(()) => None,
        Err(e) => {
            tracing::error!("AW endpoint failed on 127.0.0.1:{}: {e}", config.port);
            Some(format!("AW endpoint failed on port {}: {e}", config.port))
        }
    };
    chronicle_core::storage::set_meta(&conn, "server_error", server_error.as_deref())?;
    tracing::info!(?data_dir, "chronicle daemon running");
    let mut ui_child: Option<Child> = None;
    // First run without a model: derivation can't start, so surface the UI
    // (and its onboarding card) instead of sitting silent in the background.
    if chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir).is_none() {
        toggle_ui(&mut ui_child);
    }
    let mut scheduler = Scheduler { worker: None };
    let mut idle_since: Option<i64> = None;
    let mut next_refresh = Instant::now() + SESSIONIZE_EVERY;
    let exit_reason = 'daemon: loop {
        let timeout = next_refresh.saturating_duration_since(Instant::now());
        crossbeam_channel::select! {
            recv(rx) -> event => {
                let Ok(event) = event else { break 'daemon ExitReason::CaptureDied };
                if filters.excluded(&event) {
                    continue;
                }
                if let CaptureEvent::Afk { idle, ts } = &event {
                    if *idle {
                        idle_since.get_or_insert(ts.as_millisecond());
                    } else {
                        idle_since = None;
                    }
                }
                if let Err(e) = chronicle_core::storage::insert_event(&conn, &event) {
                    tracing::error!("event insert failed: {e}");
                }
            }
            recv(ctrl_rx) -> cmd => {
                match cmd {
                    Ok(CtrlMsg::Toggle) => {
                        // Fresh spans the moment the window appears, not up to
                        // a tick later.
                        if let Err(e) = chronicle_core::sessionizer::refresh(&mut conn, &config, Timestamp::now()) {
                            tracing::error!("sessionize refresh failed: {e}");
                        }
                        toggle_ui(&mut ui_child);
                    }
                    Ok(CtrlMsg::DeriveNow) => scheduler.tick(&conn, &config, data_dir, idle_since, true),
                    Ok(CtrlMsg::Status(reply)) => {
                        let _ = reply.send(status_json(&scheduler, idle_since, &mut ui_child, started));
                    }
                    Ok(CtrlMsg::Shutdown) => break 'daemon ExitReason::Signal,
                    Err(_) => {}
                }
            }
            default(timeout) => {
                let now = Timestamp::now();
                if let Err(e) = chronicle_core::sessionizer::refresh(&mut conn, &config, now) {
                    tracing::error!("sessionize refresh failed: {e}");
                }
                scheduler.tick(&conn, &config, data_dir, idle_since, false);
                reap_ui(&mut ui_child);
                next_refresh = Instant::now() + SESSIONIZE_EVERY;
            }
        }
    };
    tracing::info!("shutting down");
    // Child drop leaks the OS process — kills must be explicit. SIGKILL on the
    // derive worker is safe: `store_derivation` commits in one transaction, and
    // `fail_batch` records the burned attempt immediately.
    if let Some((mut child, _, kind)) = scheduler.worker.take() {
        let _ = child.kill();
        let _ = child.wait();
        match kind {
            WorkerKind::Batch(id) => {
                let _ = chronicle_core::storage::fail_batch(&conn, id);
            }
            WorkerKind::AiJob(id) => {
                let _ = chronicle_core::storage::fail_ai_job(&conn, id, "daemon shutdown");
            }
        }
    }
    if let Some(mut child) = ui_child.take() {
        let _ = child.kill();
        let _ = child.wait();
    }
    match exit_reason {
        ExitReason::Signal => Ok(()),
        ExitReason::CaptureDied => bail!("capture threads exited"),
    }
}

enum ExitReason {
    Signal,
    CaptureDied,
}

const DERIVE_TIMEOUT: Duration = Duration::from_secs(300);
/// "Low 1-min load" gate for deriving while the user is active.
const LOW_LOAD: f64 = 1.0;
const BATTERY_DEFER_PCT: u32 = 30;

/// What the single worker slot is running.
enum WorkerKind {
    Batch(i64),
    AiJob(i64),
}

struct Scheduler {
    /// At most one inference worker at a time (derive or ai-job): the whole
    /// design assumes a single resident llama.cpp process.
    worker: Option<(Child, Instant, WorkerKind)>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct DaemonStatus {
    uptime_secs: u64,
    derive_active: bool,
    idle_secs: Option<u64>,
    ui_open: bool,
}

fn status_json(
    scheduler: &Scheduler,
    idle_since: Option<i64>,
    ui_child: &mut Option<Child>,
    started: Instant,
) -> String {
    let status = DaemonStatus {
        uptime_secs: started.elapsed().as_secs(),
        derive_active: scheduler.worker.is_some(),
        idle_secs: idle_since
            .map(|since| ((Timestamp::now().as_millisecond() - since) / 1000).max(0) as u64),
        ui_open: ui_child
            .as_mut()
            .is_some_and(|c| matches!(c.try_wait(), Ok(None))),
    };
    serde_json::to_string(&status).unwrap_or_default()
}

impl Scheduler {
    fn tick(
        &mut self,
        conn: &rusqlite::Connection,
        config: &Config,
        data_dir: &Path,
        idle_since: Option<i64>,
        force: bool,
    ) {
        use chronicle_core::storage;
        if let Some((child, started, kind)) = &mut self.worker {
            // A worker that died before reporting leaves its row `running`;
            // count that as the failed attempt it was.
            let mark_dead = |conn: &rusqlite::Connection, kind: &WorkerKind| match *kind {
                WorkerKind::Batch(id) => {
                    let stuck = matches!(
                        storage::batch_status(conn, id),
                        Ok(Some(ref s)) if s == "running"
                    );
                    if stuck {
                        let _ = storage::fail_batch(conn, id);
                    }
                }
                WorkerKind::AiJob(id) => {
                    let stuck = matches!(
                        storage::ai_job_status(conn, id),
                        Ok(Some((ref s, _))) if s == "running"
                    );
                    if stuck {
                        let _ = storage::fail_ai_job(conn, id, "worker died");
                    }
                }
            };
            let id = match kind {
                WorkerKind::Batch(id) | WorkerKind::AiJob(id) => *id,
            };
            match child.try_wait() {
                Ok(Some(status)) => {
                    mark_dead(conn, kind);
                    if !status.success() {
                        tracing::warn!(id, %status, "inference worker failed");
                    }
                    self.worker = None;
                }
                Ok(None) => {
                    if started.elapsed() >= DERIVE_TIMEOUT {
                        tracing::warn!(id, "inference worker timed out; killing");
                        let _ = child.kill();
                        let _ = child.wait();
                        mark_dead(conn, kind);
                        self.worker = None;
                    }
                    return; // one worker at a time
                }
                Err(e) => {
                    tracing::error!(id, "inference worker wait failed: {e}");
                    self.worker = None;
                }
            }
        }
        // An interactive AI job (a user actively waiting on a suggestion)
        // jumps the idle gate; background jobs and derivation respect it.
        let interactive = storage::next_eligible_ai_job(conn, storage::AI_JOB_INTERACTIVE)
            .ok()
            .flatten();
        if interactive.is_none() && !force && !derive_gates_open(config, idle_since) {
            return;
        }
        if on_low_battery() {
            tracing::debug!("inference deferred: battery low");
            return;
        }
        prune_if_due(conn, config);
        if chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir).is_none() {
            tracing::debug!("inference skipped: no model downloaded");
            return;
        }
        if let Some(job_id) = interactive {
            self.spawn(spawn_ai_job_worker(job_id), WorkerKind::AiJob(job_id));
            return;
        }
        // Derivation stays ahead of background summarization.
        match storage::next_eligible_batch(conn) {
            Ok(Some(batch_id)) => {
                self.spawn(spawn_derive_worker(batch_id), WorkerKind::Batch(batch_id));
                return;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!("eligible-batch query failed: {e}");
                return;
            }
        }
        if let Ok(Some(job_id)) = storage::next_eligible_ai_job(conn, i64::MIN) {
            self.spawn(spawn_ai_job_worker(job_id), WorkerKind::AiJob(job_id));
        }
    }

    fn spawn(&mut self, child: std::io::Result<Child>, kind: WorkerKind) {
        let (id, what) = match kind {
            WorkerKind::Batch(id) => (id, "derive"),
            WorkerKind::AiJob(id) => (id, "ai-job"),
        };
        match child {
            Ok(child) => {
                tracing::info!(id, "{what} worker spawned");
                self.worker = Some((child, Instant::now(), kind));
            }
            Err(e) => tracing::error!(id, "failed to spawn {what} worker: {e}"),
        }
    }
}

const PRUNE_EVERY_MS: i64 = 24 * 3_600_000;
const PRUNE_BATCH: usize = 1000;

/// Daily retention prune + stale-task autoclose, piggybacking on the idle
/// gate the caller already checked. The stamp is written even when nothing
/// was deleted (or the prune failed) so a busy DB isn't retried every tick.
fn prune_if_due(conn: &rusqlite::Connection, config: &Config) {
    use chronicle_core::storage;
    if config.retention_days == 0 && config.task_autoclose_days == 0 {
        return; // 0 = keep forever / never autoclose
    }
    let now = Timestamp::now().as_millisecond();
    let last = storage::get_meta(conn, "last_prune_ts")
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    if now - last < PRUNE_EVERY_MS {
        return;
    }
    if config.retention_days != 0 {
        let cutoff = now - i64::from(config.retention_days) * 86_400_000;
        match storage::prune(conn, cutoff, PRUNE_BATCH) {
            Ok(0) => {}
            Ok(rows) => tracing::info!(rows, "retention prune"),
            Err(e) => tracing::error!("retention prune failed: {e}"),
        }
    }
    if config.task_autoclose_days != 0 {
        match storage::autoclose_stale_tasks(conn, Timestamp::now(), config.task_autoclose_days) {
            Ok(0) => {}
            Ok(rows) => tracing::info!(rows, "stale derived tasks closed"),
            Err(e) => tracing::error!("task autoclose failed: {e}"),
        }
    }
    let _ = storage::set_meta(conn, "last_prune_ts", Some(&now.to_string()));
}

fn spawn_derive_worker(batch_id: i64) -> std::io::Result<Child> {
    // Worker logging goes to the log file; keep the daemon terminal clean.
    Command::new(own_exe()?)
        .args(["derive", "--batch", &batch_id.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

fn spawn_ai_job_worker(job_id: i64) -> std::io::Result<Child> {
    Command::new(own_exe()?)
        .args(["ai-job", "--id", &job_id.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

/// Derive when AFK ≥ `derive_idle_secs`, or the 1-min load is low enough
/// that inference won't be noticed.
fn derive_gates_open(config: &Config, idle_since: Option<i64>) -> bool {
    let idle_long_enough = idle_since.is_some_and(|since| {
        Timestamp::now().as_millisecond() - since >= i64::from(config.derive_idle_secs) * 1000
    });
    idle_long_enough || load_1min().is_some_and(|l| l < LOW_LOAD)
}

fn load_1min() -> Option<f64> {
    let s = std::fs::read_to_string("/proc/loadavg").ok()?;
    s.split_whitespace().next()?.parse().ok()
}

/// Defer derivation below 30% on battery power. No battery = never defers.
fn on_low_battery() -> bool {
    let Ok(entries) = std::fs::read_dir("/sys/class/power_supply") else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_battery =
            std::fs::read_to_string(path.join("type")).is_ok_and(|t| t.trim() == "Battery");
        if !is_battery {
            continue;
        }
        let discharging =
            std::fs::read_to_string(path.join("status")).is_ok_and(|s| s.trim() == "Discharging");
        let low = std::fs::read_to_string(path.join("capacity"))
            .ok()
            .and_then(|c| c.trim().parse::<u32>().ok())
            .is_some_and(|c| c < BATTERY_DEFER_PCT);
        if discharging && low {
            return true;
        }
    }
    false
}

// 15 s tick + 10 s UI wake bounds timeline staleness at ~25 s worst case
// (was 60+30 ≈ 90 s); the tail rewrite is a handful of rows, cost negligible.
const SESSIONIZE_EVERY: Duration = Duration::from_secs(15);

#[cfg(target_os = "linux")]
fn spawn_capture(config: &Config, tx: Sender<CaptureEvent>) -> anyhow::Result<()> {
    use chronicle_capture::FocusProvider;
    use chronicle_capture::x11::{X11AfkProvider, X11FocusProvider};

    let focus = X11FocusProvider::new().map_err(|e| anyhow::anyhow!("X11 focus provider: {e}"))?;
    let ftx = tx.clone();
    std::thread::Builder::new()
        .name("focus".into())
        .spawn(move || {
            if let Err(e) = focus.run(ftx) {
                tracing::error!("focus provider exited: {e}");
            }
        })?;

    let afk = X11AfkProvider::new().map_err(|e| anyhow::anyhow!("X11 afk provider: {e}"))?;
    let threshold_ms = u64::from(config.afk_close_secs) * 1000;
    std::thread::Builder::new()
        .name("afk".into())
        .spawn(move || afk_loop(afk, tx, threshold_ms))?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn spawn_capture(_config: &Config, _tx: Sender<CaptureEvent>) -> anyhow::Result<()> {
    bail!("capture on this platform lands in M9/M10")
}

const AFK_POLL: Duration = Duration::from_secs(30);
const GAP_MARKER_SECS: i64 = 60;

fn afk_loop(afk: impl chronicle_capture::AfkProvider, tx: Sender<CaptureEvent>, threshold_ms: u64) {
    // Announce the starting state so a dangling AFK span (gap marker, or a
    // restart while idle) gets closed.
    let mut was_idle = match afk.idle_ms() {
        Ok(ms) => {
            let idle = ms >= threshold_ms;
            if tx.send(afk_event(idle, ms)).is_err() {
                return;
            }
            idle
        }
        Err(e) => {
            tracing::warn!("afk poll failed: {e}");
            false
        }
    };
    loop {
        std::thread::sleep(AFK_POLL);
        let ms = match afk.idle_ms() {
            Ok(ms) => ms,
            Err(e) => {
                tracing::warn!("afk poll failed: {e}");
                continue;
            }
        };
        let idle = ms >= threshold_ms;
        if idle == was_idle {
            continue;
        }
        was_idle = idle;
        if tx.send(afk_event(idle, ms)).is_err() {
            return;
        }
    }
}

/// Idle transitions are backdated to when input actually stopped.
fn afk_event(idle: bool, idle_ms: u64) -> CaptureEvent {
    let now = Timestamp::now();
    let ts = if idle {
        now.checked_sub((idle_ms as i64).milliseconds())
            .unwrap_or(now)
    } else {
        now
    };
    CaptureEvent::Afk { idle, ts }
}

struct Filters {
    apps: Vec<Regex>,
    titles: Vec<Regex>,
}

impl Filters {
    fn new(config: &Config) -> anyhow::Result<Self> {
        let compile = |patterns: &[String]| -> anyhow::Result<Vec<Regex>> {
            patterns
                .iter()
                .map(|p| Regex::new(p).with_context(|| format!("bad exclusion regex {p:?}")))
                .collect()
        };
        Ok(Self {
            apps: compile(&config.excluded_apps)?,
            titles: compile(&config.excluded_titles)?,
        })
    }

    /// Excluded events are dropped before storage — never written at all.
    fn excluded(&self, event: &CaptureEvent) -> bool {
        let (app, title, url) = match event {
            CaptureEvent::Focus(e) | CaptureEvent::TitleChanged(e) => (&e.app, &e.title, None),
            CaptureEvent::Url(e) => (&e.app, &e.title, Some(&e.url)),
            CaptureEvent::Afk { .. } => return false,
        };
        self.apps.iter().any(|r| r.is_match(app))
            || self
                .titles
                .iter()
                .any(|r| r.is_match(title) || url.is_some_and(|u| r.is_match(u)))
    }
}

fn init_logging(data_dir: &Path) -> anyhow::Result<tracing_appender::non_blocking::WorkerGuard> {
    use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt};
    let dir = data_dir.join("logs");
    let file = rotate::SizeRotatingWriter::open(&dir, "chronicle.log")
        .with_context(|| format!("failed to open log file in {}", dir.display()))?;
    let (file_writer, guard) = tracing_appender::non_blocking(file);
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .with(fmt::layer().with_writer(std::io::stderr))
        .with(fmt::layer().with_ansi(false).with_writer(file_writer))
        .init();
    Ok(guard)
}

fn mcp_check(data_dir: &Path) -> anyhow::Result<()> {
    let _guard = init_logging(data_dir)?;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let path = config
        .mcp_config
        .clone()
        .unwrap_or_else(|| data_dir.join("mcp.toml"));
    println!("mcp config: {}", path.display());
    match chronicle_mcp::gather_context(&path) {
        Some(ctx) => println!("\n## Workspace context\n{ctx}"),
        None => println!(
            "no context gathered (missing/empty config, or every call failed — see warnings above)"
        ),
    }
    Ok(())
}

enum Liveness {
    Stopped,
    Unresponsive,
    Running(DaemonStatus),
}

fn query_daemon(sock: &Path) -> Liveness {
    use std::io::{BufRead, Write};
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(sock) else {
        return Liveness::Stopped;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    if stream.write_all(b"status\n").is_err() {
        return Liveness::Unresponsive;
    }
    let mut line = String::new();
    if std::io::BufReader::new(&stream)
        .read_line(&mut line)
        .is_err()
        || line.trim().is_empty()
    {
        return Liveness::Unresponsive;
    }
    match serde_json::from_str(line.trim()) {
        Ok(status) => Liveness::Running(status),
        Err(_) => Liveness::Unresponsive,
    }
}

fn fmt_secs(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h {}m", secs / 3600, (secs % 3600) / 60),
        _ => format!("{}d {}h", secs / 86_400, (secs % 86_400) / 3600),
    }
}

fn format_status(
    liveness: &Liveness,
    last_event_age_secs: Option<u64>,
    last_batch_end_ms: Option<i64>,
    model_present: bool,
    server_error: Option<&str>,
    last_prune_age_secs: Option<u64>,
) -> String {
    let mut out = String::new();
    match liveness {
        Liveness::Running(s) => {
            out.push_str(&format!(
                "chronicle: healthy — daemon running (uptime {})\n",
                fmt_secs(s.uptime_secs)
            ));
            out.push_str(&format!(
                "  derive worker: {}\n",
                if s.derive_active { "running" } else { "idle" }
            ));
            out.push_str(&format!(
                "  ui: {}\n",
                if s.ui_open { "open" } else { "closed" }
            ));
            if let Some(idle) = s.idle_secs {
                out.push_str(&format!("  user idle: {}\n", fmt_secs(idle)));
            }
        }
        Liveness::Stopped => out.push_str("chronicle: stopped — daemon not running\n"),
        Liveness::Unresponsive => {
            out.push_str("chronicle: running but not responding — socket up, no status reply\n");
        }
    }
    match last_event_age_secs {
        Some(age) => out.push_str(&format!("  last event: {} ago\n", fmt_secs(age))),
        None => out.push_str("  last event: none recorded yet\n"),
    }
    if let Some(end_ms) = last_batch_end_ms
        && let Ok(t) = local(end_ms)
    {
        out.push_str(&format!(
            "  last derived batch ended: {}\n",
            t.strftime("%Y-%m-%d %H:%M")
        ));
    }
    out.push_str(&format!(
        "  model: {}\n",
        if model_present {
            "present"
        } else {
            "not downloaded"
        }
    ));
    if let Some(err) = server_error {
        out.push_str(&format!("  warning: {err}\n"));
    }
    if let Some(age) = last_prune_age_secs {
        out.push_str(&format!("  last prune: {} ago\n", fmt_secs(age)));
    }
    out
}

fn status(data_dir: &Path, json: bool) -> anyhow::Result<()> {
    let liveness = query_daemon(&socket_path(data_dir));
    let conn = chronicle_core::storage::open(&data_dir.join("chronicle.db"))?;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let now_ms = Timestamp::now().as_millisecond();
    let age = |ms: i64| ((now_ms - ms) / 1000).max(0) as u64;
    let last_event_age_secs = chronicle_core::storage::latest_event_ts(&conn)?.map(age);
    let last_batch_end_ms = chronicle_core::storage::latest_batch_end(&conn)?;
    let model_present =
        chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir).is_some();
    let server_error = chronicle_core::storage::get_meta(&conn, "server_error")?;
    let last_prune_age_secs = chronicle_core::storage::get_meta(&conn, "last_prune_ts")?
        .and_then(|v| v.parse::<i64>().ok())
        .map(age);

    if json {
        let (liveness_str, daemon) = match &liveness {
            Liveness::Running(s) => ("healthy", Some(s)),
            Liveness::Stopped => ("stopped", None),
            Liveness::Unresponsive => ("unresponsive", None),
        };
        let doc = serde_json::json!({
            "liveness": liveness_str,
            "daemon": daemon,
            "last_event_age_secs": last_event_age_secs,
            "last_batch_end_ms": last_batch_end_ms,
            "model_present": model_present,
            "server_error": server_error,
            "last_prune_age_secs": last_prune_age_secs,
        });
        println!("{}", serde_json::to_string_pretty(&doc)?);
    } else {
        print!(
            "{}",
            format_status(
                &liveness,
                last_event_age_secs,
                last_batch_end_ms,
                model_present,
                server_error.as_deref(),
                last_prune_age_secs,
            )
        );
    }
    if !matches!(liveness, Liveness::Running(_)) {
        // Scriptable failure code without anyhow's "Error:" noise on top of
        // the report we just printed.
        std::process::exit(1);
    }
    Ok(())
}

fn report(
    data_dir: &Path,
    day: Option<&str>,
    week: Option<&str>,
    format: ReportFormat,
) -> anyhow::Result<()> {
    let conn = chronicle_core::storage::open(&data_dir.join("chronicle.db"))?;
    let tz = TimeZone::system();
    let parse = |s: &str| -> anyhow::Result<civil::Date> {
        s.parse().with_context(|| format!("bad date {s:?}"))
    };
    let days: Vec<civil::Date> = match (day, week) {
        (Some(d), None) => vec![parse(d)?],
        (None, given) => {
            let anchor = match given {
                Some(w) => parse(w)?,
                None => Zoned::now().date(),
            };
            let monday =
                chronicle_core::timeref::week_start(anchor).context("week start out of range")?;
            (0..7)
                .map(|i| Ok(monday.checked_add(i.days())?))
                .collect::<anyhow::Result<_>>()?
        }
        (Some(_), Some(_)) => unreachable!("clap conflicts_with"),
    };
    let lo = days[0].to_zoned(tz.clone())?.timestamp().as_millisecond();
    let hi = (days[days.len() - 1]
        .to_zoned(tz.clone())?
        .checked_add(1.day())?)
    .timestamp()
    .as_millisecond();
    let tasks = chronicle_core::storage::tasks_in_range(&conn, lo, hi)?;
    let r = chronicle_core::report::build(&tasks, days, &tz)?;
    match format {
        ReportFormat::Csv => print!("{}", chronicle_core::report::to_csv(&r)),
        ReportFormat::Md => print!("{}", chronicle_core::report::to_md(&r)),
    }
    Ok(())
}

fn dump(data_dir: &Path, day: Option<&str>) -> anyhow::Result<()> {
    let conn = chronicle_core::storage::open(&data_dir.join("chronicle.db"))?;

    let range = match day {
        Some(day) => {
            let date: civil::Date = day.parse().with_context(|| format!("bad --day {day:?}"))?;
            let start = date.to_zoned(TimeZone::system())?;
            let end = start.checked_add(1.day())?;
            println!(
                "chronicle dump — {date} ({tz})",
                tz = start.time_zone().iana_name().unwrap_or("local")
            );
            Some((
                start.timestamp().as_millisecond(),
                end.timestamp().as_millisecond(),
            ))
        }
        None => {
            println!("chronicle dump — all data");
            None
        }
    };
    let (lo, hi) = range.unwrap_or((i64::MIN, i64::MAX));

    let count =
        |sql: &str| -> anyhow::Result<i64> { Ok(conn.query_row(sql, [lo, hi], |r| r.get(0))?) };
    let events = count("SELECT COUNT(*) FROM events WHERE ts >= ?1 AND ts < ?2")?;
    let spans = count("SELECT COUNT(*) FROM spans WHERE start_ts >= ?1 AND start_ts < ?2")?;
    let batches = count("SELECT COUNT(*) FROM batches WHERE start_ts >= ?1 AND start_ts < ?2")?;
    let intervals = count("SELECT COUNT(*) FROM intervals WHERE start_ts >= ?1 AND start_ts < ?2")?;
    let tasks = count(
        "SELECT COUNT(DISTINCT task_id) FROM intervals WHERE start_ts >= ?1 AND start_ts < ?2",
    )?;
    println!(
        "events: {events}  spans: {spans}  batches: {batches}  tasks: {tasks}  intervals: {intervals}"
    );

    let mut stmt = conn.prepare(
        "SELECT ts, kind, app, title, idle, url FROM events
         WHERE ts >= ?1 AND ts < ?2 ORDER BY ts, id",
    )?;
    let mut rows = stmt.query([lo, hi])?;
    while let Some(row) = rows.next()? {
        let ts: i64 = row.get(0)?;
        let (kind, app, title): (String, String, String) = (row.get(1)?, row.get(2)?, row.get(3)?);
        let idle: Option<i64> = row.get(4)?;
        let url: Option<String> = row.get(5)?;
        let t = local(ts)?;
        match kind.as_str() {
            "afk" => println!(
                "{}  [afk] idle={}",
                t.strftime("%H:%M:%S"),
                idle.unwrap_or(0) == 1
            ),
            "url" => println!(
                "{}  [url] {app}: {title} <{}>",
                t.strftime("%H:%M:%S"),
                url.as_deref().unwrap_or("")
            ),
            _ => println!("{}  [{kind}] {app}: {title}", t.strftime("%H:%M:%S")),
        }
    }

    let mut stmt = conn.prepare(
        "SELECT start_ts, end_ts, app, title, kind, url FROM spans
         WHERE start_ts >= ?1 AND start_ts < ?2 ORDER BY start_ts, id",
    )?;
    let mut rows = stmt.query([lo, hi])?;
    while let Some(row) = rows.next()? {
        let (start, end): (i64, i64) = (row.get(0)?, row.get(1)?);
        let (app, title, kind): (String, String, String) = (row.get(2)?, row.get(3)?, row.get(4)?);
        let url: Option<String> = row.get(5)?;
        let start = local(start)?;
        let end = local(end)?;
        let label = match (kind.as_str(), url) {
            ("focus", Some(url)) => {
                format!(
                    " {app}: {title} <{}>",
                    chronicle_core::sessionizer::domain(&url)
                )
            }
            ("focus", None) => format!(" {app}: {title}"),
            _ => String::new(),
        };
        println!(
            "{} – {}  [{kind}]{label}",
            start.strftime("%H:%M:%S"),
            end.strftime("%H:%M:%S"),
        );
    }

    let mut stmt = conn.prepare(
        "SELECT i.start_ts, i.end_ts, t.id, t.label, t.project, i.confidence, t.source
         FROM intervals i JOIN tasks t ON t.id = i.task_id
         WHERE i.start_ts >= ?1 AND i.start_ts < ?2 ORDER BY i.start_ts, i.id",
    )?;
    let mut rows = stmt.query([lo, hi])?;
    while let Some(row) = rows.next()? {
        let (start, end): (i64, i64) = (row.get(0)?, row.get(1)?);
        let task_id: i64 = row.get(2)?;
        let (label, project): (String, Option<String>) = (row.get(3)?, row.get(4)?);
        let confidence: f64 = row.get(5)?;
        let source: String = row.get(6)?;
        let project = project.map(|p| format!(" [{p}]")).unwrap_or_default();
        let declared = if source == "user" { " (declared)" } else { "" };
        println!(
            "{} – {}  [task #{task_id}] {label}{project}{declared} ({confidence:.2})",
            local(start)?.strftime("%H:%M:%S"),
            local(end)?.strftime("%H:%M:%S"),
        );
    }
    Ok(())
}

fn local(ms: i64) -> anyhow::Result<Zoned> {
    Ok(chronicle_core::types::ms_to_ts(ms).to_zoned(TimeZone::system()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_cmd_parses_protocol_strings() {
        // Guards the literals `send_ctrl` writes against parser drift.
        assert_eq!(parse_ctrl_cmd("toggle"), Some(CtrlCmd::Toggle));
        assert_eq!(parse_ctrl_cmd("toggle\n"), Some(CtrlCmd::Toggle));
        assert_eq!(parse_ctrl_cmd("derive"), Some(CtrlCmd::DeriveNow));
        assert_eq!(parse_ctrl_cmd("status"), Some(CtrlCmd::Status));
        assert_eq!(parse_ctrl_cmd("shutdown"), None);
        assert_eq!(parse_ctrl_cmd("bogus"), None);
    }

    #[test]
    fn format_status_healthy() {
        let s = format_status(
            &Liveness::Running(DaemonStatus {
                uptime_secs: 8000,
                derive_active: true,
                idle_secs: Some(90),
                ui_open: true,
            }),
            Some(12),
            None,
            true,
            None,
            Some(3600),
        );
        assert!(s.contains("healthy"));
        assert!(s.contains("uptime 2h 13m"));
        assert!(s.contains("derive worker: running"));
        assert!(s.contains("user idle: 1m"));
        assert!(s.contains("last event: 12s ago"));
        assert!(s.contains("model: present"));
        assert!(s.contains("last prune: 1h 0m ago"));
    }

    #[test]
    fn format_status_stopped_still_reports_db_state() {
        let s = format_status(&Liveness::Stopped, None, None, false, None, None);
        assert!(s.contains("stopped"));
        assert!(!s.contains("healthy"));
        assert!(s.contains("last event: none recorded yet"));
        assert!(s.contains("model: not downloaded"));
    }

    #[test]
    fn format_status_unresponsive_and_server_error() {
        let s = format_status(
            &Liveness::Unresponsive,
            Some(400),
            None,
            true,
            Some("AW endpoint failed on port 5600: in use"),
            None,
        );
        assert!(s.contains("running but not responding"));
        assert!(s.contains("warning: AW endpoint failed on port 5600: in use"));
    }
}
