# Sources

Every source Chronicle can draw evidence from is one entry in a registry that also drives `chronicle connections`, the Settings panel and the site's tools page. This page groups them the way `chronicle setup` does: what already works once Chronicle is running, what takes one step, and what needs an install or an account first.

## Already working

Nothing to do beyond running the daemon. These read what is already on the machine and are on by default.

### Window focus, idle and presence

Platform code sits behind the same traits everywhere, so nothing downstream knows which platform produced an event.

| Platform | Route | What it reads |
|---|---|---|
| Linux (X11, and Xwayland clients) | X11 | active window, title and pid; idle time from the screensaver extension; per-minute key, button, motion and scroll counts from XInput 2 raw events |
| Linux (wlroots: Sway, Hyprland, Niri, river, labwc) | wlr foreign toplevel protocol | app id, title and window state, pushed by the compositor; the protocol carries no pid, so a terminal's working directory comes from the compositor's own IPC |
| Linux (KDE Plasma) | a KWin script loaded over D-Bus | window activation and caption changes, including pid, so terminal cwd resolves the same way X11's does |
| Linux (idle, both Wayland routes) | ext-idle-notify, or the KDE idle protocol where that is missing | idle time only |
| macOS | NSWorkspace polled once a second, plus the Accessibility API | frontmost app; the window title needs a one-time Accessibility grant and reads empty without it |
| macOS (idle and presence) | CoreGraphics event counters, polled | idle time and per-minute input counts |
| Windows | not built yet | not built yet |

Presence counts exist on X11 and macOS only; Wayland has no protocol for them. Screen lock comes from logind on Linux and from the session state on macOS, independent of the focus route.

Set `focus_route` (`auto`, `x11`, `wlr`, `kwin`) in `config.toml` to force a route; `auto` reads the session environment.

### Git repos and hooks

Point `dev_roots` at a folder such as `~/dev` and every git repo directly under it is watched: a new clone shows up on the daemon's next start, no config edit. Repos elsewhere are named one by one in `git_repos`. Watched repos are polled every 20 seconds for the current branch and new commits; a repo that only discovery below found is filed as a project but not polled. `chronicle hooks install` appends one backgrounded, silenced line to each repo's `post-checkout`, `post-commit` and `post-rewrite` hooks, marked with a trailing `# chronicle` comment and never replacing an existing hook, so a checkout or commit is timestamped exactly rather than caught on the next poll. `chronicle hooks remove` strips that line, `chronicle hooks status` shows which repos have it, and `chronicle hooks backfill` reads the reflog for checkouts the poller never saw.

Stored: the repo's directory name (not the full path), the branch name, the commit hash and the subject line. Not stored: diffs, the files a commit touched, or anything past the subject line.

### AI coding session transcripts

Claude Code's transcripts under `~/.claude/projects/<project>/<session>.jsonl` (or another directory set in `ai_session_dirs`) are tailed incrementally as new lines land. A transcript becomes a new session span whenever its lines pause for a while, so a chat left open all day does not read as one long session.

The same reader covers Codex, Gemini, GitHub Copilot, Aider, Cline, Amp, opencode and Cursor session stores, each read from that tool's own directory (`~/.codex/sessions`, `~/.gemini/tmp`, `~/.copilot/session-state` and so on). `ai_session_formats` narrows the list; empty reads every format whose directory exists.

Stored per session: the working directory, the branch, the first prompt clipped to 120 characters, the files it touched, and whether it was writing at a given moment. Not stored: the transcript, later prompts, or file contents.

### Editor workspaces

Recently opened folders and projects, read from each editor's own state on disk; no editor needs to be running. VS Code and its forks (Cursor, Windsurf, VSCodium), JetBrains and Zed are covered. This resolves a bare folder name in a window title to the repo it belongs to and feeds repo discovery below. It produces no evidence rows of its own.

### Browser history

Chronicle copies a Chromium or Firefox profile's history database, including Firefox's write-ahead log so recent visits are not missed, queries visits since its own cursor, then deletes the copy. The browser's own file is never touched. Query strings are dropped before a `browse` row is written, and the URL path is kept up to 200 characters. On by default (`browser_history`).

### Repo discovery and link files

A `dev_roots` child is filed as its own project the moment it is watched, with no discovery step needed. With `discover_repos` on, the default, git repos found next to the ones you already watch are filed as discovered projects instead of showing up as unfiled time. Separately, each watched repo's deploy files (`.vercel/project.json`, `fly.toml`, `.sentryclirc` and similar) are read for the service name and id they declare, never their contents, so a project can be matched by the host it deploys to.

### Repo notes

Each configured repo's `.remember/today-*.md` files are read as `note` rows: what you or your tools wrote down, and when. A file is re-read only when its size or timestamp changes, nothing else in the repo is read, and only a clipped copy of a note's body is kept.

### Local dev servers, panes and containers

- **tmux panes**: every 60 s, the working directory of every pane in an attached session, if `tmux` is on `PATH`.
- **Docker Compose stacks**: every 60 s, each running stack's `working_dir` label, if `docker` is on `PATH`.
- **Listening ports**: every 60 s, the repo behind each local dev server. Linux reads `/proc/net/tcp` and macOS uses `lsof`. This is what makes a `localhost:<port>` browser tab land on the right project.

None of these have a config toggle. They run whenever their tool is present.

### Calls (microphone in use)

