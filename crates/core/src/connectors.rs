//! The connector registry (m41 chunk 0): one descriptor per integration,
//! as a `const` table. It is the single list behind the Settings panel,
//! `chronicle connections`, and the site's tools page, so adding a tool is
//! one descriptor plus a collector — and a tool with no collector yet still
//! has a row, which is what makes `Planned` visible to a user.
//!
//! Descriptors are static and machine-independent on purpose: what a
//! connector *is* belongs here, what it is *doing on this machine* is read
//! at runtime from the config (`enabled`) and, from m41 chunk 1, from the
//! probes and the event tables.

use serde::{Serialize, Serializer, ser::SerializeSeq};

use crate::config::Config;
use crate::types::ActivityKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    Linux,
    MacOs,
    Windows,
}

/// Every platform Chronicle builds for. `MacOs` covers m38, `Windows` is
/// still m40, so a `Windows` entry here says "the descriptor is ready",
/// never "the collector runs".
pub const ALL: &[Platform] = &[Platform::Linux, Platform::MacOs, Platform::Windows];
pub const UNIX: &[Platform] = &[Platform::Linux, Platform::MacOs];
pub const LINUX: &[Platform] = &[Platform::Linux];

impl Platform {
    /// The platform this binary was built for.
    pub const HOST: Platform = if cfg!(target_os = "macos") {
        Platform::MacOs
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else {
        Platform::Linux
    };

    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Linux => "linux",
            Platform::MacOs => "macos",
            Platform::Windows => "windows",
        }
    }
}

/// How far Chronicle has got with a tool. `Planned` is first-class: the
/// panel and the site show it, and it is what a user pushes on.
/// `Detected` is never written in the table — it is the state chunk 3 gives
/// a tool it found in the person's own activity and cannot read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Support {
    Supported,
    Partial { note: &'static str },
    Planned,
    Detected,
    WontDo { reason: &'static str },
}

/// The Settings › Connections taxonomy, ranked by tools unlocked per unit
/// of user effort (`dev-tools-direction.md:400-431`). It is the panel's
/// grouping and the site page's grouping, which is why the registry has one
/// category field rather than a separate connection kind: they were the
/// same taxonomy written twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConnectKind {
    Files,
    LocalServer,
    Cli,
    Account,
    Mcp,
    Token,
}

impl ConnectKind {
    /// The panel subhead, in the order the panel draws them.
    pub const ORDER: [ConnectKind; 6] = [
        ConnectKind::Files,
        ConnectKind::LocalServer,
        ConnectKind::Cli,
        ConnectKind::Account,
        ConnectKind::Mcp,
        ConnectKind::Token,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ConnectKind::Files => "Files on this machine",
            ConnectKind::LocalServer => "Local servers and sockets",
            ConnectKind::Cli => "Your CLIs",
            ConnectKind::Account => "Accounts",
            ConnectKind::Mcp => "MCP servers",
            ConnectKind::Token => "Tokens",
        }
    }
}

