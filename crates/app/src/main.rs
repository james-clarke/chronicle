mod ai_job;
mod bench;
mod capture;
mod chat_worker;
mod daemon;
mod derive;
mod project;
mod replay;
mod rotate;
mod service;
mod sources;
mod status;
#[cfg(target_os = "macos")]
mod tray_macos;
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
    /// Internal: resident derivation worker — JSON requests on
    /// stdin, replies on stdout; exits after `worker_idle_secs` idle.
    #[command(hide = true)]
    DeriveWorker,
    /// Internal: one-shot derivation of a single batch.
    #[command(hide = true)]
    Derive {
        #[arg(long)]
        batch: i64,
    },
    /// Internal: load fixture event streams into the data dir the way the
    /// daemon would have captured them, then print the batch
    /// ids they closed, one per line, for `chronicle derive --batch` to
    /// pick up.
    #[command(hide = true)]
    Replay {
        /// `<fixture.jsonl>=<YYYY-MM-DD>`: the stream, and the local day its
        /// first event lands on, keeping every event's wall-clock time.
        /// Repeatable.
        #[arg(long = "case", value_name = "FIXTURE=YYYY-MM-DD", required = true)]
        cases: Vec<String>,
        /// Place each case's batches through the segmenter tier the way the
        /// daemon's tick does, instead of leaving them pending for
        /// `chronicle derive --batch`.
        #[arg(long)]
        reconcile: bool,
    },
    /// Internal: warm chat inference worker.
    #[command(hide = true)]
    ChatWorker {
        /// Conversation whose history seeds the model context.
        #[arg(long)]
        conversation: i64,
        /// Scope retrieval to this task's workspace.
        #[arg(long)]
        task: Option<i64>,
    },
    /// Internal: ephemeral AI-job worker (descriptions, suggestions, narratives).
    #[command(hide = true)]
    AiJob {
        #[arg(long)]
        id: i64,
    },
    /// Internal: one-off coalesce of stored derived intervals (adjacent
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
    /// Embed task labels and corrections for the example memory, then, with
    /// `embed_model` set, every focus span that has no vector yet and the
    /// task centroids.
    BackfillEmbeddings,
    /// Internal: re-read AI session transcripts modified since a local day
    /// and replace their rows (prompt minutes and titles for
    /// rows captured before the collector kept them). Run
    /// `backfill-anchors` after it.
    #[command(hide = true)]
    BackfillSessions {
        /// Transcripts modified on or after this local day; default: everything.
        #[arg(long, value_name = "YYYY-MM-DD")]
        since: Option<String>,
    },
    /// Internal: read every `git_repos` entry's `.remember/today-*.md` once
    /// and upsert their `note` rows; the daemon does the same
    /// on start.
    #[command(hide = true)]
    BackfillNotes,
    /// Internal: recompute the last seven days' self-score rows now (the
    /// daemon does it once a day) and print them.
    #[command(hide = true)]
    SelfScore,
    /// Internal: recompute span anchors for spans since a local day.
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
    /// Declare, list and edit tasks.
    Task {
        #[command(subcommand)]
        cmd: TaskCmd,
    },
    /// Projects: the rules that file time into a project.
    Project {
        #[command(subcommand)]
        cmd: ProjectCmd,
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
        /// downloaded model.
        #[arg(long)]
        backend: Option<String>,
        /// Skip MCP context for --batch cases (offline; replay never gathers it).
        #[arg(long)]
        no_mcp: bool,
        /// Re-derive every batch a correction touched and score the corrected
        /// outcome.
        #[arg(long)]
        replay: bool,
        /// Replay: only corrections made in the last N days.
        #[arg(long, default_value_t = 7)]
        since: u64,
        /// Replay: write the per-probe results as JSON here for diffing.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Score with the evidence profiler instead of (or alongside) a
        /// model: no llama load. Fixture mode: persona fixtures with
        /// `.expect.json` groups. Replay mode: scores probes against the
        /// profiler and, if a model also runs, a combined verdict.
        #[arg(long)]
        scorer: bool,
        /// Replay with `--scorer`: ask the pairwise advisor on
        /// every unsure verdict — through `--backend`, else the local
        /// model — and score with and without its answer.
        #[arg(long, requires = "scorer")]
        advisor: bool,
        /// Replay: which corrections become probes — `all` (merge probes
        /// span the target's intervals; the model's gate), `direct`
        /// (assign/reassign/eject only; the scorer's gate) or `source`
        /// (merge probes span the folded task's own intervals).
        #[arg(long, default_value = "all")]
        probes: String,
        /// With --scorer: cut each window with the segmenter first and
        /// score its segments (fixture mode walks the persona fixtures cold;
        /// replay mode takes the minute-weighted majority over a probe).
        #[arg(long)]
        segment: bool,
        /// Read the segmenter's verdict log: corrected share
        /// per margin bucket, and the delta at which "to confirm" covers the
        /// worst tenth of placements. Uses --since.
        #[arg(long)]
        calibrate: bool,
        /// Re-sessionize the last --since days with and without the quiet
        /// rule and print the daytime AFK gap histogram.
        #[arg(long)]
        gaps: bool,
        /// Place one window the way the segmenter's reconcile would, without
        /// writing it, and print each row with its share plus the window's
        /// split by project. Local times:
        /// `2026-09-03T15:00..2026-09-03T16:44`.
        #[arg(long)]
        window: Option<String>,
        /// With `--window`: score against the profiles as they stand now
        /// (what the daemon's tick uses), not as of the window's start.
        #[arg(long, requires = "window")]
        live: bool,
        /// Time an embedding GGUF over recent titles (the target is p95
        /// under 20 ms per title on this CPU).
        #[arg(long)]
        embed: Option<PathBuf>,
        /// Print the past corrections a naming prompt would see for this
        /// "app title" text: the cosine path when vectors
        /// exist, the FTS path beside it.
        #[arg(long)]
        examples: Option<String>,
        /// Pairwise naming judge: name recent derived tasks
        /// with and without past-correction examples on the local model
        /// and let `--backend` (else the local model) pick the better
        /// label; prints the win rate and every pair.
        #[arg(long)]
        judge: bool,
        /// Same-day re-run drift: yesterday's standup and a
        /// few naming prompts twice through `--backend` (else the local
        /// model); prints the word-overlap similarity per pair.
        #[arg(long)]
        drift: bool,
    },
    /// Every tool Chronicle can read, what it does with it, and what it
    /// would take to connect it here. `--json` is the same table the
    /// site's tools page reads.
    Connections {
        #[arg(long)]
        json: bool,
        /// The site's tools page body (`site/tools.html`).
        #[arg(long, conflicts_with = "json")]
        html: bool,
    },
    /// The first five minutes: what already works on this machine, what is
    /// one step away, and what needs an account first — the same list the
    /// Setup view shows, printed for a headless install.
    Setup,
    /// Print the shell hook for zsh, bash, fish or pwsh: add
    /// `eval "$(chronicle shell-init zsh)"` to your rc file. It posts each
    /// command's cwd, program name and duration to the local endpoint —
    /// never the command line.
    ShellInit { shell: String },
    /// Chronicle's git hooks: exact-second checkouts and commits,
    /// appended after any existing hook, opt-in per repo.
    Hooks {
        #[command(subcommand)]
        cmd: HooksCmd,
    },
    /// Internal: run by an installed git hook.
    #[command(hide = true)]
    Hook {
        name: String,
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
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
    /// Run Chronicle at login: a systemd user unit on Linux, a
    /// LaunchAgent on macOS. The onboarding card calls the same code.
    Service {
        #[command(subcommand)]
        cmd: ServiceCmd,
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

#[derive(Subcommand)]
enum HooksCmd {
    /// Append the chronicle line to post-checkout, post-commit and
    /// post-rewrite (every configured repo, or one).
    Install { repo: Option<String> },
    /// Strip the chronicle line again.
    Remove { repo: Option<String> },
    /// Which repos have the hooks.
    Status { repo: Option<String> },
    /// Checkouts from each repo's reflog the 20 s poll never saw.
    Backfill {
        repo: Option<String>,
        #[arg(long, default_value_t = 30)]
        days: u32,
    },
}

#[derive(Subcommand)]
enum ServiceCmd {
    /// Write the unit/plist and enable it (no `--now`: the running instance
    /// already holds the single-instance socket).
    Install,
    /// Disable/unload it and delete the unit/plist.
    Remove,
    /// Whether it is enabled/loaded and running.
    Status,
}

#[derive(Subcommand)]
enum ProjectCmd {
    /// Projects in force: name, remote, instance paths, rules.
    List,
    /// Match the last N days against the current config (the stored column
    /// is not read): minutes per project, unfiled minutes, the top unfiled
    /// places and titles to tune the rules by.
    Test {
        #[arg(long, default_value_t = 7)]
        days: u32,
        #[arg(long, default_value_t = 15)]
        top: usize,
    },
    /// Re-file stored spans after a config edit (all history unless --days).
    Rebuild {
        #[arg(long)]
        days: Option<u32>,
    },
}

#[derive(Subcommand)]
enum TaskCmd {
    /// Open tasks by project: id, project, label. `--all` lists closed
    /// ones too; `--project` keeps one project (its configured name or a
    /// repo folder).
    List {
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Declare a task, the way Home's declare row does: a work-item key in
    /// the label becomes the task's anchor, and time that matches it starts
    /// landing there.
    Add {
        /// What you are working on. A ticket key or URL in it becomes the
        /// anchor, and the label when it is all there is.
        label: String,
        /// A configured project, or a repo folder that names one; anything
        /// else is kept as typed and counts as unfiled until a rule matches.
        #[arg(long)]
        project: Option<String>,
        #[arg(long)]
        description: Option<String>,
    },
    /// Close a task (the UI's close): it stops taking time; reopen from
    /// the task manager or `task list --all` if it was a slip.
    Close {
        /// Task id, from `task list` or the UI.
        id: i64,
    },
    /// Re-score a day's placements with the tasks as they stand now, the
    /// way a keep or move does: batches sealed before a task was declared
    /// or closed take it up. Rows you placed yourself are untouched, and
    /// the re-score is one undoable correction.
    Rescore {
        /// Local day, `YYYY-MM-DD`; today when omitted.
        #[arg(long)]
        day: Option<String>,
    },
    /// Change a task's label, project or description. Label and project
    /// edits are kept as a correction the model reads first next time.
    Rename {
        /// Task id, from `task list` or the UI.
        id: i64,
        #[arg(long)]
        label: Option<String>,
        /// An empty string clears the project.
        #[arg(long)]
        project: Option<String>,
        /// An empty string clears the description.
        #[arg(long)]
        description: Option<String>,
    },
    /// Make an open declared task its project's sink: time in the
    /// project goes to it over any newer declared task.
    Current {
        /// Task id, from `task list` or the UI.
        id: i64,
    },
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
        Cmd::Replay { cases, reconcile } => replay::replay(&data_dir, &cases, reconcile),
        Cmd::DeriveWorker => derive_resident(&data_dir),
        Cmd::McpCheck => mcp_check(&data_dir),
        Cmd::Model { cmd } => model_cmd(&data_dir, cmd),
        Cmd::Task { cmd } => task_cmd(&data_dir, cmd),
        Cmd::Connections { json, html } => sources::connections(&data_dir, json, html),
        Cmd::Setup => sources::setup(&data_dir),
        Cmd::ShellInit { shell } => sources::shell_init(&data_dir, &shell),
        Cmd::Hooks { cmd } => match cmd {
            HooksCmd::Install { repo } => sources::hooks_install(&data_dir, repo.as_deref()),
            HooksCmd::Remove { repo } => sources::hooks_remove(&data_dir, repo.as_deref()),
            HooksCmd::Status { repo } => sources::hooks_status(&data_dir, repo.as_deref()),
            HooksCmd::Backfill { repo, days } => {
                sources::hooks_backfill(&data_dir, repo.as_deref(), days)
            }
        },
        Cmd::Hook { name, args } => sources::hook(&data_dir, &name, &args),
        Cmd::Project { cmd } => match cmd {
            ProjectCmd::List => project::list(&data_dir),
            ProjectCmd::Test { days, top } => project::test(&data_dir, days, top),
            ProjectCmd::Rebuild { days } => project::rebuild(&data_dir, days),
        },
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
            advisor,
            probes,
            segment,
            calibrate,
            gaps,
            window,
            live,
            embed,
            examples,
            judge,
            drift,
            backend,
        } => {
            if judge {
                bench::judge(&data_dir, backend.as_deref())
            } else if drift {
                bench::drift(&data_dir, backend.as_deref())
            } else if let Some(text) = examples {
                bench::examples(&data_dir, &text)
            } else if let Some(path) = embed {
                bench::embed_bench(&data_dir, &path)
            } else if let Some(spec) = window {
                bench::window(&data_dir, &spec, live)
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
                    advisor,
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
        Cmd::SelfScore => bench::self_score(&data_dir),
        Cmd::BackfillAnchors { since } => bench::backfill_anchors(&data_dir, since.as_deref()),
        Cmd::BackfillEmbeddings => bench::backfill_embeddings(&data_dir),
        Cmd::Anchors { days, top } => bench::anchor_report(&data_dir, days, top),
        Cmd::BackfillEvidence => backfill_evidence(&data_dir),
        Cmd::Evidence { task, top } => evidence_report(&data_dir, task, top),
        Cmd::GcalLogin {
            client_id,
            client_secret,
        } => gcal_login(&data_dir, client_id, client_secret),
        Cmd::Service { cmd } => {
            let result = match cmd {
                ServiceCmd::Install => service::install(),
                ServiceCmd::Remove => service::remove(),
                ServiceCmd::Status => service::status(),
            };
            match result {
                Ok(msg) => {
                    println!("{msg}");
                    Ok(())
                }
                Err(e) => bail!("{e}"),
            }
        }
    }
}

fn task_cmd(data_dir: &Path, cmd: TaskCmd) -> anyhow::Result<()> {
    use chronicle_core::storage;
    let mut conn = storage::open(&data_dir.join("chronicle.db"))?;
    match cmd {
        TaskCmd::List { project, all } => {
            let config = chronicle_core::config::Config::load(&data_dir.join("config.toml"))?;
            let matcher = chronicle_core::project::Matcher::from_config(&config);
            let want = project.as_deref().map(|p| {
                matcher
                    .resolve(p)
                    .map(str::to_owned)
                    .unwrap_or_else(|| p.to_owned())
            });
            let mut rows: Vec<storage::ManagedTask> = storage::all_tasks(&conn)?
                .into_iter()
                .filter(|t| all || t.open)
                .filter(|t| {
                    want.as_deref()
                        .is_none_or(|w| t.project.as_deref() == Some(w))
                })
                .collect();
            // Project-major: configured order, then the names no rule
            // knows, then no project; newest activity first inside each.
            let rank = |p: Option<&str>| -> (usize, String) {
                match p {
                    Some(p) => (
                        matcher
                            .projects
                            .iter()
                            .position(|q| q.name == p)
                            .unwrap_or(matcher.projects.len()),
                        p.to_owned(),
                    ),
                    None => (usize::MAX, String::new()),
                }
            };
            rows.sort_by_key(|t| rank(t.project.as_deref()));
            let mut last: Option<Option<String>> = None;
            for t in &rows {
                if last.as_ref() != Some(&t.project) {
                    println!("{}", t.project.as_deref().unwrap_or("(no project)"));
                    last = Some(t.project.clone());
                }
                let mut marks: Vec<&str> = Vec::new();
                if t.declared {
                    marks.push("declared");
                }
                if t.current {
                    marks.push("current");
                }
                if !t.open {
                    marks.push("closed");
                }
                println!(
                    "  {:>5}  {}{}",
                    t.id,
                    t.label,
                    if marks.is_empty() {
                        String::new()
                    } else {
                        format!("  ({})", marks.join(", "))
                    }
                );
            }
            Ok(())
        }
        TaskCmd::Add {
            label,
            project,
            description,
        } => {
            let config = Config::load(&data_dir.join("config.toml"))?;
            let now = jiff::Timestamp::now();
            let input = label.trim().to_owned();
            if input.is_empty() {
                bail!("the label cannot be empty");
            }
            let ticket = regex::Regex::new(&config.ticket_regex)
                .ok()
                .and_then(|re| re.find(&input).map(|m| m.as_str().to_owned()));
            let label = match &ticket {
                Some(key) if input == *key || input.starts_with("http") => key.clone(),
                _ => input,
            };
            // Home's rule (m35 chunk 1): a typed project resolves to a
            // configured one when it names one or its repo folder, and is
            // otherwise kept as typed.
            let typed = project.unwrap_or_default().trim().to_owned();
            let matcher = chronicle_core::project::Matcher::from_config(&config);
            let project = matcher.resolve(&typed).map(str::to_owned).unwrap_or(typed);
            let project = (!project.is_empty()).then_some(project.as_str());
            let id = storage::insert_user_task(&conn, now, &label, project)?;
            if let Some(d) = description
                .as_deref()
                .map(str::trim)
                .filter(|d| !d.is_empty())
            {
                storage::set_task_description(&conn, id, Some(d))?;
            }
            if let Some(key) = &ticket {
                storage::set_task_external_ref(&conn, id, key)?;
            }
            chronicle_core::segmenter::seed_task_evidence(&mut conn, &config, now, id)?;
            println!("task {id}: {label} [{}]", project.unwrap_or("-"));
            Ok(())
        }
        TaskCmd::Close { id } => {
            let Some((label, _, _)) = storage::task_identity(&conn, id)? else {
                bail!("no task {id}");
            };
            storage::close_task(&conn, jiff::Timestamp::now(), id)?;
            println!("task {id}: {label} closed");
            Ok(())
        }
        TaskCmd::Rescore { day } => {
            let config = chronicle_core::config::Config::load(&data_dir.join("config.toml"))?;
            if config.derive_mode != "segmenter" {
                bail!("task rescore needs derive_mode = \"segmenter\"");
            }
            let tz = jiff::tz::TimeZone::system();
            let now = jiff::Timestamp::now();
            let day = match day {
                Some(d) => d.parse::<jiff::civil::Date>().context("--day")?,
                None => now.to_zoned(tz.clone()).date(),
            };
            let lo = day.to_zoned(tz)?.timestamp().as_millisecond();
            let hi = lo + 86_400_000;
            let distractions =
                chronicle_core::evidence::compile_patterns(&config.distraction_patterns);
            let moved = chronicle_core::segmenter::rescore_day(
                &mut conn,
                &config,
                now,
                &distractions,
                &day.to_string(),
                lo,
                hi,
            )?;
            match moved {
                Some((id, n)) => println!("{day}: {n} rows moved (correction {id})"),
                None => println!("{day}: nothing moved"),
            }
            Ok(())
        }
        TaskCmd::Rename {
            id,
            label,
            project,
            description,
        } => {
            if label.is_none() && project.is_none() && description.is_none() {
                bail!("nothing to change: pass --label, --project or --description");
            }
            let Some((old_label, old_project, _)) = storage::task_identity(&conn, id)? else {
                bail!("no task {id}");
            };
            let new_label = label.as_deref().map(str::trim).unwrap_or(&old_label);
            if new_label.is_empty() {
                bail!("the label cannot be empty");
            }
            // A task lives inside a configured project or is unfiled (m35
            // chunk 1); a repo folder resolves to its project.
            let matcher = chronicle_core::project::Matcher::from_config(&Config::load(
                &data_dir.join("config.toml"),
            )?);
            let new_project = match project.as_deref().map(str::trim) {
                Some("") => None,
                Some(p) => Some(matcher.resolve(p).ok_or_else(|| {
                    anyhow::anyhow!(
                        "no project {p:?}; configured: {}",
                        matcher
                            .projects
                            .iter()
                            .map(|p| p.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    )
                })?),
                None => old_project.as_deref(),
            };
            if new_label != old_label || new_project != old_project.as_deref() {
                storage::insert_correction(
                    &mut conn,
                    jiff::Timestamp::now(),
                    id,
                    new_label,
                    new_project,
                )?;
                println!(
                    "task {id}: {old_label} [{}] \u{2192} {new_label} [{}]",
                    old_project.as_deref().unwrap_or("-"),
                    new_project.unwrap_or("-")
                );
            }
            if let Some(d) = description.as_deref().map(str::trim) {
                storage::set_task_description(&conn, id, (!d.is_empty()).then_some(d))?;
                println!(
                    "task {id}: description {}",
                    if d.is_empty() { "cleared" } else { "set" }
                );
            }
            Ok(())
        }
        TaskCmd::Current { id } => {
            if !storage::set_current_task(&mut conn, id)? {
                bail!("no open declared task {id}");
            }
            let (label, project, _) = storage::task_identity(&conn, id)?.unwrap_or_default();
            println!(
                "task {id}: {label} is current in {}",
                project.as_deref().unwrap_or("(no project)")
            );
            Ok(())
        }
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
                focus_route: Some("wlr".into()),
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
        assert!(s.contains("focus: wlr"), "{s}");
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
                claims: None,
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