Watches which apps hold the microphone open and turns each stretch into a `call` span, which explains an idle gap with no calendar entry. Linux reads PipeWire through `pw-dump`. macOS reads CoreAudio's device-running flag but cannot name the app, so the span's app is "microphone". On by default (`mic_capture`).

## One step each

A config field, a command or a one-time paste. No install or account beyond what you already have.

### Shell hook

`chronicle shell-init <shell>` prints a hook for your shell. Add it to your rc file, or let the Setup view do it, and every command from then on posts its working directory, program name and duration to the local endpoint, never the command line. Those posts fold into `shell` spans keyed by place (the git root's name, else the directory's name), so any directory files, not only configured repos.

```sh
eval "$(chronicle shell-init zsh)"   # ~/.zshrc
chronicle shell-init fish | source   # ~/.config/fish/config.fish
chronicle shell-init pwsh | Invoke-Expression   # $PROFILE
```

On by default (`shell_hook`). The route answers 403 while it is off.

### Native shell history is not read

Chronicle does not read the history files of zsh, bash or fish. They hold command lines, which can carry secrets. The shell hook gives the same coverage without the command text.

### Shell history via atuin

If you use atuin, turn on `shell_history` and Chronicle polls its database every 60 s, read-only, folding entries into `shell` spans per repo. Only the working directory, the program name and the duration come out of the query; the command line is dropped inside it. Off by default, since the shell hook covers the same ground.

### Editor heartbeats (WakaTime-compatible)

The local HTTP endpoint speaks the WakaTime plugin protocol, so any WakaTime editor plugin works by pointing its `api_url` at Chronicle instead of wakatime.com, with the API key Settings shows you. Heartbeats fold into `edit` spans per project. On by default (`editor_heartbeats`). The endpoint answers 403 while it is off.

```ini
# ~/.wakatime.cfg
[settings]
api_url = http://127.0.0.1:5600/api
api_key = <the key Settings shows>
```

### ActivityWatch browser extension

The same endpoint answers enough of the ActivityWatch REST API for the stock aw-watcher-web browser extension to report the URL and title of your focused tab as it changes, which is more exact than browser history above. Install the extension and it finds the endpoint on its own. CORS is restricted to the stock extension origins plus any regex you add to `cors_allow`.

### Calendars (ICS)

Point `calendars` at a calendar's private iCal address or a local `.ics` path. No OAuth, no account. Polled every 15 minutes for a window a week either side of now, with daily and weekly recurring events expanded, into `meeting` spans.

```toml
calendars = ["https://calendar.example.com/secret/basic.ics"]
```

### Git repos, named by hand

If a repo sits outside every dev folder and discovery does not find it, add its path to `git_repos` directly.

## Needs an install or an account

A binary you may not have yet, or a sign-in flow, comes before the switch.

### GitHub pull requests

Needs the `gh` CLI, logged in with your own account:

```sh
gh auth login
```

Then turn on `github_prs`. Every 5 minutes Chronicle runs `gh search prs` for pull requests you authored or reviewed and updated in the last day, and stores number, title, URL, state and repository. No token is stored; it rides on your `gh` login, read-only.

### GitLab merge requests

Same shape, with `glab`:

```sh
glab auth login
```

Then turn on `gitlab_mrs`. `glab mr list` needs a repo context, so this runs once per configured repo whose git remote resolves to a GitLab host.

### Google Calendar

Needs an OAuth client of your own from Google Cloud. The ICS route above is the easier path if you only want events. Run:

```sh
chronicle gcal-login
```

which writes `<data dir>/google.toml` with mode 0600. The primary calendar is polled every 5 minutes with the events read-only scope, and only the event id, title, start and end are stored.

### MCP servers

MCP is off until `<data dir>/mcp.toml` exists. The Settings panel writes it, and the daemon reloads it on every call. A server is a local command (stdio) or a remote URL. A remote server's bearer token comes from a shell command such as `gh auth token`, or an environment variable, rather than the file.

```toml
[[servers]]
name = "jira"
command = "uvx"
args = ["mcp-atlassian"]

[[context_calls]]
server = "jira"
tool = "jira_search"
args_json = '{"jql": "assignee = currentUser() AND updated >= -2d", "limit": 5}'

[[fetch_calls]]
server = "jira"
tool = "jira_get_issue"
args_json = '{"issue_key": "{ref}"}'
```

`[[context_calls]]` run at derive time, each with a 10 s timeout, and about 500 tokens of the combined result go into the digest as `## Workspace context`. `[[fetch_calls]]` run once per task with `{ref}` replaced by its ticket key, with a budget of about 6 000 characters. `[[action_calls]]` are writes you trigger by hand from the task pane, such as posting a journal entry back to a ticket, and never run on a schedule.

Everything a server returns, like every window title and URL, is treated as untrusted text to label time with. The model's output grammar is what keeps it from doing anything else with it.

The registry names a few servers to make setup concrete:

- **Jira**: a work-item key like `PROJ-123` in a branch name anchors a task; `uvx mcp-atlassian` fetches the issue on demand.
- **Linear**, **Sentry**, **Slack**: planned, not built.

## Not connected yet

- **Tailscale**, **ngrok**: planned, not built. Which network you are on, or which service a tunnel exposes, would both help place work.

`chronicle connections` prints the live state of every entry on this machine, and the site's tools page lists them all with what each would take to connect.
