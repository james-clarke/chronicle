//! Source setup commands (m37): the shell hook script, the git hooks and
//! the hook runner the git hooks call.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use chronicle_capture::hooks;
use chronicle_core::config::Config;
use chronicle_core::connectors::{self, ConnectKind, Platform, Support};
use chronicle_core::health;
use chronicle_core::setup::{self, Group};

/// `chronicle connections [--json]` (m41 chunks 0 and 1): the connector
/// registry, grouped the way Settings groups it, with what each row is
/// doing on this machine. `--json` prints `docs/connectors.json` verbatim —
/// the registry as the site's tools page reads it, which is deliberately
/// machine-independent, so health is text only.
pub(crate) fn connections(data_dir: &Path, json: bool, html: bool) -> anyhow::Result<()> {
    if json {
        print!("{}", connectors::to_json());
        return Ok(());
    }
    if html {
        print!("{}", connectors::to_site_html());
        return Ok(());
    }
    let config = Config::load(&data_dir.join("config.toml"))?;
    let daemon_up = matches!(
        crate::status::query_daemon(&crate::daemon::socket_path(data_dir)),
        crate::status::Liveness::Running(_)
    );
    let env = health::Env::host(daemon_up);
    let conn = chronicle_core::storage::open(&data_dir.join("chronicle.db")).ok();
    let now_ms = jiff::Timestamp::now().as_millisecond();
    for kind in ConnectKind::ORDER {
        let rows: Vec<_> = connectors::for_platform(Platform::HOST)
            .filter(|c| c.kind == kind)
            .collect();
        if rows.is_empty() {
            continue;
        }
        println!("{}", kind.label());
        for c in rows {
            let state = health::health_of(conn.as_ref(), &config, &env, c, now_ms);
            println!(
                "    {:<11} {:<38} {}",
                health::label(c, &state),
                c.name,
                note(c, &state, now_ms)
            );
        }
    }
    Ok(())
}

/// `chronicle setup` (m41 chunk 2): the Setup view's three groups, printed.
/// Nothing is written — the switches are `config.toml` fields and the
/// commands are the person's to run — so this is the flow for a machine
/// with no window, and a way to see it again from any terminal.
pub(crate) fn setup(data_dir: &Path) -> anyhow::Result<()> {
    let config = Config::load(&data_dir.join("config.toml"))?;
    let daemon_up = matches!(
        crate::status::query_daemon(&crate::daemon::socket_path(data_dir)),
        crate::status::Liveness::Running(_)
    );
    let env = health::Env::host(daemon_up);
    let conn = chronicle_core::storage::open(&data_dir.join("chronicle.db")).ok();
    let now_ms = jiff::Timestamp::now().as_millisecond();
    let plan = setup::plan(conn.as_ref(), &config, &env, now_ms);
    let shell = setup::shell();
    for group in Group::ORDER {
        let items: Vec<_> = plan.iter().filter(|i| i.group == group).collect();
        if items.is_empty() {
            continue;
        }
        println!("{}", group.label());
        for item in items {
            let c = item.connector;
            println!(
                "    {:<11} {:<38} {}",
                health::label(c, &item.health),
                c.name,
                note(c, &item.health, now_ms)
            );
            if group != Group::Working {
                for step in c.setup {
                    println!("    {:<11} {:<38} {}", "", "", setup::describe(step, shell));
                }
            }
        }
    }
    if !daemon_up {
        println!("the daemon is not running: `chronicle run`, or `chronicle service install`");
    }
    println!(
        "config.toml is read when the daemon starts; Settings › Connections flips the same switches"
    );
    Ok(())
}

/// The last column: the numbers when there are numbers, the reason when
/// something is broken, and otherwise the line that says what the tool
/// would give you.
fn note(c: &connectors::Connector, state: &health::Health, now_ms: i64) -> String {
    if let Some(detail) = crate::status::health_detail(health::unit(c), state, now_ms) {
        return detail;
    }
    match c.state {
        Support::Partial { note } => format!("{} \u{b7} {note}", c.blurb),
        Support::WontDo { reason } => reason.to_owned(),
        _ => c.blurb.to_owned(),
    }
}