/// What to look at to decide whether a tool is on this machine. Evaluating
/// these is m41 chunk 1; the descriptor only says what to look at, so the
/// Linux, macOS and Windows rows are the same code.
///
/// `Path` values expand `~` (home), `{config}` (`~/.config`,
/// `~/Library/Application Support`, `%APPDATA%`) and `{data}`
/// (`~/.local/share`, `~/Library/Application Support`, `%LOCALAPPDATA%`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "probe", rename_all = "snake_case")]
pub enum ProbeKind {
    /// A bare command resolved on PATH, plus the user bin dirs the daemon
    /// sees.
    Command { cmd: &'static str },
    /// A file or directory that exists.
    Path { path: &'static str },
    /// A route the local endpoint serves for this source.
    Endpoint { route: &'static str },
    /// A config field that is set (non-empty, or true).
    Config { field: &'static str },
}

/// One probe, scoped to the platforms it makes sense on. An empty
/// `platforms` means every platform the connector claims.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Probe {
    #[serde(flatten)]
    pub kind: ProbeKind,
    pub platforms: &'static [Platform],
}

impl Probe {
    pub const fn cmd(cmd: &'static str) -> Probe {
        Probe {
            kind: ProbeKind::Command { cmd },
            platforms: &[],
        }
    }
    pub const fn path(path: &'static str) -> Probe {
        Probe {
            kind: ProbeKind::Path { path },
            platforms: &[],
        }
    }
    pub const fn path_on(path: &'static str, platforms: &'static [Platform]) -> Probe {
        Probe {
            kind: ProbeKind::Path { path },
            platforms,
        }
    }
    pub const fn cmd_on(cmd: &'static str, platforms: &'static [Platform]) -> Probe {
        Probe {
            kind: ProbeKind::Command { cmd },
            platforms,
        }
    }
    pub const fn endpoint(route: &'static str) -> Probe {
        Probe {
            kind: ProbeKind::Endpoint { route },
            platforms: &[],
        }
    }
    pub const fn config(field: &'static str) -> Probe {
        Probe {
            kind: ProbeKind::Config { field },
            platforms: &[],
        }
    }
}

/// What a person has to do to connect a tool, in the order they do it. The
/// setup view (chunk 2) renders one widget per step; the CLI prints them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum SetupStep {
    /// Flip a `config.toml` field on.
    Toggle { field: &'static str },
    /// Run this, with a copy button next to it.
    Command { run: &'static str },
    /// Fill a config field in.
    Field {
        field: &'static str,
        hint: &'static str,
    },
    /// Install the tool itself.
    Install { url: &'static str },
    /// Sign in through a flow Chronicle runs.
    Account { flow: &'static str },
}

/// One integration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Connector {
    /// Stable id: the JSON key, the request label, the docs anchor.
    pub id: &'static str,
    pub name: &'static str,
    pub kind: ConnectKind,
    /// Platforms the descriptor covers, not platforms it runs on today —
    /// `state` says that.
    pub platforms: &'static [Platform],
    #[serde(flatten)]
    pub state: Support,
    /// One line, the panel caption and the site's copy.
    pub blurb: &'static str,
    pub probes: &'static [Probe],
    /// The activity kinds this connector writes; empty when it feeds
    /// discovery or anchors rather than rows of its own.
    #[serde(serialize_with = "kinds_as_str")]
    pub produces: &'static [ActivityKind],
    pub setup: &'static [SetupStep],
    /// Anchor in the README and on the site's tools page.
    pub docs: &'static str,
}

fn kinds_as_str<S: Serializer>(kinds: &&'static [ActivityKind], s: S) -> Result<S::Ok, S::Error> {
    let mut seq = s.serialize_seq(Some(kinds.len()))?;
    for k in *kinds {
        seq.serialize_element(k.as_str())?;
    }
    seq.end()
}

/// Every connector Chronicle knows about, supported or not. Ordered by
/// `ConnectKind::ORDER`, then by how much of a developer's day the tool
/// tends to account for.
pub const REGISTRY: &[Connector] = &[
    // ---- Files on this machine -------------------------------------------
    Connector {
        id: "git_repos",
        name: "Git repos",
        kind: ConnectKind::Files,
        platforms: ALL,
        state: Support::Supported,
        blurb: "branch switches and commits, polled every 60 s per watched repo",
        probes: &[Probe::config("git_repos"), Probe::cmd("git")],
        produces: &[ActivityKind::Checkout, ActivityKind::Commit],
        setup: &[SetupStep::Field {
            field: "git_repos",
            hint: "repo paths, ~ expanded; repo discovery fills this in for you",
        }],
        docs: "git",
    },
    Connector {
        id: "git_hooks",
        name: "Git hooks",
        kind: ConnectKind::Files,
        platforms: ALL,
        state: Support::Supported,
        blurb: "post-checkout, post-commit and post-rewrite record the exact time a 60 s poll misses",
        probes: &[Probe::cmd("git")],
        produces: &[ActivityKind::Checkout, ActivityKind::Commit],
        setup: &[SetupStep::Command {
            run: "chronicle hooks install",
        }],
        docs: "git-hooks",
    },
    Connector {
        id: "claude_code_sessions",
        name: "Claude Code sessions",
        kind: ConnectKind::Files,
        platforms: ALL,
        state: Support::Supported,
        blurb: "each transcript's working directory, first prompt and touched paths — never the transcript",
        probes: &[
            Probe::config("ai_session_dirs"),
            Probe::path("~/.claude/projects"),
        ],
        produces: &[ActivityKind::AiSession],
        setup: &[SetupStep::Toggle {
            field: "ai_session_dirs",
        }],
        docs: "ai-sessions",
    },
    Connector {
        id: "agent_sessions",
        name: "Other agents' sessions",
        kind: ConnectKind::Files,
        platforms: ALL,
        state: Support::Supported,
        blurb: "codex, gemini, copilot, aider, cline, opencode and cursor transcripts, read the same way, on whenever one is there",
        // One probe per format's source dir, as the format reads it
        // (`crates/capture/src/sessions/`). opencode and cursor are written
        // as Linux paths there, so the probe says Linux too rather than
        // claiming a macOS row the collector would not find.
        probes: &[
            Probe::path("~/.codex/sessions"),
            Probe::path("~/.gemini/tmp"),
            Probe::path("~/.copilot/session-state"),
            Probe::path_on("~/.local/share/opencode", LINUX),
            Probe::path_on("~/.config/Cursor/User/globalStorage", LINUX),
        ],
        produces: &[ActivityKind::AiSession],
        setup: &[],
        docs: "ai-sessions",
    },
    Connector {
        id: "editor_workspaces",
        name: "Editor workspaces",
        kind: ConnectKind::Files,
        platforms: ALL,
        state: Support::Supported,
        blurb: "recently-opened folders from VS Code, JetBrains and Zed on disk, so a window title resolves to a repo",
        // The VS Code family roots the collector scans
        // (`crates/capture/src/workspaces.rs:29`), plus JetBrains and Zed.
        probes: &[
            Probe::path("{config}/Code/User/workspaceStorage"),
            Probe::path("{config}/Cursor/User/workspaceStorage"),
            Probe::path("{config}/VSCodium/User/workspaceStorage"),
            Probe::path("{config}/Windsurf/User/workspaceStorage"),
            Probe::path("{config}/JetBrains"),
            Probe::path("{config}/Zed/db"),
        ],
        produces: &[],
        setup: &[],
        docs: "editor-workspaces",
    },
    Connector {
        id: "browser_history",
        name: "Browser history",
        kind: ConnectKind::Files,
        platforms: ALL,
        state: Support::Supported,
        blurb: "the history database, copied and read-only, query strings dropped — the zero-install browser route",
        probes: &[
            Probe::path_on("~/.config/google-chrome", LINUX),
            Probe::path_on("~/.mozilla/firefox", LINUX),
            Probe::path_on(
                "~/Library/Application Support/Google/Chrome",
                &[Platform::MacOs],
            ),
            Probe::path_on("~/Library/Safari/History.db", &[Platform::MacOs]),
            Probe::path_on(
                "~/Library/Application Support/Firefox/Profiles",
                &[Platform::MacOs],
            ),
            Probe::path_on("{data}/Google/Chrome/User Data", &[Platform::Windows]),
        ],
        produces: &[ActivityKind::Browse],
        setup: &[SetupStep::Toggle {
            field: "browser_history",
        }],
        docs: "browser-history",
    },
    Connector {
        id: "repo_discovery",
        name: "Repo discovery",
        kind: ConnectKind::Files,
        platforms: ALL,
        state: Support::Supported,
        blurb: "git repos next to the ones you watch, filed as discovered projects instead of unfiled time",
        probes: &[Probe::config("discover_repos")],
        produces: &[],
        setup: &[SetupStep::Toggle {
            field: "discover_repos",
        }],
        docs: "discovery",
    },
    Connector {
        id: "link_files",
        name: "Link files",
        kind: ConnectKind::Files,
        platforms: ALL,
        state: Support::Supported,
        blurb: ".vercel, fly.toml, render.yaml, .sentryclirc and friends name the service a repo deploys to",
        probes: &[],
        produces: &[],
        setup: &[],
        docs: "link-files",
    },
    Connector {
        id: "repo_notes",
        name: "Repo notes",
        kind: ConnectKind::Files,
        platforms: ALL,
        state: Support::Supported,
        blurb: "each repo's .remember/today-*.md entries: what you or your tools wrote down, and when",
        probes: &[],
        produces: &[ActivityKind::Note],
        setup: &[],
        docs: "notes",
    },
    Connector {
        id: "shell_history_atuin",
        name: "Shell history (atuin)",
        kind: ConnectKind::Files,
        platforms: ALL,
        state: Support::Supported,
        blurb: "atuin's history.db every 60 s: working directory, program name and duration, never the command line",
        // Not `{data}`: the collector reads this exact path on every
        // platform (`crates/capture/src/shell.rs:32`).
        probes: &[Probe::path("~/.local/share/atuin/history.db")],
        produces: &[ActivityKind::Shell],
        setup: &[SetupStep::Toggle {
            field: "shell_history",
        }],
        docs: "shell",
    },
    Connector {
        id: "calendars_ics",
        name: "Calendars (ICS)",
        kind: ConnectKind::Files,
        platforms: ALL,
        state: Support::Supported,
        blurb: "a subscription URL from any calendar, polled every 15 min, \u{b1}7 days — no OAuth",
        probes: &[Probe::config("calendars")],
        produces: &[ActivityKind::Meeting],
        setup: &[SetupStep::Field {
            field: "calendars",
            hint: "your calendar's secret iCal address, or a path to an .ics file",
        }],
        docs: "calendars",
    },
    Connector {
        id: "native_shell_history",
        name: "Native shell history (zsh, bash, fish)",
        kind: ConnectKind::Files,
        platforms: ALL,
        state: Support::WontDo {
            reason: "the file is command lines, which carry secrets; the shell hook reports cwd and program name instead",
        },
        blurb: "read the shell hook's row instead: same coverage, none of the command text",
        probes: &[],
        produces: &[],
        setup: &[],
        docs: "shell",
    },
    // ---- Local servers and sockets ---------------------------------------
    Connector {
        id: "editor_heartbeats",
        name: "Editor heartbeats (WakaTime plugins)",
        kind: ConnectKind::LocalServer,
        platforms: ALL,
        state: Support::Supported,
        blurb: "vim-wakatime and its siblings post to this machine once ~/.wakatime.cfg points api_url here; folded into edit spans per project",
        probes: &[Probe::endpoint("/api/heartbeat")],
        produces: &[ActivityKind::Edit],
        setup: &[
            SetupStep::Toggle {
                field: "editor_heartbeats",
            },
            SetupStep::Install {
                url: "https://wakatime.com/plugins",
            },
        ],
        docs: "heartbeats",
    },
    Connector {
        id: "shell_hook",
        name: "Shell hook",
        kind: ConnectKind::LocalServer,
        platforms: ALL,
        state: Support::Supported,
        blurb: "a precmd hook posts working directory, program name and duration per command — never the command line",
        probes: &[Probe::endpoint("/api/chronicle/shell")],
        produces: &[ActivityKind::Shell],
        setup: &[
            SetupStep::Toggle {
                field: "shell_hook",
            },
            SetupStep::Command {
                run: "chronicle shell-init zsh",
            },
        ],
        docs: "shell",
    },
    Connector {
        id: "browser_extension",
        name: "Browser extension (ActivityWatch)",
        kind: ConnectKind::LocalServer,
        platforms: ALL,
        state: Support::Supported,
        blurb: "aw-watcher-web posts the URL and title of the focused tab: the precise route, where history is the easy one",
        probes: &[Probe::endpoint("/api/0/buckets")],
        produces: &[ActivityKind::Browse],
        setup: &[SetupStep::Install {
            url: "https://activitywatch.net/downloads/",
        }],
        docs: "browser-extension",
    },
    Connector {
        id: "tmux",
        name: "tmux panes",
        kind: ConnectKind::LocalServer,
        platforms: UNIX,
        state: Support::Supported,
        blurb: "the attached panes' working directories every 60 s",
        probes: &[Probe::cmd("tmux")],
        produces: &[ActivityKind::Cwd],
        setup: &[SetupStep::Install {
            url: "https://github.com/tmux/tmux",
        }],
        docs: "terminal",
    },
    Connector {
        id: "docker_compose",
        name: "Docker Compose stacks",
        kind: ConnectKind::LocalServer,
        platforms: ALL,
        state: Support::Supported,
        blurb: "each running stack's working_dir label every 60 s, which is the repo it belongs to",
        probes: &[Probe::cmd("docker")],
        produces: &[ActivityKind::Cwd],
        setup: &[SetupStep::Install {
            url: "https://docs.docker.com/get-started/",
        }],
        docs: "docker",
    },
    Connector {
        id: "listening_ports",
        name: "Listening ports",
        kind: ConnectKind::LocalServer,
        platforms: ALL,
        state: Support::Partial {
            note: "Linux reads /proc; macOS needs lsof and Windows GetExtendedTcpTable",
        },
        blurb: "the repo behind every local dev server, so a localhost tab lands on the right project",
        probes: &[
            Probe::path_on("/proc/net/tcp", LINUX),
            Probe::cmd_on("lsof", &[Platform::MacOs]),
            Probe::cmd_on("netstat", &[Platform::Windows]),
        ],
        produces: &[ActivityKind::Cwd],
        setup: &[],
        docs: "ports",
    },
    Connector {
        id: "mic_capture",
        name: "Calls (microphone in use)",
        kind: ConnectKind::LocalServer,
        platforms: ALL,
        state: Support::Partial {
            note: "PipeWire on Linux; macOS CoreAudio and the Windows capability registry are not written",
        },
        blurb: "the only cross-app call signal: each stretch of microphone use becomes a call",
        probes: &[Probe::cmd_on("pw-dump", LINUX)],
        produces: &[ActivityKind::Call],
        setup: &[SetupStep::Toggle {
            field: "mic_capture",
        }],
        docs: "calls",
    },
    Connector {
        id: "ngrok",
        name: "ngrok tunnels",
        kind: ConnectKind::LocalServer,
        platforms: ALL,
        state: Support::Planned,
        blurb: "the local API says which service is exposed, and so which repo the tunnel belongs to",
        probes: &[Probe::cmd("ngrok")],
        produces: &[],
        setup: &[],
        docs: "ngrok",
    },
    // ---- Your CLIs -------------------------------------------------------
    Connector {
        id: "github_prs",
        name: "GitHub pull requests (gh)",
        kind: ConnectKind::Cli,
        platforms: ALL,
        state: Support::Supported,
        blurb: "gh search prs, authored and reviewed, every 5 min \u{b7} your existing login, read only",
        probes: &[Probe::cmd("gh")],
        produces: &[ActivityKind::PrAuthored, ActivityKind::PrReviewed],
        setup: &[
            SetupStep::Command {
                run: "gh auth login",
            },
            SetupStep::Toggle {
                field: "github_prs",
            },
        ],
        docs: "github",
    },
    Connector {
        id: "gitlab_mrs",
        name: "GitLab merge requests (glab)",
        kind: ConnectKind::Cli,
        platforms: ALL,
        state: Support::Supported,
        blurb: "glab mr list, assigned and reviewing, every 5 min per GitLab repo \u{b7} your existing login",
        probes: &[Probe::cmd("glab")],
        produces: &[ActivityKind::PrAuthored, ActivityKind::PrReviewed],
        setup: &[
            SetupStep::Command {
                run: "glab auth login",
            },
            SetupStep::Toggle {
                field: "gitlab_mrs",
            },
        ],
        docs: "gitlab",
    },
    Connector {
        id: "tailscale",
        name: "Tailscale",
        kind: ConnectKind::Cli,
        platforms: ALL,
        state: Support::Planned,
        blurb: "which network you are on, which is how prod work tells itself apart from local work",
        probes: &[Probe::cmd("tailscale")],
        produces: &[],
        setup: &[],
        docs: "tailscale",
    },
    // ---- Accounts --------------------------------------------------------
    Connector {
        id: "google_calendar",
        name: "Google Calendar",
        kind: ConnectKind::Account,
        platforms: ALL,
        state: Support::Partial {
            note: "needs a Google Cloud OAuth client of your own; the ICS route is the easier one",
        },
        blurb: "the primary calendar's events as meeting spans, with attendees",
        probes: &[Probe::config("google_calendar")],
        produces: &[ActivityKind::Meeting],
        setup: &[
            SetupStep::Account { flow: "gcal-login" },
            SetupStep::Toggle {
                field: "google_calendar",
            },
        ],
        docs: "calendars",
    },
    // ---- MCP servers -----------------------------------------------------
    Connector {
        id: "mcp_servers",
        name: "MCP servers",
        kind: ConnectKind::Mcp,
        platforms: ALL,
        state: Support::Supported,
        blurb: "local and remote MCP servers answer the chat's context calls; nothing is called unless you ask",
        probes: &[],
        produces: &[],
        setup: &[],
        docs: "mcp",
    },
    Connector {
        id: "jira",
        name: "Jira",
        kind: ConnectKind::Mcp,
        platforms: ALL,
        state: Support::Supported,
        blurb: "PROJ-123 in a branch anchors the task; the Atlassian MCP server fetches the issue on demand",
        probes: &[Probe::cmd("uvx")],
        produces: &[],
        setup: &[SetupStep::Command {
            run: "uvx mcp-atlassian",
        }],
        docs: "mcp",
    },
    Connector {
        id: "linear",
        name: "Linear",
        kind: ConnectKind::Mcp,
        platforms: ALL,
        state: Support::Planned,
        blurb: "ENG-123 keys, lowercase in branches, and the official remote server for the issue itself",
        probes: &[],
        produces: &[],
        setup: &[],
        docs: "mcp",
    },
    Connector {
        id: "sentry",
        name: "Sentry",
        kind: ConnectKind::Mcp,
        platforms: ALL,
        state: Support::Planned,
        blurb: ".sentryclirc names the project; the issue id says which fire you were putting out",
        probes: &[Probe::path(".sentryclirc")],
        produces: &[],
        setup: &[],
        docs: "mcp",
    },
    Connector {
        id: "slack",
        name: "Slack",
        kind: ConnectKind::Mcp,
        platforms: ALL,
        state: Support::Planned,
        blurb: "channel names come from the window title today; the official server would give the thread",
        probes: &[],
        produces: &[],
        setup: &[],
        docs: "mcp",
    },
];

/// The connector with this id.
pub fn by_id(id: &str) -> Option<&'static Connector> {
    REGISTRY.iter().find(|c| c.id == id)
}

/// Connectors whose descriptor claims this platform, in registry order.
pub fn for_platform(p: Platform) -> impl Iterator<Item = &'static Connector> {
    REGISTRY.iter().filter(move |c| c.platforms.contains(&p))
}

/// Whether this connector is switched on in the config: `None` when no
/// config field decides it (it is on whenever the tool is there).
///
/// Whether it is *working* is `health::health_of`, which reads this plus
/// the probes and the rows.
pub fn enabled(cfg: &Config, c: &Connector) -> Option<bool> {
    let field = c.setup.iter().find_map(|s| match s {
        SetupStep::Toggle { field } | SetupStep::Field { field, .. } => Some(*field),
        _ => None,
    })?;
    config_on(cfg, field)
}

/// Is this config field switched on? The one list mapping a descriptor's
/// field name to the typed `Config`, shared by `enabled` and by the
/// `Config` probe.
pub fn config_on(cfg: &Config, field: &str) -> Option<bool> {
    match field {
        "git_repos" => Some(!cfg.git_repos.is_empty()),
        "ai_session_dirs" => Some(!cfg.ai_session_dirs.is_empty()),
        "calendars" => Some(!cfg.calendars.is_empty()),
        "github_prs" => Some(cfg.github_prs),
        "gitlab_mrs" => Some(cfg.gitlab_mrs),
        "mic_capture" => Some(cfg.mic_capture),
        "google_calendar" => Some(cfg.google_calendar),
        "shell_history" => Some(cfg.shell_history),
        "editor_heartbeats" => Some(cfg.editor_heartbeats),
        "shell_hook" => Some(cfg.shell_hook),
        "discover_repos" => Some(cfg.discover_repos),
        "browser_history" => Some(cfg.browser_history),
        _ => None,
    }
}

/// The registry as the JSON the site reads (`docs/connectors.json`).
/// Committed, so Render — which has no cargo — never has to build anything.
pub fn to_json() -> String {
    let mut s = serde_json::to_string_pretty(&REGISTRY).expect("registry serialises");
    s.push('\n');
    s
}

/// Open integration requests by connector id (m41 chunk 5): the issue a
/// `Planned` row points at, so a person sees their request is on a list.
/// `scripts/requests.sh` prints the lines to paste here.
pub const REQUESTED: &[(&str, u32)] = &[];

pub fn issue_for(id: &str) -> Option<u32> {
    REQUESTED.iter().find(|(c, _)| *c == id).map(|(_, n)| *n)
}

pub const ISSUES: &str = "https://github.com/james-clarke/chronicle/issues";

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The site's tools page body (m41 chunk 5): every connector by kind, its
/// state, the platforms the descriptor covers, and what it gives you. The
/// fragment is committed into `site/tools.html` between `<!-- tools -->`
/// markers (`chronicle connections --html`, then `scripts/site-tools.sh`);
/// a drift test keeps it equal to this.
pub fn to_site_html() -> String {
    let mut out = String::new();
    for kind in ConnectKind::ORDER {
        let rows: Vec<&Connector> = REGISTRY.iter().filter(|c| c.kind == kind).collect();
        if rows.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "<h3>{}</h3>\n<table class=\"tools\">\n",
            esc(kind.label())
        ));
        out.push_str(
            "<tr><th>Tool</th><th>State</th><th>Platforms</th><th>What it gives you</th></tr>\n",
        );
        for c in rows {
            let (class, state) = match c.state {
                Support::Supported => ("supported", "supported".to_owned()),
                Support::Partial { note } => ("partial", format!("partial \u{b7} {}", esc(note))),
                Support::Planned => (
                    "planned",
                    match issue_for(c.id) {
                        Some(n) => format!("planned \u{b7} <a href=\"{ISSUES}/{n}\">#{n}</a>"),
                        None => "planned".to_owned(),
                    },
                ),
                Support::Detected => ("detected", "detected, not read".to_owned()),
                Support::WontDo { reason } => {
                    ("wontdo", format!("not planned \u{b7} {}", esc(reason)))
                }
            };
            let platforms: Vec<&str> = c
                .platforms
                .iter()
                .map(|p| match p {
                    Platform::Linux => "Linux",
                    Platform::MacOs => "macOS",
                    Platform::Windows => "Windows",
                })
                .collect();
            out.push_str(&format!(
                "<tr id=\"{}\"><td>{}</td><td class=\"st {class}\">{state}</td><td>{}</td><td>{}</td></tr>\n",
                esc(c.id),
                esc(c.name),
                platforms.join(" \u{b7} "),
                esc(c.blurb)
            ));
        }
        out.push_str("</table>\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_and_slug_shaped() {
        let mut seen: Vec<&str> = Vec::new();
        for c in REGISTRY {
            assert!(!seen.contains(&c.id), "duplicate id {}", c.id);
            assert!(
                c.id.chars().all(|ch| ch.is_ascii_lowercase() || ch == '_'),
                "id {} is not a slug",
                c.id
            );
            assert!(
                !c.name.is_empty() && !c.blurb.is_empty(),
                "{} has no copy",
                c.id
            );
            assert!(!c.platforms.is_empty(), "{} claims no platform", c.id);
            seen.push(c.id);
        }
    }

    /// Every source the Settings panel drew before the registry existed
    /// (m41 chunk 0's gate).
    #[test]
    fn every_panel_source_has_a_descriptor() {
        for id in [
            "claude_code_sessions",
            "agent_sessions",
            "browser_history",
            "repo_discovery",
            "link_files",
            "editor_workspaces",
            "shell_history_atuin",
            "calendars_ics",
            "editor_heartbeats",
            "shell_hook",
            "tmux",
            "docker_compose",
            "mic_capture",
            "github_prs",
            "gitlab_mrs",
            "google_calendar",
        ] {
            assert!(by_id(id).is_some(), "no descriptor for {id}");
        }
    }

    /// A `Toggle` or `Field` step names a real config field, so the panel
    /// and the CLI cannot report a switch that does not exist.
    #[test]
    fn toggles_name_a_config_field() {
        let cfg = Config::default();
        for c in REGISTRY {
            let names_field = c
                .setup
                .iter()
                .any(|s| matches!(s, SetupStep::Toggle { .. } | SetupStep::Field { .. }));
            if names_field {
                assert!(
                    enabled(&cfg, c).is_some(),
                    "{} toggles a field `enabled` does not know",
                    c.id
                );
            }
        }
    }

    /// The JSON the site reads survives a trip through text.
    #[test]
    fn json_round_trips() {
        let text = to_json();
        let back: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(back, serde_json::to_value(REGISTRY).unwrap());
        let rows = back.as_array().expect("an array");
        assert_eq!(rows.len(), REGISTRY.len());
        assert_eq!(rows[0]["id"], "git_repos");
        assert_eq!(rows[0]["state"], "supported");
    }

    #[test]
    fn detected_is_runtime_only() {
        assert!(
            REGISTRY.iter().all(|c| c.state != Support::Detected),
            "Detected is what chunk 3 finds, never what the table declares"
        );
    }
}
