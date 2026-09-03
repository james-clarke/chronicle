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
    /// Internal: resident derivation worker (m27 chunk 3) — JSON requests on
    /// stdin, replies on stdout; exits after `worker_idle_secs` idle.
    #[command(hide = true)]
    DeriveWorker,
    /// Internal: one-shot derivation of a single batch.
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
        /// Scope retrieval to this task's workspace (m16).
        #[arg(long)]
        task: Option<i64>,
    },
    /// Internal: ephemeral AI-job worker (descriptions, suggestions, narratives).
    #[command(hide = true)]
    AiJob {
        #[arg(long)]
        id: i64,
    },
    /// Internal: one-off m27 coalesce of stored derived intervals (adjacent
    /// same-task pieces join unless an AFK ≥ 5 min or a user row lies between).
    #[command(hide = true)]
    BackfillCoalesce {
        /// Batches starting on or after this local day.
        #[arg(long, value_name = "YYYY-MM-DD")]
        since: String,
        /// Report what would change without writing.
        #[arg(long)]
        dry_run: bool,
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
    /// Print the drafted standup for a day (default: yesterday).
    Standup {
        #[arg(long, value_name = "YYYY-MM-DD")]
        day: Option<String>,
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
        /// Skip MCP context for --batch cases (offline; replay never gathers it).
        #[arg(long)]
        no_mcp: bool,
        /// Re-derive every batch a correction touched and score the corrected
        /// outcome (m27 chunk 2).
        #[arg(long)]
        replay: bool,
        /// Replay: only corrections made in the last N days.
        #[arg(long, default_value_t = 7)]
        since: u64,
        /// Replay: write the per-probe results as JSON here for diffing.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Sign in to Google Calendar (loopback OAuth) and store the refresh
    /// token in `<data dir>/google.toml`.
    GcalLogin {
        /// OAuth client id; falls back to $CHRONICLE_GOOGLE_CLIENT_ID.
        #[arg(long)]
        client_id: Option<String>,
        /// OAuth client secret; falls back to $CHRONICLE_GOOGLE_CLIENT_SECRET.
        #[arg(long)]
        client_secret: Option<String>,
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
        Cmd::Standup { day } => standup_cmd(&data_dir, day.as_deref()),
        Cmd::Derive { batch } => derive_worker(&data_dir, batch),
        Cmd::DeriveWorker => derive_resident(&data_dir),
        Cmd::McpCheck => mcp_check(&data_dir),
        Cmd::Model { cmd } => model_cmd(&data_dir, cmd),
        Cmd::Bench {
            fixtures,
            batch,
            digest,
            only,
            model,
            no_mcp,
            replay,
            since,
            out,
        } => {
            if replay {
                replay_eval(&data_dir, since, model.as_deref(), out.as_deref())
            } else {
                bench(
                    &data_dir,
                    &fixtures,
                    &batch,
                    digest,
                    only.as_deref(),
                    model.as_deref(),
                    no_mcp,
                )
            }
        }
        Cmd::ChatWorker { conversation, task } => chat_worker(&data_dir, conversation, task),
        Cmd::AiJob { id } => ai_job_worker(&data_dir, id),
        Cmd::BackfillDescriptions { limit } => backfill_descriptions(&data_dir, limit),
        Cmd::BackfillCoalesce { since, dry_run } => backfill_coalesce(&data_dir, &since, dry_run),
        Cmd::GcalLogin {
            client_id,
            client_secret,
        } => gcal_login(&data_dir, client_id, client_secret),
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
        /// `task_id` scopes retrieval to that task's workspace (m16).
        Switch {
            conversation_id: i64,
            #[serde(default)]
            task_id: Option<i64>,
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

/// Warm chat worker, spawned by the UI when the chat panel opens and killed
/// when it closes. The model stays resident between questions; retrieval is
/// local-DB only (time-ref range or FTS, in chronicle_core::chat).
fn chat_worker(
    data_dir: &Path,
    mut conversation_id: i64,
    mut task_scope: Option<i64>,
) -> anyhow::Result<()> {
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
                task_id,
            }) => {
                conversation_id = id;
                task_scope = task_id;
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
        let context = match match task_scope {
            Some(task_id) => chronicle_core::chat::build_task_context(
                &conn,
                task_id,
                &jiff::tz::TimeZone::system(),
            ),
            None => chronicle_core::chat::build_context(&conn, &ask, &Zoned::now()),
        } {
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
fn bench_models(
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
fn replay_eval(
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

/// Accumulates the model's raw output and reports the human-readable
/// progress label whenever it changes.
#[derive(Default)]
struct ProgressRelay {
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
fn progress_label(partial: &str, open: &[chronicle_core::types::OpenTask]) -> Option<String> {
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

fn ref_label(from_ref: &str, open: &[chronicle_core::types::OpenTask]) -> Option<String> {
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
fn consolidate_day(
    conn: &mut rusqlite::Connection,
    config: &Config,
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
        let input = consolidate::render_input(&remaining);
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
fn live_pass(
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
const OPEN_CAP: usize = 16;
/// Open tasks offered to the live prompt.
const LIVE_OPEN_CAP: usize = 16;
/// A live label is a guess over a short window; the batch tier confirms it.
const LIVE_CONFIDENCE_SCALE: f64 = 0.8;

/// Everything the model sees for one batch, plus what post-processing needs.
struct BatchDigest {
    digest: String,
    open: Vec<chronicle_core::types::OpenTask>,
    spans: Vec<chronicle_core::sessionizer::SpanDraft>,
    /// AFK gaps ≥ 5 min in window minutes (coalesce boundaries).
    gaps: Vec<(i64, i64)>,
    activity: Vec<chronicle_core::types::ActivityEvent>,
}

/// Build the batch prompt the way production builds it, so bench and the
/// replay eval score the same digest the worker would send (m27 chunk 2
/// parity). `open` is the caller's open-task list; pre-pass hints over the
/// window are appended so the model can link to them by ref.
fn build_batch_digest(
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
fn build_window_digest(
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

fn derive_worker(data_dir: &Path, batch_id: i64) -> anyhow::Result<()> {
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
fn derive_resident(data_dir: &Path) -> anyhow::Result<()> {
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
                let result = consolidate_day(&mut conn, &config, &model, lo, hi, &mut |label| {
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
fn derive_batch(
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
                // session's `gitBranch` is not vcs activity.
                let vcs: Vec<_> = activity
                    .iter()
                    .filter(|e| e.kind.is_vcs() || e.kind.is_pr())
                    .cloned()
                    .collect();
                for (task_id, key) in
                    chronicle_core::anchor::anchor_tasks(&stored, &prior, &vcs, &re)
                {
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

/// Ephemeral AI-job worker: claim job → kind-specific inference → store
/// result → exit. Any failure marks the job failed (retry-once via attempts
/// cap), mirroring the derive worker.
fn ai_job_worker(data_dir: &Path, job_id: i64) -> anyhow::Result<()> {
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
struct SkipJob(String);

impl std::fmt::Display for SkipJob {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SkipJob {}

fn run_ai_job(
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
            let (label, project): (String, Option<String>) = conn.query_row(
                "SELECT label, project FROM tasks WHERE id=?1",
                [task_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
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
            let (label, project): (String, Option<String>) = conn.query_row(
                "SELECT label, project FROM tasks WHERE id=?1",
                [task_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
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
fn standup_digest_text(
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
fn standup_activity_fallback(
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
fn civil_days(lo: i64, hi: i64, tz: &TimeZone) -> anyhow::Result<Vec<jiff::civil::Date>> {
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

/// An AFK span this long is a hard boundary: it splits time intervals and
/// blocks coalescing across it (M5 time honesty).
const AFK_SPLIT_MS: i64 = 5 * 60_000;
/// Unlabelled space two same-task pieces may join across.
const COALESCE_GAP_MIN: i64 = 2;

/// AFK gaps ≥ 5 min as `(start, end)` in whole minutes from `origin_ms`,
/// widened outward so a gap never looks shorter than it is.
fn afk_gaps_min(
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

/// One-off after the m27 chunk 1 deploy: apply `merge::coalesce` to stored
/// derived rows, batch by batch, in ms. AFK gaps come from the batch's spans;
/// the user's own rows and rows a correction points at block a join the
/// same way (and the latter stay put: `corrections.interval_id` is a
/// foreign key). Take a `.bak` of the DB
/// first; the daemon may run alongside (each batch is one transaction and a
/// re-derivation replaces the batch's rows anyway).
fn backfill_coalesce(data_dir: &Path, since: &str, dry_run: bool) -> anyhow::Result<()> {
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

/// Per-day counter in `meta` (`fetches:2026-09-03`, `posts:2026-09-03`):
/// what Settings › Storage reports as having left this machine today.
/// Best-effort — a meta write must never block the thing it counts.
pub(crate) fn bump_day_counter(conn: &rusqlite::Connection, prefix: &str) {
    bump_day_counter_by(conn, prefix, 1);
}

/// [`bump_day_counter`] for a batch: one mcp gather runs several calls.
pub(crate) fn bump_day_counter_by(conn: &rusqlite::Connection, prefix: &str, by: usize) {
    use chronicle_core::storage;
    if by == 0 {
        return;
    }
    let key = day_counter_key(prefix);
    let n = storage::get_meta(conn, &key)
        .ok()
        .flatten()
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let _ = storage::set_meta(conn, &key, Some(&(n + by as i64).to_string()));
}

/// The key [`bump_day_counter`] writes and the settings panel reads.
pub(crate) fn day_counter_key(prefix: &str) -> String {
    format!("{prefix}:{}", jiff::Zoned::now().date())
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
    Consolidate,
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
    /// Home's "tidy today": run the day tier now.
    Consolidate,
    Status,
}

fn parse_ctrl_cmd(line: &str) -> Option<CtrlCmd> {
    match line.trim() {
        "toggle" => Some(CtrlCmd::Toggle),
        "derive" => Some(CtrlCmd::DeriveNow),
        "consolidate" => Some(CtrlCmd::Consolidate),
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
                    Some(CtrlCmd::Consolidate) => {
                        if tx.send(CtrlMsg::Consolidate).is_err() {
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
pub(crate) fn spawn_chat_worker(
    conversation_id: i64,
    task_scope: Option<i64>,
) -> std::io::Result<Child> {
    let mut cmd = Command::new(own_exe()?);
    cmd.args([
        "chat-worker",
        "--conversation",
        &conversation_id.to_string(),
    ]);
    if let Some(task_id) = task_scope {
        cmd.args(["--task", &task_id.to_string()]);
    }
    cmd.stdin(Stdio::piped())
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
    spawn_capture(&config, data_dir, tx.clone())?;
    // Port taken (a real aw-server?) must not kill capture: log, warn in UI.
    let api_key = wakapi_api_key(&conn)?;
    // The key exists either way so Settings can show it; the switch decides
    // whether the routes accept it.
    let server_error =
        match chronicle_server::spawn(&config, tx, config.editor_heartbeats.then_some(api_key)) {
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
    let mut scheduler = Scheduler::new();
    let _ = chronicle_core::storage::set_derive_progress(&conn, None);
    let distractions = chronicle_core::evidence::compile_patterns(&config.distraction_patterns);
    let mut idle_since: Option<i64> = None;
    // Fires once per idle stretch: remembers which idle_since epoch already
    // queued checkpoints, cleared when the user comes back.
    let mut checkpointed_idle: Option<i64> = None;
    let mut next_refresh = Instant::now() + SESSIONIZE_EVERY;
    let mut last_prepass = Instant::now();
    let exit_reason = 'daemon: loop {
        let timeout = next_refresh.saturating_duration_since(Instant::now());
        // Cloned per iteration so the select borrow does not pin the scheduler.
        let resident_rx = scheduler.resident_rx();
        crossbeam_channel::select! {
            recv(resident_rx) -> reply => match reply {
                Ok(reply) => scheduler.on_reply(&conn, reply),
                Err(_) => scheduler.resident_disconnected(&conn),
            },
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
                    Ok(CtrlMsg::Consolidate) => {
                        scheduler.consolidate_requested = true;
                        scheduler.tick(&conn, &config, data_dir, idle_since, true);
                    }
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
                if config.prepass_secs > 0
                    && last_prepass.elapsed() >= Duration::from_secs(u64::from(config.prepass_secs))
                {
                    last_prepass = Instant::now();
                    match chronicle_core::prepass::run(&mut conn, &config, now) {
                        Ok(placed) if !placed.is_empty() => {
                            tracing::debug!(n = placed.len(), "pre-pass placed runs");
                        }
                        Ok(_) => {}
                        Err(e) => tracing::error!("pre-pass failed: {e}"),
                    }
                    if let Err(e) =
                        chronicle_core::proposals::refresh(&mut conn, now, &distractions)
                    {
                        tracing::error!("proposals refresh failed: {e}");
                    }
                    scheduler.maybe_live(&conn, &config, data_dir, idle_since);
                }
                scheduler.tick(&conn, &config, data_dir, idle_since, false);
                maybe_enqueue_checkpoints(&conn, &config, idle_since, &mut checkpointed_idle, now);
                maybe_enqueue_standup(&conn, now);
                reap_ui(&mut ui_child);
                next_refresh = Instant::now() + SESSIONIZE_EVERY;
            }
        }
    };
    tracing::info!("shutting down");
    // Child drop leaks the OS process — kills must be explicit.
    scheduler.shutdown(&conn);
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

/// Lunch-scale AFK (m16): once per idle stretch, queue a background
/// checkpoint job for every task with activity since its last checkpoint.
/// Day end needs no separate trigger — the evening's long idle is one.
fn maybe_enqueue_checkpoints(
    conn: &rusqlite::Connection,
    config: &Config,
    idle_since: Option<i64>,
    checkpointed_idle: &mut Option<i64>,
    now: Timestamp,
) {
    use chronicle_core::storage;
    if config.checkpoint_afk_secs == 0 {
        return;
    }
    let Some(since) = idle_since else {
        *checkpointed_idle = None;
        return;
    };
    if *checkpointed_idle == Some(since)
        || now.as_millisecond() - since < i64::from(config.checkpoint_afk_secs) * 1000
    {
        return;
    }
    *checkpointed_idle = Some(since);
    let tasks = match storage::tasks_needing_checkpoint(conn, since) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("checkpoint eligibility query failed: {e}");
            return;
        }
    };
    for task_id in tasks {
        if let Err(e) = storage::enqueue_ai_job(
            conn,
            now,
            "checkpoint",
            0,
            &storage::task_description_payload(task_id),
        ) {
            tracing::error!(task_id, "checkpoint enqueue failed: {e}");
        }
    }
}

/// Standup draft trigger: once yesterday has journal entries and no draft or
/// queued job exists for it, queue a background standup job. The gate is
/// pure DB state (draft row + job dedupe), so it is restart-safe and cheap
/// to re-check every tick.
fn maybe_enqueue_standup(conn: &rusqlite::Connection, now: Timestamp) {
    use chronicle_core::storage;
    let tz = TimeZone::system();
    let today = now.to_zoned(tz.clone()).date();
    let Ok(yd) = today.checked_sub(jiff::Span::new().days(1)) else {
        return;
    };
    let day = yd.to_string();
    if storage::get_standup_draft(conn, &day)
        .map(|d| d.is_some())
        .unwrap_or(true)
        || storage::pending_standup_job(conn, &day).unwrap_or(true)
    {
        return;
    }
    let (Ok(lo), Ok(hi)) = (yd.to_zoned(tz.clone()), today.to_zoned(tz)) else {
        return;
    };
    let (lo, hi) = (
        lo.timestamp().as_millisecond(),
        hi.timestamp().as_millisecond(),
    );
    match storage::standup_digest(conn, lo, hi) {
        Ok(rows) if !rows.is_empty() => {
            if let Err(e) =
                storage::enqueue_ai_job(conn, now, "standup", 0, &storage::standup_payload(&day))
            {
                tracing::error!(%day, "standup enqueue failed: {e}");
            }
        }
        Ok(_) => {}
        Err(e) => tracing::error!(%day, "standup eligibility query failed: {e}"),
    }
}

const DERIVE_TIMEOUT: Duration = Duration::from_secs(300);
/// "Low 1-min load" gate for deriving while the user is active.
const LOW_LOAD: f64 = 1.0;
const BATTERY_DEFER_PCT: u32 = 30;

/// The resident derive worker (m27 chunk 3): one process holding the model
/// and a KV cache with the instruction prefix. Requests go down its stdin; a
/// reader thread relays its replies.
struct Resident {
    child: Child,
    stdin: std::process::ChildStdin,
    rx: crossbeam_channel::Receiver<deriveproto::Reply>,
    ready: bool,
    /// The request in flight and when it was sent.
    busy: Option<(Job, Instant)>,
    last_used: Instant,
    /// The feed's "deriving…" row, mirrored to meta `derive_progress`.
    progress: Option<chronicle_core::storage::DeriveProgress>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Job {
    Batch(i64),
    Live { lo: i64, hi: i64 },
    Consolidate { lo: i64, hi: i64 },
}

/// The day tier runs at the first long AFK after this local hour …
const CONSOLIDATE_AFTER_HOUR: i8 = 14;
/// … or unconditionally (under the batch gates) after this one.
const CONSOLIDATE_FALLBACK_HOUR: i8 = 18;

/// The live tier looks back at most this far for the current stretch.
const LIVE_LOOKBACK_MS: i64 = 15 * 60_000;
/// A live pass needs this much focus in its window.
const LIVE_MIN_FOCUS_MS: i64 = 5 * 60_000;
/// A live pass slower than this skips the next one (self-throttle).
const LIVE_SLOW: Duration = Duration::from_secs(60);

/// Daemon-side backstop over the worker's own idle exit.
const RESIDENT_IDLE_GRACE_SECS: u64 = 60;

struct Scheduler {
    /// AI jobs stay one-shot subprocesses (`Describer`/`ChatModel`); at most
    /// one inference process works at a time, derive or ai-job.
    ai_job: Option<(Child, Instant, i64)>,
    resident: Option<Resident>,
    last_live: Option<Instant>,
    /// Wall time of the last live pass; over `LIVE_SLOW` skips one pass.
    last_live_took: Option<Duration>,
    /// Home's "tidy today" asked for a run regardless of time or stamp.
    consolidate_requested: bool,
    /// A run failed today; wait for a request rather than retrying.
    consolidate_failed_day: Option<String>,
}

static NEVER_REPLY: std::sync::LazyLock<crossbeam_channel::Receiver<deriveproto::Reply>> =
    std::sync::LazyLock::new(crossbeam_channel::never);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(crate) struct DaemonStatus {
    pub(crate) uptime_secs: u64,
    pub(crate) derive_active: bool,
    /// What the inference slot is running ("derive batch 71", "ai job 12")
    /// and for how long; absent when idle or from a pre-m27 daemon.
    #[serde(default)]
    pub(crate) worker: Option<String>,
    #[serde(default)]
    pub(crate) worker_secs: Option<u64>,
    /// The resident derive worker is up with the model loaded.
    #[serde(default)]
    pub(crate) model_resident: bool,
    pub(crate) idle_secs: Option<u64>,
    pub(crate) ui_open: bool,
}

fn status_json(
    scheduler: &Scheduler,
    idle_since: Option<i64>,
    ui_child: &mut Option<Child>,
    started: Instant,
) -> String {
    let busy = scheduler.busy();
    let status = DaemonStatus {
        uptime_secs: started.elapsed().as_secs(),
        derive_active: busy.is_some(),
        worker: busy.as_ref().map(|(what, _)| what.clone()),
        worker_secs: busy.as_ref().map(|(_, since)| since.elapsed().as_secs()),
        model_resident: scheduler.resident.as_ref().is_some_and(|r| r.ready),
        idle_secs: idle_since
            .map(|since| ((Timestamp::now().as_millisecond() - since) / 1000).max(0) as u64),
        ui_open: ui_child
            .as_mut()
            .is_some_and(|c| matches!(c.try_wait(), Ok(None))),
    };
    serde_json::to_string(&status).unwrap_or_default()
}

#[derive(Debug, PartialEq)]
enum ResidentAction {
    Keep,
    KillTimeout,
    KillIdle,
}

/// Pure state rule for the resident worker: a request past `timeout` kills
/// it; an idle worker past `idle` is reaped (it exits itself first).
fn resident_action(
    busy_since: Option<Instant>,
    last_used: Instant,
    now: Instant,
    timeout: Duration,
    idle: Duration,
) -> ResidentAction {
    match busy_since {
        Some(since) if now.duration_since(since) >= timeout => ResidentAction::KillTimeout,
        Some(_) => ResidentAction::Keep,
        None if now.duration_since(last_used) >= idle => ResidentAction::KillIdle,
        None => ResidentAction::Keep,
    }
}

impl Scheduler {
    fn new() -> Self {
        Self {
            ai_job: None,
            resident: None,
            last_live: None,
            last_live_took: None,
            consolidate_requested: false,
            consolidate_failed_day: None,
        }
    }

    /// The resident worker's reply channel for the daemon's select loop (a
    /// never-ready channel when no worker is up).
    fn resident_rx(&self) -> crossbeam_channel::Receiver<deriveproto::Reply> {
        match &self.resident {
            Some(r) => r.rx.clone(),
            None => NEVER_REPLY.clone(),
        }
    }

    /// What the inference slot is doing, and since when.
    fn busy(&self) -> Option<(String, Instant)> {
        if let Some((_, started, id)) = &self.ai_job {
            return Some((format!("ai job {id}"), *started));
        }
        if let Some(r) = &self.resident
            && let Some((job, since)) = r.busy
        {
            let what = match job {
                Job::Batch(id) => format!("derive batch {id}"),
                Job::Live { .. } => "live pass".to_owned(),
                Job::Consolidate { .. } => "tidying today".to_owned(),
            };
            return Some((what, since));
        }
        None
    }

    /// Live tier (chunk 4), on the pre-pass timer: while the user is active
    /// and the slot is free, label the stretch since the last boundary.
    fn maybe_live(
        &mut self,
        conn: &rusqlite::Connection,
        config: &Config,
        data_dir: &Path,
        idle_since: Option<i64>,
    ) {
        use chronicle_core::storage;
        if config.live_secs == 0 || idle_since.is_some() || self.busy().is_some() {
            return;
        }
        let every = Duration::from_secs(u64::from(config.live_secs));
        if self.last_live.is_some_and(|t| t.elapsed() < every) {
            return;
        }
        if let Some(took) = self.last_live_took.take()
            && took > LIVE_SLOW
        {
            tracing::info!(took_secs = took.as_secs(), "live pass slow; skipping one");
            self.last_live = Some(Instant::now());
            return;
        }
        if !load_1min().is_some_and(|l| l < LOW_LOAD) || on_low_battery() {
            return;
        }
        if chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir).is_none() {
            return;
        }
        let now = Timestamp::now().as_millisecond();
        // The live tier labels the tail only: a stretch inside a closed batch
        // belongs to that batch's derive, which replaces live rows it covers.
        let tail_start = storage::latest_batch_end(conn).ok().flatten().unwrap_or(0);
        let floor = (now - LIVE_LOOKBACK_MS).max(tail_start);
        let lo = match storage::live_window_start(conn, floor, now) {
            Ok(lo) => lo,
            Err(e) => {
                tracing::error!("live window query failed: {e}");
                return;
            }
        };
        let focus = storage::focus_ms_in(conn, lo, now).unwrap_or(0);
        if focus < LIVE_MIN_FOCUS_MS {
            return;
        }
        self.last_live = Some(Instant::now());
        self.dispatch(conn, Job::Live { lo, hi: now });
    }

    fn tick(
        &mut self,
        conn: &rusqlite::Connection,
        config: &Config,
        data_dir: &Path,
        idle_since: Option<i64>,
        force: bool,
    ) {
        use chronicle_core::storage;
        self.reap_ai_job(conn);
        self.poll_resident(conn, config);
        if self.busy().is_some() {
            return; // one inference process at a time
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
            self.spawn_ai_job(job_id);
            return;
        }
        // Derivation stays ahead of background summarization.
        match storage::next_eligible_batch(conn) {
            Ok(Some(batch_id)) => {
                self.dispatch(conn, Job::Batch(batch_id));
                return;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!("eligible-batch query failed: {e}");
                return;
            }
        }
        if self.maybe_consolidate(conn, config, idle_since) {
            return;
        }
        if let Ok(Some(job_id)) = storage::next_eligible_ai_job(conn, i64::MIN) {
            self.spawn_ai_job(job_id);
        }
    }

    /// Day tier (chunk 6): once per local day, at the first AFK ≥
    /// `checkpoint_afk_secs` after 14:00, at 18:00 regardless, or when Home
    /// asked. Returns true when a run was dispatched.
    fn maybe_consolidate(
        &mut self,
        conn: &rusqlite::Connection,
        config: &Config,
        idle_since: Option<i64>,
    ) -> bool {
        use chronicle_core::storage;
        let now = Timestamp::now();
        let local = now.to_zoned(TimeZone::system());
        let day = local.date().to_string();
        let requested = std::mem::take(&mut self.consolidate_requested);
        if !requested {
            if self.consolidate_failed_day.as_deref() == Some(&day)
                || matches!(storage::consolidation_of_day(conn, &day), Ok(Some(_)))
            {
                return false;
            }
            let idle_long = config.checkpoint_afk_secs > 0
                && idle_since.is_some_and(|since| {
                    now.as_millisecond() - since >= i64::from(config.checkpoint_afk_secs) * 1000
                });
            let hour = local.hour();
            let due =
                (hour >= CONSOLIDATE_AFTER_HOUR && idle_long) || hour >= CONSOLIDATE_FALLBACK_HOUR;
            if !due {
                return false;
            }
        }
        let Ok(start) = local.start_of_day() else {
            return false;
        };
        let lo = start.timestamp().as_millisecond();
        let hi = now.as_millisecond();
        tracing::info!(%day, requested, "consolidation dispatched");
        self.dispatch(conn, Job::Consolidate { lo, hi });
        true
    }

    fn reap_ai_job(&mut self, conn: &rusqlite::Connection) {
        use chronicle_core::storage;
        let Some((child, started, id)) = &mut self.ai_job else {
            return;
        };
        let id = *id;
        // A worker that died before reporting leaves its row `running`;
        // count that as the failed attempt it was.
        let mark_dead = |conn: &rusqlite::Connection| {
            let stuck = matches!(
                storage::ai_job_status(conn, id),
                Ok(Some((ref s, _))) if s == "running"
            );
            if stuck {
                let _ = storage::fail_ai_job(conn, id, "worker died");
            }
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                mark_dead(conn);
                if !status.success() {
                    tracing::warn!(id, %status, "ai-job worker failed");
                }
                self.ai_job = None;
            }
            Ok(None) => {
                if started.elapsed() >= DERIVE_TIMEOUT {
                    tracing::warn!(id, "ai-job worker timed out; killing");
                    let _ = child.kill();
                    let _ = child.wait();
                    mark_dead(conn);
                    self.ai_job = None;
                }
            }
            Err(e) => {
                tracing::error!(id, "ai-job worker wait failed: {e}");
                self.ai_job = None;
            }
        }
    }

    /// One reply from the resident worker (the daemon's select loop feeds
    /// these as they arrive; `poll_resident` drains stragglers).
    fn on_reply(&mut self, conn: &rusqlite::Connection, msg: deriveproto::Reply) {
        use chronicle_core::storage;
        use deriveproto::Reply;
        let Some(r) = &mut self.resident else {
            return;
        };
        match msg {
            Reply::Ready => {
                r.ready = true;
                // The request queued behind model load; the timeout clock
                // covers inference only.
                if let Some((_, since)) = &mut r.busy {
                    *since = Instant::now();
                }
                tracing::info!("derive worker ready");
            }
            Reply::Progress { label } => {
                if let Some(p) = &mut r.progress {
                    p.label = label;
                    if let Err(e) = storage::set_derive_progress(conn, Some(p)) {
                        tracing::warn!("derive progress write failed: {e}");
                    }
                }
            }
            Reply::Done {
                batch_id,
                intervals,
            } => {
                tracing::info!(?batch_id, intervals, "derive worker done");
                self.finish_job(conn);
            }
            Reply::Err { batch_id, message } => {
                tracing::warn!(?batch_id, "derive worker: {message}");
                if let Some((Job::Consolidate { lo, .. }, _)) = r.busy {
                    self.consolidate_failed_day = Some(
                        chronicle_core::types::ms_to_ts(lo)
                            .to_zoned(TimeZone::system())
                            .date()
                            .to_string(),
                    );
                }
                self.finish_job(conn);
            }
        }
    }

    /// The in-flight request is over: free the slot, clear the feed row,
    /// remember how long a live pass took.
    fn finish_job(&mut self, conn: &rusqlite::Connection) {
        let Some(r) = &mut self.resident else {
            return;
        };
        if let Some((Job::Live { .. }, since)) = r.busy.take() {
            self.last_live_took = Some(since.elapsed());
        }
        r.last_used = Instant::now();
        r.progress = None;
        let _ = chronicle_core::storage::set_derive_progress(conn, None);
    }

    /// The reader thread ended (worker stdout closed): treat as dead.
    fn resident_disconnected(&mut self, conn: &rusqlite::Connection) {
        if let Some(mut r) = self.resident.take() {
            tracing::warn!(busy = ?r.busy.map(|b| b.0), "derive worker disconnected");
            let _ = r.child.kill();
            let _ = r.child.wait();
            if let Some(day) = fail_stuck(conn, r.busy) {
                self.consolidate_failed_day = Some(day);
            }
            let _ = chronicle_core::storage::set_derive_progress(conn, None);
        }
    }

    /// Drain stragglers, then apply the timeout/idle rule.
    fn poll_resident(&mut self, conn: &rusqlite::Connection, config: &Config) {
        while let Some(msg) = self.resident.as_ref().and_then(|r| r.rx.try_recv().ok()) {
            self.on_reply(conn, msg);
        }
        let Some(r) = &mut self.resident else {
            return;
        };
        let action = resident_action(
            r.busy.map(|(_, since)| since),
            r.last_used,
            Instant::now(),
            DERIVE_TIMEOUT,
            Duration::from_secs(u64::from(config.worker_idle_secs) + RESIDENT_IDLE_GRACE_SECS),
        );
        match r.child.try_wait() {
            Ok(Some(status)) => {
                if r.busy.is_some() || !status.success() {
                    tracing::warn!(%status, busy = ?r.busy.map(|b| b.0), "derive worker exited");
                } else {
                    tracing::info!("derive worker exited (idle)");
                }
                let lost = fail_stuck(conn, r.busy);
                let _ = chronicle_core::storage::set_derive_progress(conn, None);
                self.resident = None;
                if lost.is_some() {
                    self.consolidate_failed_day = lost;
                }
            }
            Ok(None) => match action {
                ResidentAction::Keep => {}
                ResidentAction::KillTimeout => {
                    tracing::warn!(busy = ?r.busy.map(|b| b.0), "derive worker timed out; killing");
                    let _ = r.child.kill();
                    let _ = r.child.wait();
                    let lost = fail_stuck(conn, r.busy);
                    let _ = chronicle_core::storage::set_derive_progress(conn, None);
                    self.resident = None;
                    if lost.is_some() {
                        self.consolidate_failed_day = lost;
                    }
                }
                ResidentAction::KillIdle => {
                    tracing::info!("derive worker idle past grace; killing");
                    let _ = r.child.kill();
                    let _ = r.child.wait();
                    self.resident = None;
                }
            },
            Err(e) => {
                tracing::error!("derive worker wait failed: {e}");
                let _ = r.child.kill();
                let lost = fail_stuck(conn, r.busy);
                let _ = chronicle_core::storage::set_derive_progress(conn, None);
                self.resident = None;
                if lost.is_some() {
                    self.consolidate_failed_day = lost;
                }
            }
        }
    }

    fn spawn_ai_job(&mut self, job_id: i64) {
        match spawn_ai_job_worker(job_id) {
            Ok(child) => {
                tracing::info!(id = job_id, "ai-job worker spawned");
                self.ai_job = Some((child, Instant::now(), job_id));
            }
            Err(e) => tracing::error!(id = job_id, "failed to spawn ai-job worker: {e}"),
        }
    }

    /// Send a request to the resident worker, spawning it first if needed.
    /// The request queues in the pipe behind model load; `Ready` restarts the
    /// timeout clock so it covers inference only.
    fn dispatch(&mut self, conn: &rusqlite::Connection, job: Job) {
        use chronicle_core::storage;
        use std::io::Write;
        let (request, progress) = match job {
            Job::Batch(batch_id) => {
                let (start_ts, end_ts) = storage::batch_row(conn, batch_id)
                    .ok()
                    .flatten()
                    .map_or((0, 0), |b| (b.start_ts, b.end_ts));
                (
                    deriveproto::Request::Derive { batch_id },
                    storage::DeriveProgress {
                        kind: "batch".into(),
                        batch_id: Some(batch_id),
                        start_ts,
                        end_ts,
                        started_ts: Timestamp::now().as_millisecond(),
                        label: String::new(),
                    },
                )
            }
            Job::Live { lo, hi } => (
                deriveproto::Request::Live { lo, hi },
                storage::DeriveProgress {
                    kind: "live".into(),
                    batch_id: None,
                    start_ts: lo,
                    end_ts: hi,
                    started_ts: Timestamp::now().as_millisecond(),
                    label: String::new(),
                },
            ),
            Job::Consolidate { lo, hi } => (
                deriveproto::Request::Consolidate { lo, hi },
                storage::DeriveProgress {
                    kind: "day".into(),
                    batch_id: None,
                    start_ts: lo,
                    end_ts: hi,
                    started_ts: Timestamp::now().as_millisecond(),
                    label: String::new(),
                },
            ),
        };
        if self.resident.is_none() {
            match spawn_resident() {
                Ok(r) => {
                    tracing::info!("derive worker spawned");
                    self.resident = Some(r);
                }
                Err(e) => {
                    tracing::error!("failed to spawn derive worker: {e}");
                    return;
                }
            }
        }
        let Some(r) = self.resident.as_mut() else {
            return;
        };
        let mut line = serde_json::to_string(&request).expect("request serializes");
        line.push('\n');
        match r
            .stdin
            .write_all(line.as_bytes())
            .and_then(|()| r.stdin.flush())
        {
            Ok(()) => {
                tracing::info!(?job, "derive requested");
                r.busy = Some((job, Instant::now()));
                if let Err(e) = storage::set_derive_progress(conn, Some(&progress)) {
                    tracing::warn!("derive progress write failed: {e}");
                }
                r.progress = Some(progress);
            }
            Err(e) => {
                tracing::error!(?job, "derive worker pipe broken: {e}");
                let _ = r.child.kill();
                let _ = r.child.wait();
                self.resident = None;
            }
        }
    }

    /// Kill whatever is running; the rows they were working on go back to
    /// retryable states. SIGKILL is safe: `store_derivation` commits in one
    /// transaction and `fail_batch` records the burned attempt immediately.
    fn shutdown(&mut self, conn: &rusqlite::Connection) {
        use chronicle_core::storage;
        if let Some((mut child, _, id)) = self.ai_job.take() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = storage::fail_ai_job(conn, id, "daemon shutdown");
        }
        if let Some(mut r) = self.resident.take() {
            let _ = r.child.kill();
            let _ = r.child.wait();
            let _ = fail_stuck(conn, r.busy);
            let _ = storage::set_derive_progress(conn, None);
        }
    }
}

/// A batch the worker never answered for keeps its `running` row; count
/// that as the failed attempt it was. A lost consolidation returns its day
/// so the scheduler stops retrying it until asked. Live passes have no row
/// to repair.
fn fail_stuck(conn: &rusqlite::Connection, busy: Option<(Job, Instant)>) -> Option<String> {
    use chronicle_core::storage;
    match busy {
        Some((Job::Batch(id), _)) => {
            if matches!(storage::batch_status(conn, id), Ok(Some(ref s)) if s == "running") {
                let _ = storage::fail_batch(conn, id);
            }
            None
        }
        Some((Job::Consolidate { lo, .. }, _)) => Some(
            chronicle_core::types::ms_to_ts(lo)
                .to_zoned(TimeZone::system())
                .date()
                .to_string(),
        ),
        _ => None,
    }
}

fn spawn_resident() -> std::io::Result<Resident> {
    use std::io::BufRead;
    // Worker logging goes to the log file; stderr would only carry llama's
    // own noise.
    let mut child = Command::new(own_exe()?)
        .arg("derive-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdin = child.stdin.take().expect("stdin piped");
    let stdout = child.stdout.take().expect("stdout piped");
    let (tx, rx) = crossbeam_channel::unbounded();
    std::thread::Builder::new()
        .name("derive-reader".into())
        .spawn(move || {
            for line in std::io::BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if let Ok(msg) = serde_json::from_str::<deriveproto::Reply>(&line)
                    && tx.send(msg).is_err()
                {
                    break;
                }
            }
        })?;
    Ok(Resident {
        child,
        stdin,
        rx,
        ready: false,
        busy: None,
        last_used: Instant::now(),
        progress: None,
    })
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

/// Key the WakaTime heartbeat routes authenticate with, generated once and
/// shown (with a copy button) in Settings › Connections. 32 url-safe
/// characters from `/dev/urandom`; the 64-character alphabet divides 256, so
/// the byte-to-character map is unbiased.
fn wakapi_api_key(conn: &rusqlite::Connection) -> anyhow::Result<String> {
    use chronicle_core::storage;
    use std::io::Read;
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    if let Some(key) = storage::get_meta(conn, "wakapi_api_key")?
        && !key.is_empty()
    {
        return Ok(key);
    }
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut bytes))
        .context("reading /dev/urandom for the heartbeat api key")?;
    let key: String = bytes
        .iter()
        .map(|b| char::from(ALPHABET[usize::from(*b) % ALPHABET.len()]))
        .collect();
    storage::set_meta(conn, "wakapi_api_key", Some(&key))?;
    Ok(key)
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
fn spawn_capture(config: &Config, data_dir: &Path, tx: Sender<CaptureEvent>) -> anyhow::Result<()> {
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
    let gtx = tx.clone();
    std::thread::Builder::new()
        .name("afk".into())
        .spawn(move || afk_loop(afk, gtx, threshold_ms))?;

    spawn_git_capture(config, tx.clone())?;
    spawn_ai_sessions_capture(config, tx.clone())?;
    spawn_github_capture(config, tx.clone())?;
    spawn_shell_capture(config, tx.clone())?;
    spawn_mic_capture(config, tx.clone())?;
    spawn_gcal_capture(config, data_dir, tx)
}

/// Mic-in-use watcher via `pw-dump`: optional, never load-bearing.
#[cfg(target_os = "linux")]
fn spawn_mic_capture(config: &Config, tx: Sender<CaptureEvent>) -> anyhow::Result<()> {
    use chronicle_capture::FocusProvider;
    use chronicle_capture::mic::MicProvider;

    if !config.mic_capture {
        return Ok(());
    }
    let pw_dump = chronicle_core::config::resolve_command("pw-dump");
    if !pw_dump.contains('/') {
        tracing::warn!("mic_capture = true but `pw-dump` is not on PATH");
        return Ok(());
    }
    let provider = MicProvider::new(PathBuf::from(pw_dump));
    std::thread::Builder::new()
        .name("mic".into())
        .spawn(move || {
            if let Err(e) = provider.run(tx) {
                tracing::error!("mic provider exited: {e}");
            }
        })?;
    Ok(())
}

/// Git poller: optional, never load-bearing — a dead thread loses git
/// evidence, not capture.
fn spawn_git_capture(config: &Config, tx: Sender<CaptureEvent>) -> anyhow::Result<()> {
    use chronicle_capture::FocusProvider;
    use chronicle_capture::git::GitProvider;

    let repos: Vec<PathBuf> = config
        .git_repos
        .iter()
        .map(|p| chronicle_core::config::expand_home(p))
        .collect();
    let git = GitProvider::new(&repos);
    if git.is_empty() {
        if !repos.is_empty() {
            tracing::warn!("git_repos configured but none resolved to a git dir");
        }
        return Ok(());
    }
    std::thread::Builder::new()
        .name("git".into())
        .spawn(move || {
            if let Err(e) = git.run(tx) {
                tracing::error!("git provider exited: {e}");
            }
        })?;
    Ok(())
}

/// AI session watcher: optional, never load-bearing — same contract as git.
fn spawn_ai_sessions_capture(config: &Config, tx: Sender<CaptureEvent>) -> anyhow::Result<()> {
    use chronicle_capture::FocusProvider;
    use chronicle_capture::ai_sessions::AiSessionProvider;

    let dirs: Vec<PathBuf> = config
        .ai_session_dirs
        .iter()
        .map(|p| chronicle_core::config::expand_home(p))
        .collect();
    let watcher = AiSessionProvider::new(&dirs);
    if watcher.is_empty() {
        if !dirs.is_empty() {
            tracing::warn!("ai_session_dirs configured but none is a directory");
        }
        return Ok(());
    }
    std::thread::Builder::new()
        .name("ai-sessions".into())
        .spawn(move || {
            if let Err(e) = watcher.run(tx) {
                tracing::error!("ai session provider exited: {e}");
            }
        })?;
    Ok(())
}

/// PR poller via the user's `gh`: opt-in, never load-bearing.
fn spawn_github_capture(config: &Config, tx: Sender<CaptureEvent>) -> anyhow::Result<()> {
    use chronicle_capture::FocusProvider;
    use chronicle_capture::github::GitHubProvider;

    if !config.github_prs {
        return Ok(());
    }
    let gh = chronicle_core::config::resolve_command("gh");
    if !gh.contains('/') {
        tracing::warn!("github_prs = true but `gh` is not on PATH");
        return Ok(());
    }
    let provider = GitHubProvider::new(PathBuf::from(gh));
    std::thread::Builder::new()
        .name("github".into())
        .spawn(move || {
            if let Err(e) = provider.run(tx) {
                tracing::error!("github provider exited: {e}");
            }
        })?;
    Ok(())
}

/// atuin history poller: opt-in, never load-bearing.
fn spawn_shell_capture(config: &Config, tx: Sender<CaptureEvent>) -> anyhow::Result<()> {
    use chronicle_capture::FocusProvider;
    use chronicle_capture::shell::{ShellProvider, default_db_path};

    if !config.shell_history {
        return Ok(());
    }
    let db = default_db_path();
    if !db.is_file() {
        tracing::warn!("shell_history = true but {} is missing", db.display());
        return Ok(());
    }
    let repos: Vec<PathBuf> = config
        .git_repos
        .iter()
        .map(|p| chronicle_core::config::expand_home(p))
        .collect();
    let provider = ShellProvider::new(db, &repos);
    std::thread::Builder::new()
        .name("shell".into())
        .spawn(move || {
            if let Err(e) = provider.run(tx) {
                tracing::error!("shell provider exited: {e}");
            }
        })?;
    Ok(())
}

/// Google Calendar poller: opt-in and only once `chronicle gcal-login` has
/// written the token file; never load-bearing.
fn spawn_gcal_capture(
    config: &Config,
    data_dir: &Path,
    tx: Sender<CaptureEvent>,
) -> anyhow::Result<()> {
    use chronicle_capture::FocusProvider;
    use chronicle_capture::gcal::{GcalProvider, Tokens, token_path};

    if !config.google_calendar {
        return Ok(());
    }
    let path = token_path(data_dir);
    if !path.exists() {
        tracing::warn!(
            "google_calendar = true but no {}: run `chronicle gcal-login`",
            path.display()
        );
        return Ok(());
    }
    let tokens = match Tokens::load(&path) {
        Ok(t) => t,
        Err(e) => {
            tracing::warn!("google_calendar: {} unreadable: {e}", path.display());
            return Ok(());
        }
    };
    let provider = GcalProvider::new(tokens);
    std::thread::Builder::new()
        .name("gcal".into())
        .spawn(move || {
            if let Err(e) = provider.run(tx) {
                tracing::error!("google calendar provider exited: {e}");
            }
        })?;
    Ok(())
}

/// `chronicle gcal-login`: OAuth desktop flow. Opens the consent screen in
/// the browser, takes the code off an ephemeral loopback port, exchanges it
/// and writes `<data dir>/google.toml` (mode 0600).
fn gcal_login(
    data_dir: &Path,
    client_id: Option<String>,
    client_secret: Option<String>,
) -> anyhow::Result<()> {
    use chronicle_capture::gcal;

    let client_id = flag_or_env(client_id, "CHRONICLE_GOOGLE_CLIENT_ID")
        .context("no OAuth client id: pass --client-id or set CHRONICLE_GOOGLE_CLIENT_ID")?;
    let client_secret = flag_or_env(client_secret, "CHRONICLE_GOOGLE_CLIENT_SECRET").context(
        "no OAuth client secret: pass --client-secret or set CHRONICLE_GOOGLE_CLIENT_SECRET",
    )?;

    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let redirect_uri = format!("http://{}", listener.local_addr()?);
    let state = random_hex(16)?;
    let endpoints = gcal::Endpoints::default();
    let url = gcal::auth_url(&endpoints, &client_id, &redirect_uri, &state);
    let _ = Command::new("xdg-open")
        .arg(&url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    println!("waiting for Google on {redirect_uri}; if no browser opened, visit:\n{url}");

    let code = wait_for_oauth_code(&listener, &state)?;
    let grant = gcal::exchange_code(&endpoints, &client_id, &client_secret, &code, &redirect_uri)
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let refresh_token = grant.refresh_token.context(
        "Google returned no refresh token: remove Chronicle under \
         myaccount.google.com/permissions and sign in again",
    )?;
    let email = gcal::account_email(&endpoints, &grant.access_token).unwrap_or_default();
    std::fs::create_dir_all(data_dir)?;
    let path = gcal::token_path(data_dir);
    gcal::Tokens {
        client_id,
        client_secret,
        refresh_token,
        email: email.clone(),
    }
    .save(&path)
    .map_err(|e| anyhow::anyhow!("writing {}: {e}", path.display()))?;
    let who = if email.is_empty() {
        "the primary calendar".to_owned()
    } else {
        email
    };
    println!(
        "signed in as {who} \u{b7} token in {} \u{b7} turn Google Calendar on in \
         Settings \u{203a} Connections and restart the daemon",
        path.display()
    );
    Ok(())
}

fn flag_or_env(flag: Option<String>, var: &str) -> Option<String> {
    flag.or_else(|| std::env::var(var).ok())
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
}

/// `n` bytes from the OS CSPRNG, hex-encoded.
fn random_hex(n: usize) -> anyhow::Result<String> {
    use std::io::Read;

    let mut buf = vec![0u8; n];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

/// The browser's redirect carries the code; anything else on the port (a
/// favicon probe, a stray hit) is answered and ignored.
fn wait_for_oauth_code(listener: &std::net::TcpListener, state: &str) -> anyhow::Result<String> {
    use chronicle_capture::gcal::redirect_param;
    use std::io::{BufRead, BufReader, Read, Write};

    for stream in listener.incoming() {
        let mut stream = stream?;
        let mut line = String::new();
        // Bounded: the request line is all we read, and the peer is not
        // necessarily the browser we opened.
        BufReader::new(&stream)
            .take(8 * 1024)
            .read_line(&mut line)?;
        let code = redirect_param(&line, "code");
        let error = redirect_param(&line, "error");
        let body = match (&code, &error) {
            (Some(_), _) => "Chronicle is signed in. You can close this tab.",
            (_, Some(_)) => "Google refused the sign-in; check the terminal.",
            _ => "Waiting for Google.",
        };
        let _ = stream.write_all(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain; charset=utf-8\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        );
        let _ = stream.flush();
        if let Some(err) = error {
            bail!("Google returned {err}");
        }
        if let Some(code) = code {
            if redirect_param(&line, "state").as_deref() != Some(state) {
                bail!("OAuth state mismatch \u{2014} ignoring the redirect");
            }
            return Ok(code);
        }
    }
    bail!("the loopback listener closed before the code arrived")
}

#[cfg(not(target_os = "linux"))]
fn spawn_capture(
    _config: &Config,
    _data_dir: &Path,
    _tx: Sender<CaptureEvent>,
) -> anyhow::Result<()> {
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
            CaptureEvent::Activity(_) | CaptureEvent::Afk { .. } => return false,
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
    let path = config.mcp_path(data_dir);
    println!("mcp config: {}", path.display());
    let mcp = chronicle_mcp::McpConfig::load(&path)?;
    if mcp.servers.is_empty() {
        println!("no servers configured");
    }
    for server in &mcp.servers {
        if !server.enabled {
            println!("{}: disabled", server.name);
            continue;
        }
        match chronicle_mcp::probe_server(server) {
            Ok(p) => println!(
                "{}: ok \u{2014} {} {} \u{b7} {} tools \u{b7} {:.1}s",
                server.name,
                p.server_name,
                p.server_version,
                p.tools.len(),
                p.elapsed.as_secs_f32()
            ),
            Err(e) => println!("{}: FAILED \u{2014} {e:#}", server.name),
        }
    }
    match chronicle_mcp::gather_context(&path).1 {
        Some(ctx) => println!("\n## Workspace context\n{ctx}"),
        None => println!(
            "no context gathered (missing/empty config, or every call failed — see warnings above)"
        ),
    }
    Ok(())
}

pub(crate) enum Liveness {
    Stopped,
    Unresponsive,
    Running(DaemonStatus),
}

pub(crate) fn query_daemon(sock: &Path) -> Liveness {
    query_daemon_within(sock, Duration::from_secs(1))
}

/// `query_daemon` with a caller-chosen reply timeout (the UI thread polls
/// with a short one so an unresponsive daemon cannot stall a frame).
pub(crate) fn query_daemon_within(sock: &Path, timeout: Duration) -> Liveness {
    use std::io::{BufRead, Write};
    let Ok(mut stream) = std::os::unix::net::UnixStream::connect(sock) else {
        return Liveness::Stopped;
    };
    let _ = stream.set_read_timeout(Some(timeout));
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

/// What `chronicle status` reads from the DB and the model dir (the daemon's
/// own view arrives as `Liveness`).
#[derive(Default)]
struct DbStatus {
    last_event_age_secs: Option<u64>,
    last_batch_end_ms: Option<i64>,
    /// Model file name plus preset name; None when no model is downloaded.
    model_file: Option<String>,
    server_error: Option<String>,
    last_prune_age_secs: Option<u64>,
    last_derive: Option<chronicle_core::storage::DeriveMetrics>,
    pending_batches: i64,
}

fn format_status(liveness: &Liveness, db: &DbStatus) -> String {
    let DbStatus {
        last_event_age_secs,
        last_batch_end_ms,
        model_file,
        server_error,
        last_prune_age_secs,
        last_derive,
        pending_batches,
    } = db;
    let mut out = String::new();
    match liveness {
        Liveness::Running(s) => {
            out.push_str(&format!(
                "chronicle: healthy — daemon running (uptime {})\n",
                fmt_secs(s.uptime_secs)
            ));
            let worker = match (&s.worker, s.worker_secs) {
                (Some(w), Some(secs)) => format!("{w} ({})", fmt_secs(secs)),
                (Some(w), None) => w.clone(),
                (None, _) if s.derive_active => "running".into(),
                (None, _) if s.model_resident => "idle (model resident)".into(),
                (None, _) => "idle".into(),
            };
            out.push_str(&format!("  derive worker: {worker}\n"));
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
        Some(age) => out.push_str(&format!("  last event: {} ago\n", fmt_secs(*age))),
        None => out.push_str("  last event: none recorded yet\n"),
    }
    if let Some(end_ms) = last_batch_end_ms
        && let Ok(t) = local(*end_ms)
    {
        out.push_str(&format!(
            "  last derived batch ended: {}\n",
            t.strftime("%Y-%m-%d %H:%M")
        ));
    }
    match last_derive {
        Some(d) => {
            let when = local(d.derived_ts)
                .map(|t| t.strftime("%Y-%m-%d %H:%M").to_string())
                .unwrap_or_default();
            out.push_str(&format!(
                "  last derive: batch {} at {when}, {:.1}s, {} prompt + {} gen tokens\n",
                d.batch_id,
                d.derive_ms as f64 / 1000.0,
                d.prompt_tokens,
                d.gen_tokens
            ));
        }
        None => out.push_str("  last derive: none instrumented yet\n"),
    }
    out.push_str(&format!("  pending batches: {pending_batches}\n"));
    out.push_str(&format!(
        "  model: {}\n",
        model_file.as_deref().unwrap_or("not downloaded")
    ));
    if let Some(err) = server_error {
        out.push_str(&format!("  warning: {err}\n"));
    }
    if let Some(age) = last_prune_age_secs {
        out.push_str(&format!("  last prune: {} ago\n", fmt_secs(*age)));
    }
    out
}

fn status(data_dir: &Path, json: bool) -> anyhow::Result<()> {
    let liveness = query_daemon(&socket_path(data_dir));
    let conn = chronicle_core::storage::open(&data_dir.join("chronicle.db"))?;
    let config = Config::load(&data_dir.join("config.toml"))?;
    let now_ms = Timestamp::now().as_millisecond();
    let age = |ms: i64| ((now_ms - ms) / 1000).max(0) as u64;
    let model_file =
        chronicle_derive::model::resolve(config.model_path.as_deref(), data_dir).map(|p| {
            let file = p
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            match chronicle_derive::model::PRESETS
                .iter()
                .find(|m| m.file == file)
            {
                Some(m) => format!("{file} ({})", m.name),
                None => file,
            }
        });
    let db = DbStatus {
        last_event_age_secs: chronicle_core::storage::latest_event_ts(&conn)?.map(age),
        last_batch_end_ms: chronicle_core::storage::latest_batch_end(&conn)?,
        model_file,
        server_error: chronicle_core::storage::get_meta(&conn, "server_error")?,
        last_prune_age_secs: chronicle_core::storage::get_meta(&conn, "last_prune_ts")?
            .and_then(|v| v.parse::<i64>().ok())
            .map(age),
        last_derive: chronicle_core::storage::last_derive(&conn)?,
        pending_batches: chronicle_core::storage::pending_batch_count(&conn)?,
    };

    if json {
        let (liveness_str, daemon) = match &liveness {
            Liveness::Running(s) => ("healthy", Some(s)),
            Liveness::Stopped => ("stopped", None),
            Liveness::Unresponsive => ("unresponsive", None),
        };
        let doc = serde_json::json!({
            "liveness": liveness_str,
            "daemon": daemon,
            "last_event_age_secs": db.last_event_age_secs,
            "last_batch_end_ms": db.last_batch_end_ms,
            "model_present": db.model_file.is_some(),
            "model_file": db.model_file,
            "server_error": db.server_error,
            "last_prune_age_secs": db.last_prune_age_secs,
            "last_derive": db.last_derive,
            "pending_batches": db.pending_batches,
        });
        println!("{}", serde_json::to_string_pretty(&doc)?);
    } else {
        print!("{}", format_status(&liveness, &db));
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

fn standup_cmd(data_dir: &Path, day: Option<&str>) -> anyhow::Result<()> {
    let conn = chronicle_core::storage::open(&data_dir.join("chronicle.db"))?;
    let date: civil::Date = match day {
        Some(d) => d.parse().with_context(|| format!("bad --day {d:?}"))?,
        None => Zoned::now().date().checked_sub(1.day())?,
    };
    let key = date.to_string();
    match chronicle_core::storage::get_standup_draft(&conn, &key)? {
        Some((_, content)) => println!("{content}"),
        None => println!(
            "no standup draft yet for {key} — it drafts in the daemon's next idle window once the day has journal entries"
        ),
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
                worker: Some("derive batch 71".into()),
                worker_secs: Some(45),
                model_resident: true,
                idle_secs: Some(90),
                ui_open: true,
            }),
            &DbStatus {
                last_event_age_secs: Some(12),
                model_file: Some("Qwen3-4B-Q4_K_M.gguf (qwen3-4b)".into()),
                last_prune_age_secs: Some(3600),
                last_derive: Some(chronicle_core::storage::DeriveMetrics {
                    batch_id: 70,
                    derived_ts: 0,
                    derive_ms: 61_500,
                    prompt_tokens: 2100,
                    gen_tokens: 140,
                }),
                pending_batches: 2,
                ..DbStatus::default()
            },
        );
        assert!(s.contains("healthy"));
        assert!(s.contains("uptime 2h 13m"));
        assert!(s.contains("derive worker: derive batch 71 (45s)"));
        assert!(s.contains("user idle: 1m"));
        assert!(s.contains("last event: 12s ago"));
        assert!(s.contains("last derive: batch 70 at "), "{s}");
        assert!(s.contains(", 61.5s, 2100 prompt + 140 gen tokens"), "{s}");
        assert!(s.contains("pending batches: 2"));
        assert!(s.contains("model: Qwen3-4B-Q4_K_M.gguf (qwen3-4b)"));
        assert!(s.contains("last prune: 1h 0m ago"));
    }

    #[test]
    fn format_status_pre_m27_daemon_reports_running_worker() {
        // An older daemon's status JSON lacks the worker fields.
        let s: DaemonStatus = serde_json::from_str(
            r#"{"uptime_secs":5,"derive_active":true,"idle_secs":null,"ui_open":false}"#,
        )
        .unwrap();
        let s = format_status(&Liveness::Running(s), &DbStatus::default());
        assert!(s.contains("derive worker: running"));
        assert!(s.contains("last derive: none instrumented yet"));
    }

    #[test]
    fn deriveproto_round_trips() {
        use deriveproto::{Reply, Request};
        let req = serde_json::to_string(&Request::Derive { batch_id: 71 }).unwrap();
        assert_eq!(req, r#"{"t":"derive","batch_id":71}"#);
        assert_eq!(
            serde_json::from_str::<Request>(&req).unwrap(),
            Request::Derive { batch_id: 71 }
        );
        let live = serde_json::to_string(&Request::Live { lo: 1, hi: 2 }).unwrap();
        assert_eq!(live, r#"{"t":"live","lo":1,"hi":2}"#);
        let day = serde_json::to_string(&Request::Consolidate { lo: 1, hi: 2 }).unwrap();
        assert_eq!(day, r#"{"t":"consolidate","lo":1,"hi":2}"#);
        for reply in [
            Reply::Ready,
            Reply::Progress {
                label: "fixing checkout\u{2026}".into(),
            },
            Reply::Done {
                batch_id: Some(71),
                intervals: 3,
            },
            Reply::Done {
                batch_id: None,
                intervals: 1,
            },
            Reply::Err {
                batch_id: None,
                message: "boom".into(),
            },
        ] {
            let line = serde_json::to_string(&reply).unwrap();
            assert_eq!(
                serde_json::from_str::<Reply>(&line).unwrap(),
                reply,
                "{line}"
            );
        }
        // A pre-m27 daemon's status JSON lacks model_resident.
        let s: DaemonStatus = serde_json::from_str(
            r#"{"uptime_secs":5,"derive_active":false,"idle_secs":null,"ui_open":false}"#,
        )
        .unwrap();
        assert!(!s.model_resident);
        let s = format_status(&Liveness::Running(s), &DbStatus::default());
        assert!(s.contains("derive worker: idle\n"), "{s}");
    }

    #[test]
    fn progress_label_follows_the_partial_json() {
        use chronicle_core::types::OpenTask;
        let open = vec![OpenTask {
            id: 5,
            label: "fixing checkout crash".into(),
            project: None,
            declared: true,
        }];
        assert_eq!(progress_label("{\"intervals\": [{\"re", &open), None);
        assert_eq!(
            progress_label("{\"intervals\": [{\"ref\": 1, \"label\": null", &open).as_deref(),
            Some("linking to fixing checkout crash")
        );
        assert_eq!(
            progress_label("{\"intervals\": [{\"ref\": 3, ", &open),
            None
        );
        assert_eq!(
            progress_label("{\"ref\": null, \"label\": \"redesign", &open).as_deref(),
            Some("redesign\u{2026}")
        );
        assert_eq!(
            progress_label(
                "{\"ref\": null, \"label\": \"redesigning blog\", \"proj",
                &open
            )
            .as_deref(),
            Some("redesigning blog")
        );
        // Second interval supersedes the first.
        let two = "{\"intervals\": [{\"ref\": null, \"label\": \"blog\", \"start\": 0, \"end\": 5, \"confidence\": 0.9}, {\"ref\": 1, \"label\": nu";
        assert_eq!(
            progress_label(two, &open).as_deref(),
            Some("linking to fixing checkout crash")
        );
        assert_eq!(progress_label("{\"ref\": null, \"label\": \"", &open), None);
    }

    #[test]
    fn resident_action_rules() {
        let t0 = Instant::now();
        let timeout = Duration::from_secs(300);
        let idle = Duration::from_secs(1260);
        let at = |secs: u64| t0 + Duration::from_secs(secs);
        assert_eq!(
            resident_action(Some(at(10)), t0, at(100), timeout, idle),
            ResidentAction::Keep
        );
        assert_eq!(
            resident_action(Some(at(10)), t0, at(310), timeout, idle),
            ResidentAction::KillTimeout
        );
        assert_eq!(
            resident_action(None, at(10), at(1000), timeout, idle),
            ResidentAction::Keep
        );
        assert_eq!(
            resident_action(None, at(10), at(1270), timeout, idle),
            ResidentAction::KillIdle
        );
        // Busy since well past idle but under timeout: busy wins.
        assert_eq!(
            resident_action(Some(at(2000)), t0, at(2100), timeout, idle),
            ResidentAction::Keep
        );
    }

    #[test]
    fn format_status_stopped_still_reports_db_state() {
        let s = format_status(&Liveness::Stopped, &DbStatus::default());
        assert!(s.contains("stopped"));
        assert!(!s.contains("healthy"));
        assert!(s.contains("last event: none recorded yet"));
        assert!(s.contains("model: not downloaded"));
    }

    #[test]
    fn format_status_unresponsive_and_server_error() {
        let s = format_status(
            &Liveness::Unresponsive,
            &DbStatus {
                last_event_age_secs: Some(400),
                model_file: Some("m.gguf".into()),
                server_error: Some("AW endpoint failed on port 5600: in use".into()),
                ..DbStatus::default()
            },
        );
        assert!(s.contains("running but not responding"));
        assert!(s.contains("warning: AW endpoint failed on port 5600: in use"));
    }

    // The standup prompt's drift rule reads a "## Plan" section; the digest
    // opens with it when the day had an intent.
    #[test]
    fn standup_digest_opens_with_the_days_plan() {
        use chronicle_core::storage::{JournalEntry, StandupDigestRow};
        let rows = [StandupDigestRow {
            task_id: 1,
            label: "m26 chunk 5".into(),
            project: Some("chronicle".into()),
            external_ref: None,
            entries: vec![JournalEntry {
                id: 1,
                batch_id: 1,
                start_ts: 0,
                end_ts: 60_000,
                entry: "wired the picker".into(),
            }],
            checkpoint: None,
        }];
        let plain = standup_digest_text(&rows, &TimeZone::UTC, None);
        assert!(!plain.contains("## Plan"), "{plain}");
        let plan = "- task: m26 chunk 5 [chronicle]\n";
        let with = standup_digest_text(&rows, &TimeZone::UTC, Some(plan));
        assert_eq!(with, format!("## Plan\n{plan}{plain}"));
        // "Skip today" stores an empty intent: the prompt is unchanged.
        assert_eq!(
            standup_digest_text(&rows, &TimeZone::UTC, Some("  \n")),
            plain
        );
    }
}