/// `chronicle shell-init <shell>`: print the precmd hook for the shell to
/// `eval` from its rc file. Posts cwd, program name and duration to the
/// local endpoint; never the command line.
pub(crate) fn shell_init(data_dir: &Path, shell: &str) -> anyhow::Result<()> {
    let config = Config::load(&data_dir.join("config.toml"))?;
    match chronicle_capture::shell_hook::shell_init(shell, config.port) {
        Some(script) => {
            print!("{script}");
            Ok(())
        }
        None => bail!("unknown shell {shell:?}: zsh, bash, fish or pwsh"),
    }
}

/// The repos `hooks install|remove|status` act on: the one given, else every
/// watched repo and its worktrees.
fn hook_repos(data_dir: &Path, repo: Option<&str>) -> anyhow::Result<Vec<PathBuf>> {
    if let Some(r) = repo {
        let p = chronicle_core::config::expand_home(r);
        if chronicle_capture::git::resolve_git_dir(&p).is_none() {
            bail!("{} is not a git repo", p.display());
        }
        return Ok(vec![p]);
    }
    let config = Config::load(&data_dir.join("config.toml"))?;
    let mut out: Vec<PathBuf> = Vec::new();
    for repo in config.watched_repos() {
        for wt in chronicle_core::project::worktrees_of(&repo) {
            if !out.contains(&wt) {
                out.push(wt);
            }
        }
        if !out.contains(&repo) {
            out.push(repo);
        }
    }
    out.retain(|p| chronicle_capture::git::resolve_git_dir(p).is_some());
    Ok(out)
}

pub(crate) fn hooks_install(data_dir: &Path, repo: Option<&str>) -> anyhow::Result<()> {
    let exe = std::env::current_exe().context("current exe")?;
    for repo in hook_repos(data_dir, repo)? {
        match hooks::install(&repo, &exe) {
            Ok(files) => {
                println!("{}: {} hooks", repo.display(), files.len());
                for f in files {
                    println!("    {}", f.display());
                }
            }
            Err(e) => println!("{}: failed: {e}", repo.display()),
        }
    }
    Ok(())
}

pub(crate) fn hooks_remove(data_dir: &Path, repo: Option<&str>) -> anyhow::Result<()> {
    for repo in hook_repos(data_dir, repo)? {
        match hooks::remove(&repo) {
            Ok(files) => println!("{}: {} hooks cleaned", repo.display(), files.len()),
            Err(e) => println!("{}: failed: {e}", repo.display()),
        }
    }
    Ok(())
}

pub(crate) fn hooks_status(data_dir: &Path, repo: Option<&str>) -> anyhow::Result<()> {
    for repo in hook_repos(data_dir, repo)? {
        let st = hooks::status(&repo);
        println!(
            "{}: {}",
            repo.display(),
            if st.installed.is_empty() {
                "no chronicle hooks".to_owned()
            } else {
                st.installed.join(", ")
            }
        );
    }
    Ok(())
}

/// `chronicle hooks backfill [--days N]`: checkouts from each repo's reflog
/// the poller never saw.
pub(crate) fn hooks_backfill(data_dir: &Path, repo: Option<&str>, days: u32) -> anyhow::Result<()> {
    let conn = chronicle_core::storage::open(&data_dir.join("chronicle.db"))?;
    let now_ms = jiff::Timestamp::now().as_millisecond();
    let since_ms = now_ms - i64::from(days) * 86_400_000;
    for repo in hook_repos(data_dir, repo)? {
        let events = chronicle_capture::reflog::reflog_checkouts(&repo, since_ms, now_ms);
        let n = events.len();
        for e in &events {
            chronicle_core::storage::insert_activity_event(&conn, e)?;
        }
        println!("{}: {n} reflog checkouts", repo.display());
    }
    Ok(())
}

/// `chronicle hook <name> [args]`, run by the installed git hook inside the
/// repo: collect what happened and post it to the daemon. Silent and quick
/// (one second) so a hook never slows a commit; a stopped daemon is fine.
pub(crate) fn hook(data_dir: &Path, name: &str, args: &[String]) -> anyhow::Result<()> {
    let cwd = std::env::current_dir()?;
    let Some(post) = hooks::collect(name, &cwd, args) else {
        return Ok(());
    };
    let port = Config::load(&data_dir.join("config.toml"))
        .map(|c| c.port)
        .unwrap_or(5600);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(1)))
        .build()
        .into();
    let body = serde_json::to_string(&post)?;
    let _ = agent
        .post(&format!("http://127.0.0.1:{port}/api/chronicle/git"))
        .header("Content-Type", "application/json")
        .send(body.as_bytes());
    Ok(())
}
