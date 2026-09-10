# M37 — Sources and connections: read what the tools already write

Status: written and executed 2026-09-09 evening on James's standing
"grab next task and execute" instruction; research and rationale in
`dev-tools-direction.md` ("What developers use, and what Chronicle sees",
"Connections"). Runs after M36. Every "today" claim carries a `file:line`.

## The short version

Accuracy is capped by evidence shape (the M30 thesis), so every source that
names a directory, a branch, a ticket or a meeting is an accuracy win
before any model runs. Today Chronicle reads Claude Code transcripts, six
git repos it was told about, atuin, `gh`, PipeWire, Google Calendar behind
an OAuth client, and window titles. This milestone adds the sources that
are already on disk for most developers, the two local hooks that turn
terminal time into a cwd (shell precmd, git hooks), discovery so a repo
under `~/dev` files without a config edit, and a Connections page
organised by how much each kind unlocks per unit of user effort.

The gate on this box: the 7-day report (`chronicle project test`,
crates/app/src/project.rs:117) shows 84.9 % filed; the unfiled 290 min is
`continental` (80 min, a repo under `~/dev` no rule knows), Terminator
titles that are Claude Code session titles or bare cwd prompts (75 min),
a markdown viewer tab (68 min), Meet (13 min). Discovery, the shell hook
and session-title matching take the first two; the third is a
`localhost`-style file preview that the browser-history reader resolves
through the page URL.

## What the code does today

- Collectors are `FocusProvider` threads spawned from
  `spawn_capture` (crates/app/src/capture.rs:15); each writes
  `ActivityEvent { ts, end_ts, repo, branch, kind, ext_id, summary,
  detail }` (crates/core/src/types.rs:25) through
  `insert_activity_event` (crates/core/src/storage.rs:161) into
  `activity_events` (migration 010); `repo` is a place basename, and the
  project is resolved later by `Matcher::resolve`
  (crates/core/src/project.rs:114).
- `ActivityKind` has eleven variants (types.rs:49) with a fixed dedupe
  policy each (types.rs:131); the Home sources row maps kinds to words
  (`source_word`, crates/app/src/ui/mod.rs:2858) in `SOURCE_ORDER`
  (mod.rs:2852) from `activity_kinds_by_repo` (storage.rs:1151).
- AI sessions: Claude Code's layout only
  (crates/capture/src/ai_sessions.rs:1); cwd basename is the place
  (ai_sessions.rs:484); `detail` carries prompts, paths, prompt minutes
  and terminal titles.
- Shell: atuin's `history.db`, one `shell` span per configured repo by
  cwd prefix (crates/capture/src/shell.rs:203), `argv[0]` only. Off by
  default (config.rs:121) and off here.
- Git: a 20 s poll of `.git/HEAD` per `git_repos` entry
  (crates/capture/src/git.rs:1); no hooks, no reflog.
- Identity: remote `host/org/repo` and worktrees as instances
  (project.rs:332, :361, M35 chunk 0); no discovery — a repo outside
  `git_repos` is unfiled (`projects_effective`, config.rs:247).
- Editors: WakaTime heartbeats (crates/server/src/lib.rs:373) and title
  paths (crates/core/src/extract.rs:1107); no workspace lists.
- Browser: URL only from aw-watcher-web (server lib.rs:348); otherwise
  the title and the domain guessed from it.
- Calendar: Google OAuth only (crates/capture/src/gcal.rs:1).
- MCP: a stdio client with presets (crates/mcp/src/lib.rs:18,
  crates/app/src/ui/connections.rs:65); no remote servers.
- Config is `deny_unknown_fields` (config.rs:17): every new source is a
  typed field. Settings › Connections lists sources globally
  (`sources_ui`, connections.rs:1258).
- No `mode` anchor: `AnchorKind` is Item, Change, Branch, Session, Event,
  Doc, Place, People, Domain (extract.rs:19).

## Chunks and gates

### 0. Session formats

- `SessionFormat` trait in `crates/capture/src/sessions/`: `name()`,
  `roots()` (default dirs, `~` expanded, on when the dir exists),
  `scan(root) -> Vec<PathBuf>`, `read(path, from) -> (Vec<Rec>, cursor)`
  where `Rec { ts, cwd, branch, prompt, paths, title }` is what
  `ai_sessions.rs` already folds into a `Segment`. Claude Code moves
  behind the trait unchanged; `AiSessionProvider` runs every detected
  format. `ext_id` becomes `<format>:<session>[#n]` (Claude keeps its bare
  id so nothing re-mints) and `detail.tool` names the format.
