//! The first five minutes (m41 chunk 2): which connectors already work on
//! this machine, which are one step away, and which need an account or an
//! install first. The Setup view and `chronicle setup` render this one plan,
//! so the window and a headless terminal say the same thing.

use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::config::{self, Config};
use crate::connectors::{self, Connector, ProbeKind, SetupStep, Support};
use crate::health::{self, Env, Health};
use crate::project::{self, DiscoveredRepo};

/// Where a connector lands in the setup view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// Rows are landing, or the source is wired and waiting for the first
    /// one: nothing to do.
    Working,
    /// Every remaining step is a switch, a config field or a command on this
    /// machine.
    OneStep,
    /// An install or a sign-in flow stands in front of the switches: the
    /// descriptor says so, or the tool's binary is not on this machine.
    Account,
}

impl Group {
    pub const ORDER: [Group; 3] = [Group::Working, Group::OneStep, Group::Account];

    pub fn label(self) -> &'static str {
        match self {
            Group::Working => "already working",
            Group::OneStep => "one step each",
            Group::Account => "needs an install or an account",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Item {
    pub connector: &'static Connector,
    pub health: Health,
    pub group: Group,
}

/// The group for one connector, or `None` when setup has nothing to offer:
/// a planned or declined tool, or a supported one with no steps that is
/// simply not on the machine — Settings › Connections still lists those.
///
/// `Connected` counts as working only when no step is still pending: a
/// switch is never pending here (the config probe already covers it), an
/// install is done once a probe found the tool, and a command, field or
/// account step is always pending until rows land. A connector whose only
/// probes are endpoints is blind — the daemon answering says nothing about
/// the hook line or the extension — so its install stays pending too.
pub fn group_of(c: &Connector, h: &Health) -> Option<Group> {
    if !matches!(c.state, Support::Supported | Support::Partial { .. }) {
        return None;
    }
    let blind = !c.probes.is_empty()
        && c.probes
            .iter()
            .all(|p| matches!(p.kind, ProbeKind::Endpoint { .. }));
    let install_ok = *h != Health::Absent && !blind;
    let pending = c.setup.iter().any(|s| match s {
        SetupStep::Toggle { .. } => false,
        SetupStep::Install { .. } => !install_ok,
        _ => true,
    });
    if matches!(h, Health::Working { .. }) || (*h == Health::Connected && !pending) {
        return Some(Group::Working);
    }
    if c.setup.is_empty() {
        return None;
    }
    let binary_missing = *h == Health::Absent
        && c.probes
            .iter()
            .any(|p| matches!(p.kind, ProbeKind::Command { .. }));
    let gated = binary_missing
        || c.setup.iter().any(|s| match s {
            SetupStep::Install { .. } => !install_ok,
            SetupStep::Account { .. } => true,
            _ => false,
        });
    Some(if gated {
        Group::Account
    } else {
        Group::OneStep
    })
}

/// Every connector setup can place, in registry order, with this machine's
/// health. `env.platform` decides which descriptors are in scope, so a
/// `fake` env plans another platform from here.
pub fn plan(conn: Option<&Connection>, cfg: &Config, env: &Env, now_ms: i64) -> Vec<Item> {
    connectors::for_platform(env.platform)
        .filter_map(|c| {
            let h = health::health_of(conn, cfg, env, c, now_ms);
            group_of(c, &h).map(|group| Item {
                connector: c,
                health: h,
                group,
            })
        })
        .collect()
}

/// The login shell's name from `$SHELL`, for the hook line; zsh when unset
/// or unknown, since that is what the descriptor's command names.
pub fn shell() -> &'static str {
    let shell = std::env::var("SHELL").unwrap_or_default();
    match Path::new(&shell).file_name().and_then(|n| n.to_str()) {
        Some("bash") => "bash",
        Some("fish") => "fish",
        Some("pwsh") => "pwsh",
        _ => "zsh",
    }
}

/// The line a person pastes, from a descriptor's `Command`: the shell hook
/// becomes the `eval` line for their shell; anything else runs as written.
pub fn paste_line(run: &str, shell: &str) -> String {
    if run.starts_with("chronicle shell-init") {
        return match shell {
            "fish" => "chronicle shell-init fish | source".to_owned(),
            "pwsh" => "chronicle shell-init pwsh | Invoke-Expression".to_owned(),
            _ => format!("eval \"$(chronicle shell-init {shell})\""),
        };
    }
    run.to_owned()
}

/// The rc file the hook line belongs in.
pub fn rc_file(shell: &str) -> &'static str {
    match shell {
        "bash" => "~/.bashrc",
        "fish" => "~/.config/fish/config.fish",
        "pwsh" => "$PROFILE",
        _ => "~/.zshrc",
    }
}

/// One step as a sentence, for the terminal and for hover text.
pub fn describe(step: &SetupStep, shell: &str) -> String {
    match step {
        SetupStep::Toggle { field } => format!("{field} = true in config.toml"),
        SetupStep::Command { run } => {
            let line = paste_line(run, shell);
            if run.starts_with("chronicle shell-init") {
                format!("{line}  in {}", rc_file(shell))
            } else {
                line
            }
        }
        SetupStep::Field { field, hint } => format!("{field} in config.toml: {hint}"),
        SetupStep::Install { url } => format!("install it: {url}"),
        SetupStep::Account { flow } => format!("chronicle {flow}"),
    }
}

/// Folders a developer's repos tend to sit under, relative to home.
pub const REPO_PARENTS: &[&str] = &["dev", "src", "code", "projects", "work", "repos", "git"];

/// Git repos under the usual parent folders that `git_repos` does not
/// already name — the scan behind "find my repos" on a fresh profile,
/// where discovery has no watched repo to start from.
pub fn scan_repos(home: &Path, git_repos: &[String]) -> Vec<DiscoveredRepo> {
    let parents: Vec<PathBuf> = REPO_PARENTS
        .iter()
        .map(|p| home.join(p))
        .filter(|p| p.is_dir())
        .collect();
    let known: Vec<PathBuf> = git_repos.iter().map(|r| config::expand_home(r)).collect();
    project::discover_repos(&parents, &known)
}
