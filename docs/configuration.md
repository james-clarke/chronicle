# Configuration

Chronicle reads its settings from `config.toml` in its data directory, with a few sibling files for secrets that do not belong in a hand-edited settings file. This page lists every field, the other files in the data directory, and the environment variables that change behaviour.

## config.toml

A missing `config.toml` is not an error. Chronicle starts with the defaults below and writes the file the first time you save something from the Settings panel. A file that fails to parse, or that carries a field name the current build does not know, stops the daemon rather than falling back silently. The daemon reads the file at start, so a hand edit needs a restart.

### Capture and activity detection

| Field | Default | Meaning |
|---|---|---|
| `batch_minutes` | `30` | A batch of activity closes after this many minutes of non-AFK time. |
| `batch_min_minutes` | `10` | A batch may also close early at an AFK gap of 5 minutes or more, once it already holds at least this many minutes of activity. |
| `afk_close_secs` | `120` | Seconds without input before the AFK poller reports the user idle. |
| `quiet_secs` | `600` | Idle time with nothing live on screen (no agent writing, no call, no meeting window) before a span closes at the point idle began. `0` closes on any idle. |
| `away_secs` | `1800` | Idle time with a live context (something was actively running) before a span closes. |
| `capture_presence` | `true` | Count keystrokes, clicks, scroll and motion per minute into a `presence` row (counts only, never keystroke content). Off means no row is written. Linux X11 and macOS only; Wayland has no protocol for it. |
| `focus_route` | `"auto"` | Which focus provider runs on Linux: `auto` reads the session environment, `x11` forces X11 (also how X11 apps running under Xwayland get captured), `wlr` uses the wlr-foreign-toplevel protocol, `kwin` uses a KWin script. Ignored on macOS. |
| `mic_capture` | `true` | Watch for apps holding the microphone open (PipeWire, Linux) and store each stretch as a `call`. |
| `browser_apps` | `["firefox", "librewolf", "zen", "navigator", "chrome", "chromium", "brave", "vivaldi", "opera", "edge", "safari"]` | Apps whose focus spans are split per site by browser URL heartbeats (case-insensitive substring match on the window's app name). |
| `excluded_apps` | `[]` | Regexes; matching apps are never stored at all. |
| `excluded_titles` | `[]` | Regexes; matching window titles are never stored at all. |
| `distraction_patterns` | `[]` | Regexes marking apps or sites as distractions in insights, matched against the app name and the browser site key. Empty turns the feature off. |

### Sessionizing and derivation

| Field | Default | Meaning |
|---|---|---|
| `derive_idle_secs` | `300` | Derivation (turning raw activity into labeled tasks) may start once the user has been idle at least this long. |
| `live_secs` | `300` | While active, the resident derive worker re-labels the current stretch this often. `0` turns the live tier off. |
| `worker_idle_secs` | `1200` | The resident derive worker (model loaded, prompt prefix cached) exits after this long without a request. `0` means it exits after every request. |
| `prepass_secs` | `60` | How often the deterministic pre-pass (branch to ticket, repo to project, past corrections) places provisional intervals over the not-yet-derived tail. `0` turns it off. |
| `title_similarity` | `0.8` | Consecutive same-app events merge into one span when normalized title similarity is at least this (absorbs jitter such as an unread-count prefix changing). |
| `retention_days` | `180` | Rows older than this are pruned daily. `0` keeps rows forever. Corrections are always kept regardless of this setting. |
| `task_autoclose_days` | `3` | Open derived tasks with no interval for this many days are closed automatically each day. `0` turns this off; declared tasks only close by hand either way. |
| `derive_mode` | `"model"` | How the live tail gets placed. `model` runs the deterministic pre-pass, then the resident model every `live_secs`, then a model batch derive. `segmenter` does deterministic segmentation over span anchors scored against task evidence, reduces the batch tier to a re-score, and only asks the model to name new tasks. |
| `segment_switch_min` | `3` | Segmenter only: minutes a run of unrelated spans must last before it becomes its own segment; shorter excursions fold into the surrounding one. |
| `segment_new_task_min` | `10` | Segmenter only: minutes a stretch the scorer calls new must last before a task gets created for it. |
| `embed_model` | `None` | Embedding model for the soft tier: a file name inside the models directory (from `chronicle model pull bge-small`) or a full path. Unset means no vectors; title words alone carry the soft tier. |
| `scorer_delta` | `None` | Score margin a placement needs to skip a "to confirm" state in the segmenter. Unset uses the scorer's own default; `chronicle bench --calibrate` prints the value the verdict log currently supports. |
| `ticket_regex` | `"[A-Z][A-Z0-9]+-[0-9]+"` | Full-match-anywhere regex used to pull a ticket key out of branch names, anchoring derived tasks to it. |
| `checkpoint_afk_secs` | `1800` | Idle time (lunch-scale) that queues a checkpoint for every task with activity since its last one. `0` turns the feature off. |
| `task_stuck_days` | `3` | An open task with nothing moving it (no interval, no fresh checkpoint) for this many days gets a "stuck" chip. `0` turns the feature off. |
| `background_minutes` | `10` | Derived tasks totaling under this many minutes in a day collapse into the timeline's background strip and stay out of activity-only standup drafts. `0` turns this off. Declared tasks and tasks carrying journals, checkpoints, or a ticket reference never collapse. |

### Projects and tasks

| Field | Default | Meaning |
|---|---|---|
| `git_repos` | `[]` | Repo paths polled for branch and commit evidence (`~` expanded). Empty means git capture is off. |
| `projects` | `[]` | The projects in force; every focus span files into the first project whose rule matches it (repo path or place, ticket prefix, domain, title regex, app), or stays unfiled. Empty means one project per `git_repos` entry, named after its folder. |
| `project_join_min` | `2` | An unfiled span shorter than this many minutes, sitting between two spans of the same project, joins that project (treats a quick tab glance as part of the surrounding work). `0` turns this off. |

Each entry in `projects` is a `ProjectCfg`:

| Field | Default | Meaning |
|---|---|---|
| `name` | `""` | Project name. |
| `repos` | `[]` | Repo or folder paths (`~` expanded); a repo's worktrees count as the repo. |
| `tickets` | `[]` | Work-item key prefixes, matched case-insensitively against item anchors and branch names. |
| `domains` | `[]` | Sites (for example `contoso.atlassian.net`); a subdomain of a listed site matches too. |
| `titles` | `[]` | Window-title regexes, matched anywhere in the title. |
| `apps` | `[]` | Whole apps, matched case-insensitively by app name. |
| `derive` | `true` | Whether derived sub-tasks are minted inside this project. |

### Sources and integrations

| Field | Default | Meaning |
|---|---|---|
| `ai_session_dirs` | `["~/.claude/projects"]` | Directories of AI coding transcripts watched for session evidence, in the layout `<dir>/<project>/*.jsonl`. Empty turns this off. |
| `ai_session_formats` | `[]` | Which transcript formats besides the Claude Code layout to read: `codex`, `gemini`, `copilot`, `aider`, `cline`, `amp`, `opencode`, `cursor`. Empty means every format whose directory exists on the machine is read. |
| `github_prs` | `false` | Poll `gh search prs` for the signed-in user's authored and reviewed pull requests. Needs `gh auth login`. |
| `gitlab_mrs` | `false` | Poll `glab mr list` for the signed-in user's assigned and reviewed merge requests. Needs `glab auth login`. |
| `google_calendar` | `false` | Poll the primary Google Calendar for meeting spans. Needs `chronicle gcal-login` first. |
| `shell_history` | `false` | Fold atuin shell history into shell spans, keyed by repo (cwd, `argv[0]`, and duration only). |
| `editor_heartbeats` | `true` | Accept WakaTime-style heartbeats from editor plugins on the local endpoint and fold them into edit spans. Off makes the route answer 403. |
| `shell_hook` | `true` | Accept posts from the `chronicle shell-init` precmd hook (cwd, program, duration; never the command line) on the local endpoint and fold them into shell spans. Off makes the route answer 403. |
| `discover_repos` | `true` | Scan the parents of `git_repos` entries for git repos no configured project claims, and file them as discovered projects. |
| `browser_history` | `true` | Read the browsers' history databases (a copy, read-only; query strings dropped) into `browse` rows so a tab's real URL anchors the span. |
| `calendars` | `[]` | ICS calendars (URLs or file paths) polled for meeting spans, no OAuth required. |

### Server

| Field | Default | Meaning |
|---|---|---|
| `port` | `5600` | Port for the ActivityWatch-compatible local HTTP server. |
| `cors_allow` | `[]` | Extra CORS origin regexes for sideloaded browser extensions (full-match); the stock aw-watcher-web origins are already built in. |

### Model

| Field | Default | Meaning |
|---|---|---|
| `model_path` | `None` | Explicit path to the local GGUF model. Unset means Chronicle looks for the default downloaded preset under `<data dir>/models/`; if that is not there either, the model step is skipped and only the rules place time. |
| `model_path_heavy` | `None` | A larger model used only for the day-tier consolidation pass. Unset means that pass uses the same model as everything else. |

### MCP

| Field | Default | Meaning |
|---|---|---|
| `mcp_config` | `None` | Path to the MCP allowlist file. Unset means `<data dir>/mcp.toml`. |

## Other files in the data directory

### mcp.toml

The MCP allowlist. Every call Chronicle may make is spelled out here; there is no dynamic tool selection. A missing file means MCP is off, and an invalid file is an error. The Settings panel writes it with mode 0600 because server entries can carry tokens in `env`. The daemon reloads it on every call, so edits apply without a restart.

```toml
[[servers]]
name = "jira"
command = "/usr/bin/uvx"
args = ["mcp-atlassian"]
enabled = true

[servers.env]
JIRA_API_TOKEN = "..."

[[context_calls]]
server = "jira"
tool = "jira_search"
args_json = '{"jql": "assignee = currentUser()"}'
```

A server needs either `command` (stdio) or `url` (a remote streamable HTTP or SSE server). A remote server's token comes from `bearer_command` (a shell line such as `gh auth token`) or `bearer_env`, never from a token stored in the file. `context_calls` run on every derivation. `fetch_calls` run once per task and may use a `{ref}` placeholder for the task's ticket key. `action_calls` are writes you trigger by hand, such as posting a journal entry to a ticket, and are never scheduled. [Sources](sources.md) has the full example.

### models.toml

Cloud model backends and which jobs go to them. Kept out of `config.toml` because it carries API keys, and written with mode 0600. A missing file means everything runs locally.

```toml
[backends.anthropic]
kind = "anthropic"
model = "claude-sonnet-5"
api_key = "sk-ant-..."

[routes]
chat = "anthropic"
standup = "anthropic"

max_usd_per_day = 2.0
```

`backends` names each cloud backend. The kind is `anthropic`, `open_ai_compat`, or `claude_code`, which uses your own Claude Code login and needs no key. `routes` maps a job kind (`chat`, `narrative`, `standup`, `derive`, `live` and the rest) to a backend name. A route naming a backend that no longer exists behaves as if unset, and the job runs locally. `max_usd_per_day` defaults to `2.0`; once the day's spend reaches it, every route falls back to the local model until midnight.

### google.toml

Written by `chronicle gcal-login` and read by the Google Calendar collector. Mode 0600, never logged.

```toml
client_id = "..."
client_secret = "..."
refresh_token = "..."
email = "you@example.com"
```

### Data directory location

Chronicle resolves its data directory once at start:

| Platform | Path |
|---|---|
| Linux | `$XDG_DATA_HOME/chronicle` (typically `~/.local/share/chronicle`) |
| macOS | `~/Library/Application Support/chronicle` |
| Windows | `%APPDATA%\chronicle` |

The directory holds `config.toml`, `chronicle.db`, `mcp.toml`, `models.toml`, `google.toml`, a `models/` directory for downloaded GGUF files and a `logs/` directory. The daemon's control socket, `chronicle.sock`, lives here too unless `XDG_RUNTIME_DIR` is set, in which case the socket goes there.

## Environment variables

| Variable | Effect |
|---|---|
| `CHRONICLE_GOOGLE_CLIENT_ID` | OAuth client id for `chronicle gcal-login`, used when `--client-id` is not passed. |
| `CHRONICLE_GOOGLE_CLIENT_SECRET` | OAuth client secret for `chronicle gcal-login`, used when `--client-secret` is not passed. |
| `XDG_RUNTIME_DIR` | Linux only. When set, the daemon's control socket (`chronicle.sock`) is created here instead of in the data directory. |
| `XDG_DATA_HOME` | Linux only. Relocates the whole data directory. |
| `RUST_LOG` | Log filter for `logs/chronicle.log` and stderr. Defaults to `info`. |
| `WAYLAND_DISPLAY`, `XDG_CURRENT_DESKTOP`, `KDE_SESSION_VERSION` | Linux only. Read when `focus_route` is `auto` to pick X11, wlr or KWin. Your session sets these; you do not. |