- Formats, from the direction doc's table and the public layouts:
  Codex (`~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`, `session_meta`
  carries cwd), Gemini CLI (`~/.gemini/tmp/<hash>/chats/session-*.json`,
  cwd from the sibling project file), Copilot CLI
  (`~/.copilot/session-state/<id>/workspace.yaml` + `events.jsonl`),
  Aider (`.aider.chat.history.md` at each repo root and worktree),
  Cline (`globalStorage/saoudrizwan.claude-dev/tasks/<ts>/`),
  Amp (`~/.local/share/amp/threads/T-*.json`), OpenCode
  (`~/.local/share/opencode/opencode.db`, read-only SQLite), Cursor
  (`~/.config/Cursor/User/globalStorage/state.vscdb` `cursorDiskKV`,
  last: its schema churns). Each parser reads timestamps, cwd, the first
  prompt clipped to 120 chars and touched paths; never the transcript.
- Config: `ai_session_dirs` stays for Claude Code; new
  `ai_session_formats: Vec<String>` (empty = every detected format;
  `["claude"]` pins it).
- Gate: one fixture per format under `fixtures/sessions/<tool>/`, each
  yielding cwd, first-prompt time and prompt in a test; Settings ›
  Connections "AI sessions" row lists the formats found on this machine.

### 1. Shell hook, tmux, Docker

