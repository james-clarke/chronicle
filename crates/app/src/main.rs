mod ai_job;
mod bench;
mod capture;
mod chat_worker;
mod daemon;
mod derive;
mod rotate;
mod status;
mod ui;

use ai_job::ai_job_worker;
use bench::{
    backfill_coalesce, backfill_descriptions, backfill_evidence, bench, evidence_report,
    replay_eval,
};
use capture::gcal_login;
use chat_worker::chat_worker;
use daemon::{run, send_ctrl, socket_path};
use derive::{derive_resident, derive_worker};
use status::{dump, mcp_check, report, standup_cmd, status};

// Re-exports so `crate::X` paths in `src/ui/` (and this file's own tests)
// stay valid regardless of which module actually defines `X`.
#[cfg(test)]
pub(crate) use ai_job::standup_digest_text;
pub(crate) use chat_worker::chatproto;
#[cfg(test)]
pub(crate) use daemon::{CtrlCmd, ResidentAction, parse_ctrl_cmd, resident_action};
pub(crate) use daemon::{
    DaemonStatus, bump_day_counter, day_counter_key, own_exe, spawn_chat_worker,
};
pub(crate) use derive::tail_digest;
#[cfg(test)]
pub(crate) use derive::{deriveproto, progress_label};
#[cfg(test)]
pub(crate) use status::{DbStatus, format_status};
pub(crate) use status::{Liveness, query_daemon_within};

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use chronicle_core::config::Config;
use clap::{Parser, Subcommand, ValueEnum};

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
    /// Embed every focus span that has no vector yet with the configured
    /// `embed_model` (m30 chunk 6), then rebuild the task centroids.
    BackfillEmbeddings,
    /// Internal: re-read AI session transcripts modified since a local day
    /// and replace their rows (m32 chunk 2: prompt minutes and titles for
    /// rows captured before the collector kept them). Run
    /// `backfill-anchors` after it.
    #[command(hide = true)]
    BackfillSessions {
        /// Transcripts modified on or after this local day; default: everything.
        #[arg(long, value_name = "YYYY-MM-DD")]
        since: Option<String>,
    },
    /// Internal: read every `git_repos` entry's `.remember/today-*.md` once
    /// and upsert their `note` rows (m32 chunk 5); the daemon does the same
    /// on start.
    #[command(hide = true)]
    BackfillNotes,
    /// Internal: recompute span anchors (m30) for spans since a local day.
    #[command(hide = true)]
    BackfillAnchors {
        /// Spans starting on or after this local day; default: everything.
        #[arg(long, value_name = "YYYY-MM-DD")]
        since: Option<String>,
    },
    /// How much focus time carries an anchor (work item, document, place…),
    /// and which anchors cover the most time.
    Anchors {
        /// Window in days ending now.
        #[arg(long, default_value_t = 7)]
        days: u32,
        /// Values to list.
        #[arg(long, default_value_t = 20)]
        top: usize,
    },
    /// Internal: one-off rebuild of `task_evidence` from stored intervals,
    /// anchored spans and corrections.
    #[command(hide = true)]
    BackfillEvidence,
    /// Show a task's evidence rows, or a summary of the strongest evidence
    /// across all tasks.
    Evidence {
        /// Show this task's evidence rows instead of the cross-task summary.
        #[arg(long)]
        task: Option<i64>,
        /// Entries to list per task in the summary.
        #[arg(long, default_value_t = 5)]
        top: usize,
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
        /// Replay through a cloud backend named in models.toml instead of a
        /// downloaded model (m31 chunk 0 gate).
        #[arg(long)]
        backend: Option<String>,
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
        /// Score with the m30 evidence profiler instead of (or alongside) a
        /// model: no llama load. Fixture mode: persona fixtures with
        /// `.expect.json` groups. Replay mode: scores probes against the
        /// profiler and, if a model also runs, a combined verdict.
        #[arg(long)]
        scorer: bool,
        /// Replay: which corrections become probes — `all` (merge probes
        /// span the target's intervals; the model's gate), `direct`
        /// (assign/reassign/eject only; the scorer's gate) or `source`
        /// (merge probes span the folded task's own intervals).
        #[arg(long, default_value = "all")]
        probes: String,
        /// With --scorer: cut each window with the m30 segmenter first and
        /// score its segments (fixture mode walks the persona fixtures cold;
        /// replay mode takes the minute-weighted majority over a probe).
        #[arg(long)]
        segment: bool,
        /// Read the segmenter's verdict log (m30 chunk 4): corrected share
        /// per margin bucket, and the delta at which "to confirm" covers the
        /// worst tenth of placements. Uses --since.
        #[arg(long)]
        calibrate: bool,
        /// Re-sessionize the last --since days with and without the m32
        /// quiet rule and print the daytime AFK gap histogram (chunk 1 gate).
        #[arg(long)]
        gaps: bool,
        /// Place one window the way the segmenter's reconcile would, without
        /// writing it, and print each row with its share plus the window's
        /// split by project (m32 chunk 3 gate). Local times:
        /// `2026-09-03T15:00..2026-09-03T16:44`.
        #[arg(long)]
        window: Option<String>,
        /// Time an embedding GGUF over recent titles (m30 chunk 6 gate:
        /// p95 under 20 ms per title on this CPU).
        #[arg(long)]
        embed: Option<PathBuf>,
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
            scorer,
            probes,
            segment,
            calibrate,
            gaps,
            window,
            embed,
            backend,
        } => {
            if let Some(path) = embed {
                bench::embed_bench(&data_dir, &path)
            } else if let Some(spec) = window {
                bench::window(&data_dir, &spec)
            } else if calibrate {
                bench::calibrate(&data_dir, since)
            } else if gaps {
                bench::gaps(&data_dir, since)
            } else if replay {
                let set = chronicle_core::replay::ProbeSet::parse(&probes)
                    .ok_or_else(|| anyhow::anyhow!("--probes must be all, direct or source"))?;
                replay_eval(
                    &data_dir,
                    since,
                    model.as_deref(),
                    backend.as_deref(),
                    scorer,
                    segment,
                    set,
                    out.as_deref(),
                )
            } else {
                bench(
                    &data_dir,
                    &fixtures,
                    &batch,
                    digest,
                    only.as_deref(),
                    model.as_deref(),
                    no_mcp,
                    scorer,
                    segment,
                )
            }
        }
        Cmd::ChatWorker { conversation, task } => chat_worker(&data_dir, conversation, task),
        Cmd::AiJob { id } => ai_job_worker(&data_dir, id),
        Cmd::BackfillDescriptions { limit } => backfill_descriptions(&data_dir, limit),
        Cmd::BackfillCoalesce { since, dry_run } => backfill_coalesce(&data_dir, &since, dry_run),
        Cmd::BackfillSessions { since } => bench::backfill_sessions(&data_dir, since.as_deref()),
        Cmd::BackfillNotes => bench::backfill_notes(&data_dir),
        Cmd::BackfillAnchors { since } => bench::backfill_anchors(&data_dir, since.as_deref()),
        Cmd::BackfillEmbeddings => bench::backfill_embeddings(&data_dir),
        Cmd::Anchors { days, top } => bench::anchor_report(&data_dir, days, top),
        Cmd::BackfillEvidence => backfill_evidence(&data_dir),
        Cmd::Evidence { task, top } => evidence_report(&data_dir, task, top),
        Cmd::GcalLogin {
            client_id,
            client_secret,
        } => gcal_login(&data_dir, client_id, client_secret),
    }
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
                        .chain(model::EMBED_PRESETS.iter())
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
            for spec in model::PRESETS.iter().chain(model::EMBED_PRESETS.iter()) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    use jiff::tz::TimeZone;

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
        let truth = std::collections::HashMap::from([(
            1,
            vec![chronicle_core::types::ActivityEvent {
                ts: chronicle_core::types::ms_to_ts(14 * 3_600_000),
                end_ts: None,
                repo: "chronicle".into(),
                branch: "main".into(),
                kind: chronicle_core::types::ActivityKind::Commit,
                ext_id: Some("abc1234def".into()),
                summary: Some("feat: picker".into()),
                detail: None,
            }],
        )]);
        let plain = standup_digest_text(&rows, &TimeZone::UTC, None, &truth);
        assert!(!plain.contains("## Plan"), "{plain}");
        // Every line a claim can build on ends in its source tag (m32
        // chunk 5).
        assert!(
            plain.contains("wired the picker [journal 00:00]"),
            "{plain}"
        );
        assert!(
            plain.contains("Ground truth:\n- 14:00 commit chronicle@main"),
            "{plain}"
        );
        let plan = "- task: m26 chunk 5 [chronicle]\n";
        let with = standup_digest_text(&rows, &TimeZone::UTC, Some(plan), &truth);
        assert_eq!(with, format!("## Plan\n{plan}{plain}"));
        // "Skip today" stores an empty intent: the prompt is unchanged.
        assert_eq!(
            standup_digest_text(&rows, &TimeZone::UTC, Some("  \n"), &truth),
            plain
        );
    }
}