- `chronicle shell-init zsh|bash|fish|pwsh` prints a hook: preexec keeps
  the start time and `argv[0]`, precmd POSTs `{cwd, program, started,
  ended}` to `127.0.0.1:<port>/api/chronicle/shell` in the background with
  a one-second timeout. No command line ever leaves the shell. The route
  folds into `shell` spans through the fold `shell.rs` already has, keyed
  by cwd (any directory, not only configured repos; the place is the git
  root's basename when the cwd is inside a repo, else the cwd basename).
  `shell_hook: bool` (default on) gates the route like
  `editor_heartbeats`.
- tmux: when `tmux` is on PATH, poll `tmux list-panes -a -F` every 60 s for
  active panes' `pane_current_path` → `cwd` events `tmux:<pane>`.
- Docker Compose: when `docker` is on PATH and the socket answers, poll
  `docker ps` with the `com.docker.compose.project.working_dir` label
  every 60 s → `cwd` events `docker:<container>`; a stack's directory is a
  place while it runs.
- Gate: unit tests for the fold and the route; `shell-init` output sourced
  in a scratch zsh posts a span that lands in `activity_events`. The rc
  line is James's to add (his dotfiles repo).

### 2. Discovery, git hooks, link files, `glab`

- Discovery: the parents of `git_repos` entries (`~/dev` here) are
  scanned on the git poll; every git repo found that no project claims is
  a **discovered project** (name = folder, `derive` on) in
  `projects_effective` order after the configured ones. Home shows it
  with the amber "not configured" chip; `chronicle project test` lists
  them under "discovered". This files `continental`, `dotfiles`,
  `brotherhood-tooling` with no config edit.
- Git hooks, opt-in: `chronicle hooks install [repo]` appends one line to
  `post-checkout`, `post-commit`, `post-rewrite` after whatever is there
  (Husky, pre-commit) and never replaces; the line runs
  `chronicle hook <name>` which POSTs `{repo, event, branch, commit}` to
  `/api/chronicle/git`, giving exact-second checkouts and commits the
  20 s poll misses. `chronicle hooks remove` strips the line. A
  "hooks" chip per repo in Settings › Connections.
- Reflog backfill: on the first poll of a repo (and `chronicle hooks
  backfill`), `git reflog show --date=iso -g HEAD` fills `checkout`
  events for the last 30 days that the poll never saw (dedupe on
  `reflog:<sha>@<ts>`).
- Link files, read on the git poll, names and ids only:
  `.vercel/project.json`, `.netlify/state.json`, `fly.toml`,
  `render.yaml`, `railway.json`, `wrangler.toml`,
  `supabase/config.toml`, `.doppler.yaml`, `.sentryclirc`,
  `.circleci/config.yml`, `.buildkite/pipeline.yml`, plus the build
  system (`Cargo.toml`, `package.json`, `pom.xml`, `build.gradle`,
  `nx.json`, `turbo.json`). Stored in `repo_links` (migration 036:
  repo, kind, name, url_pattern, seen_ts); the `Matcher` loads them so a
  dashboard URL (`vercel.com/<team>/<project>`, `fly.io/apps/<name>`,
  `supabase.com/dashboard/project/<ref>`, `sentry.io/organizations/<org>/…`)
  files into the project with no `domains` edit; the build system goes in
  the project's vocabulary for the standup.
- `glab`: when on PATH and `gitlab_mrs` is on, `glab mr list
  --assignee=@me` and `--reviewer=@me` every 5 min → `pr_authored` /
  `pr_reviewed`, same shape as `github.rs`.
- Gate: `chronicle project test` shows the discovered repos filed;
  `hooks install` on this repo records the next commit within a second;
  `render.yaml` here yields a `render` link row; unit tests for the link
  parsers and the reflog reader.

### 3. Editor workspaces

- VS Code family (`Code`, `Code - OSS`, `VSCodium`, `Cursor`, `Windsurf`)
  `globalStorage/state.vscdb` key `history.recentlyOpenedPathsList` →
  local folders and remote URIs (`vscode-remote://ssh-remote+<host>/<path>`,
  `codespaces+<name>`); `workspaceStorage/<hash>/workspace.json` for the
  folder of each open window. JetBrains `options/recentProjects.xml`
  (`projectOpenTimestamp`, `activationTimestamp`), Zed `db/*/db.sqlite`
  `workspaces`.
- Stored in `editor_workspaces` (migration 036: editor, path, remote,
  last_ts). Two uses: discovery candidates (chunk 2's list), and the
  title resolver in extract.rs — a bare folder name in an editor title
  resolves to the recorded path, and a `[SSH: host]` title to the remote
  path, so remote-dev time files. JetBrains activation timestamps also
  give `cwd` events `jetbrains:<path>`.
- Gate: fixture tests for the three readers; none of the editors is on
  this box, so the machine gate is chunk 2's.

### 4. Browser history, the `mode` anchor, ICS calendars

- Browser history: Chromium family (`History`: `urls` × `visits`,
  WebKit microseconds) and Firefox family (`places.sqlite`: `moz_places`
  × `moz_historyvisits`, epoch microseconds), copied then queried
  read-only every 60 s from the last visit seen; profiles found under
  the known roots. Each visit is a `browse` event (new `ActivityKind`):
  `repo` empty, `ext_id` = the visit id, `summary` = the title,
  `detail` = `{"url": "<scheme://host/path>"}` with the query and
  fragment dropped. `browser_history: bool` default on; private windows
  never reach the DB.
- extract.rs reads `browse` events overlapping a browser focus span the
  way it reads AW's URL today, so `Domain` and `Item` anchors come from
  the real URL, and `localhost:<port>` resolves through the ports table.
- `mode` anchor (`AnchorKind::Mode`, Weak): from URL families and apps —
  deploying (CI run pages: GitHub Actions, GitLab pipelines, CircleCI,
  Buildkite; Vercel/Render/Fly deploy pages; Argo CD), on-call
  (PagerDuty, Opsgenie, incident.io, Slack `#inc-` titles), debugging
  prod (Sentry issues, Datadog, Grafana, New Relic, Honeycomb, Kibana),
  testing an API (Postman, Bruno, Insomnia, Hoppscotch), database
  (DBeaver, TablePlus, DataGrip, pgAdmin, psql titles), infra (AWS, GCP,
  Azure consoles), design, docs, mail. Reports and the standup carry
  minutes by mode per project ("of which 40 min deploying").
- ICS: `calendars: Vec<String>` (URLs or paths), fetched every 15 min,
  VEVENTs in ±7 days → `meeting` spans as `gcal.rs` writes them; `RRULE`
  daily and weekly with `BYDAY`, `UNTIL`, `COUNT`; other rules logged
  once. Zero OAuth: Google's secret iCal address, Outlook publish,
  Fastmail all work.
- Gate: fixture tests for both history schemas, the mode table and the
  ICS parser; on this box Chrome and Firefox history fill `browse` rows
  and the markdown-viewer tab files by URL.

### 5. Connections taxonomy, CLIs, remote MCP, status

- Settings › Connections regroups by kind: **Files on this machine**
  (repos + discovered, sessions with the formats found, link files,
  editor workspaces, browser history, calendars), **Local servers and
  sockets** (editor heartbeats, ActivityWatch, shell hook, tmux, Docker,
  mic, lock), **Your CLIs** (`gh`, `glab`, `gcloud`, `az`, `aws`,
  `vercel`, `op`, `tailscale` — detected on PATH with the auth state
  from `<cli> auth status`; "use your existing login"), **Accounts**
  (Google Calendar), **MCP servers** (presets and remote), **Tokens**.
- Remote MCP: `[[servers]]` in `mcp.toml` accepts `url` with
  `bearer_command` (e.g. `gh auth token`) or `bearer_env`; rmcp's
  streamable-HTTP client transport. OAuth 2.1 loopback waits for a server
  that needs it.
- `chronicle status` gains a sources block: one line per kind with last
  seen and this week's count (from `latest_activity_per_kind`,
  storage.rs:313); `chronicle status --json` carries it.
- Sources row: new words `browser`, `tmux`, `docker`, `hooks`, `links`,
  `workspaces`; hover lists the formats.
- Gate: unfiled below 10 % on the 7-day self-score with no config edit
  beyond the six repos; every source that fed a project this week shows
  in its Sources row.

## Out of scope

macOS and Windows collectors (M38, M40), the GNOME extension, OAuth 2.1
for MCP, webhooks, native shell history (command text is a secrets trap),
kubectl/k9s/Tilt, Graphite, the Windows Visual Studio MRU.
