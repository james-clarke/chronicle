# M21 — Settings: connections

Builds m18 steps 1–3 (`m18-settings-plan.md`): the Settings panel gains a
**Connections** section — MCP servers with status + a test button, git repos
with per-repo status — and the rest of the panel moves toward the m18 layout.
One slice, no new tables. m18 steps 4–6 (calendar, GitHub, standup schedule)
stay separate milestones and plug into the server list this one builds.

## Today (verified in code)

- Settings is a config.toml editor: sections Capture / Derivation / Model /
  Storage & server / Integrations / Appearance
  (`crates/app/src/ui/settings.rs:154-266`). "Integrations" is one text field
  holding the mcp.toml path (`settings.rs:260-262`). Save rewrites
  config.toml; the daemon reads config at startup, so every save says
  "restart daemon to apply" (`settings.rs:9-12`, `:290`).
- Git repos: `Config.git_repos` (`crates/core/src/config.rs:52-54`) has no
  widget. `~` expansion is an inline closure in `spawn_git_capture`
  (`crates/app/src/main.rs:1932-1938`). `GitProvider::new` resolves `.git`
  and names a repo by its directory basename
  (`crates/capture/src/git.rs:38-56`, `resolve_git_dir` `:136-145`); the
  provider is built once at daemon start, polls every 20 s, has no stop
  signal. `vcs_events.repo` is that basename (`migrations/007`).
- MCP: mcp.toml already *is* a first-class server list — `[[servers]]`
  name/command/args/env, stdio only (rmcp 3.1.4), plus the allowlists
  `context_calls` / `fetch_calls` (`crates/mcp/src/config.rs:24-54`,
  `deny_unknown_fields`, Deserialize only). It is loaded fresh on every call
  (`gather_context` `lib.rs:31`, `fetch_context` `lib.rs:47`), so edits apply
  live without a restart. `connect()` (`lib.rs:137`) spawns the child and
  does the initialize handshake; nothing reads the server's name/version or
  lists its tools. `chronicle mcp-check` (`main.rs:2063`) runs the context
  calls and prints the text — the only test path, no per-server verdict.
- Live file: one server, `jira` = `/home/james/.local/bin/uvx mcp-atlassian`
  with `JIRA_URL` / `JIRA_USERNAME` / `JIRA_API_TOKEN` env; file mode 0600.
  Six git repos configured.
- Per-task context fetches already leave evidence: `ai_jobs` rows of kind
  `fetch_context` (`main.rs:600`), status/error readable via `ai_job_status`
  (`crates/core/src/storage.rs:448`). Not surfaced anywhere.
- Onboarding shows service + model cards only (`onboarding.rs:160`, `:237`).
- No native-dialog crate (rfd/ashpd/gtk) in the tree.

## Design decisions

### File of record (deviation from m18 step 1)

m18 proposed moving servers into config.toml as `mcp_servers`. Not doing
that: mcp.toml is already the list, holds secrets (0600), is loaded per call
(live apply), and its allowlists belong beside the servers. Instead:

- `McpConfig` / `ServerConfig` / `ContextCall` gain `Serialize`;
  `McpConfig::save(path)` writes atomically (temp file + rename, so the
  daemon never reads a torn file) with mode 0600. Hand comments are lost on
  save — same caveat as config.toml.
- `ServerConfig.enabled: bool` (serde default true). `run_blocking` skips
  calls to disabled servers; the server still counts as a valid call target
  so disabling never turns the file invalid (`UnknownServer`).
- `Config.mcp_config` stays as an advanced field (passes through `base`);
  the panel drops the text field. Path resolution
  (`mcp_config.unwrap_or(data_dir/mcp.toml)`, duplicated at `main.rs:568`,
  `:689`, `:2067`) becomes `Config::mcp_path(&self, data_dir)`.

### Probe ("test")

- `chronicle_mcp::probe_server(&ServerConfig) -> Result<ServerProbe, String>`:
  existing `connect()`, then `peer_info()` → `server_info.name/version`
  (rmcp `RunningService::peer_info`, `service.rs:1018`), `list_all_tools()`
  (`service/client.rs:1727`), `close()`. `ServerProbe { name, version,
  tools: Vec<String>, elapsed }`. Own current-thread runtime like
  `run_blocking`; 20 s overall timeout (uvx cold start is slow; the 10 s
  `CALL_TIMEOUT` for tool calls is unchanged).
- The error text is the customer's diagnostic: spawn failure ("command not
  found" — PATH under systemd is minimal), handshake timeout, protocol
  error. First line in the row, full chain on hover.
- The UI runs a probe on a detached thread with `mpsc` +
  `request_repaint`, mirroring `ModelDownload` (`onboarding.rs:24-40`). One
  in flight per server; the row shows a spinner meanwhile.
- Result cached in meta `mcp_probe:<name>` as JSON
  `{ok, ts, version, tools, error}` so the row reads
  "connected · 42 tools · mcp-atlassian 0.11 · checked 14:02" across
  restarts. Never auto-probed — spawning servers costs seconds and can burn
  API quota; an unprobed server says "not tested".
- Passive status per server: newest `ai_jobs` row of kind `fetch_context`
  → "last context fetch ok 13:40" / "failed 13:40: <error>"
  (new `storage::latest_ai_job(conn, kind)`). The allowlisted calls are what
  actually run, so this is the truer signal; the probe is the on-demand one.
- `chronicle mcp-check` prints one probe line per server before the existing
  context dump (same function) — headless verification under the daemon's
  real PATH/env.
- Secrets: env values are never logged, never written to meta; the probe
  passes them to the child only.

### Connections section (new, first section)

- Server rows on `ListRow` (`theme.rs:556`): dot = GREEN ok / AMBER untested
  or disabled / RED failed; title = server name; chip "N tools"; trailing
  `test` · `edit` · `×`. Second line weak: command + args, account hint,
  last-fetch line.
- Account hint = value of the first env key matching `USER|USERNAME|EMAIL`
  ("as james.clarke@…"). Display-only heuristic; presets set the key so it
  lands.
- Edit/add form (inline card under the row): name, command, args (one per
  line), env (`KEY=VALUE` per line; values whose key matches
  `TOKEN|SECRET|PASSWORD|KEY` render with `TextEdit::password`), enabled
  checkbox, and a read-only calls summary ("1 context call · 1 fetch call —
  edit mcp.toml for allowlists"). Allowlist editing stays file-only this
  milestone; presets cover the common case.
- "add" opens a presets menu:
  - **Jira (mcp-atlassian)** — command `uvx` resolved to an absolute path via
    PATH at add time (the daemon's PATH may lack `~/.local/bin`; James's
    file already stores the absolute path), args `["mcp-atlassian"]`, env
    `JIRA_URL` / `JIRA_USERNAME` / `JIRA_API_TOKEN` blank,
    `fetch_calls` = `jira_get_issue {"issue_key":"{ref}","comment_limit":10}`,
    `context_calls` = `jira_search` (assignee = currentUser(), updated ≥
    -3d, limit 5) — i.e. the live file, minus values.
  - **Custom** — empty form.
  Presets are one table in `ui/connections.rs`; GitHub/calendar servers join
  it in their own milestones.
- Remove = `×` with confirm-click (chat-history pattern, 7480016). Removing
  a server also drops its calls, otherwise the next load fails with
  `UnknownServer`.
- Form submit writes mcp.toml immediately (live apply), independent of the
  config.toml `save` button. The save status line says "saved" or
  "saved — restart daemon to apply" depending on which file changed:
  git repos and numbers need the restart, MCP doesn't.

### Git repos (inside Connections)

- Rows from `config.git_repos` as written (`~` kept). Title = basename,
  path weak. Status from two sources: resolve
  (`chronicle_capture::git::resolve_git_dir` made `pub`, after
  `expand_home`) → RED "not a git repo"; DB:
  `storage::latest_vcs_event_per_repo(conn)` (`MAX(id) GROUP BY repo`, the
  `branch_state_before` shape, `storage.rs:146`) → "main · commit 13:11" /
  "no activity yet". Trailing `×`.
- Add = text field + button: expand `~`, must resolve; a duplicate basename
  is rejected with the reason (poller and `vcs_events` key repos by
  basename, `git.rs:43-46` — a real limitation, surfaced instead of silently
  merging two repos).
- No native directory picker: nothing in the tree provides one and the X11
  sandbox loop can't drive GTK dialogs. Text entry + validation is the UX;
  auto-discovery from window titles is roadmap.
- `expand_home` moves from `main.rs:1932-1938` to
  `chronicle_core::config::expand_home(&str) -> PathBuf`; daemon and UI
  share it.
- Applies on daemon restart (provider built once, no stop signal). Live
  reload is roadmap.

### Rest of the m18 layout (optional chunk, cut if it drags)

- Capture: `distraction_patterns` (regex lines, `regex_lines` validator).
- Standup & journal (new section): `checkpoint_afk_secs` (0 = off);
  `task_autoclose_days` moves here from Derivation.
- Window & appearance: autohide toggle as meta `ui_autohide` (replaces
  env-only `CHRONICLE_UI_AUTOHIDE`; env still wins when set); "reset window
  position" (clears `ui_window_pos`).
- Section order: Connections, Model, Capture, Derivation, Standup & journal,
  Storage & server, Window & appearance. `section()` style unchanged.

## Build order (each chunk reviewable + committed on its own)

1. **mcp crate**: `Serialize` derives, `enabled`, `save()` (0600, atomic),
   `probe_server` + `ServerProbe`, disabled-skip in `run_blocking`,
   `Config::mcp_path`; `chronicle mcp-check` prints probes. Tests: TOML
   round-trip with `enabled` defaulting, disabled server skipped, save keeps
   both allowlists, saved file mode 0600.
2. **core/capture**: `expand_home`, `latest_vcs_event_per_repo`,
   `latest_ai_job`, `resolve_git_dir` pub; `main.rs` uses `expand_home`.
   Tests: expand_home (`~/x`, absolute, bare); latest-per-repo over 3
   events / 2 repos; latest_ai_job picks the newest of its kind.
3. **Connections UI**: new `crates/app/src/ui/connections.rs` (server rows,
   edit form, presets, probe thread, git rows + add); `settings.rs`
   restructure (panel holds `McpConfig` + path + per-file change tracking
   for the restart hint); meta cache read/write. Visual loop at zoom
   0.9 / 1.0 / 1.15 per `chronicle-ui-visual-loop.md`; probe exercised
   against the live jira server from a sandbox copy of the data dir.
4. **Rest of the m18 layout** (optional): widgets, section reorder,
   `ui_autohide`, position reset.
5. **Docs**: README §Settings (line 157: "MCP config path" → connections)
   and §MCP context (the UI edits mcp.toml; comments lost on save);
   progress.md entry.

Files: `mcp/config.rs`, `mcp/lib.rs`, `core/config.rs`, `core/storage.rs`,
`capture/git.rs`, `app/main.rs`, `app/ui/settings.rs`,
`app/ui/connections.rs` (new), `app/ui/mod.rs` (module + autohide boot),
`README.md`.

## Acceptance

- Fresh data dir: Connections shows "no servers" + add menu. Add the Jira
  preset, fill env, test → "connected · N tools · mcp-atlassian x.y";
  mcp.toml written 0600; `chronicle mcp-check` prints the same verdict.
- James's existing file loads unchanged; test passes; edit + save
  round-trips both allowlists (formatting/comments aside).
- Wrong command → RED row whose first line names the command; disabled
  server → AMBER "disabled" and its calls are skipped (fetch job for a
  ticket still completes as "no context" rather than erroring).
- Git: the 6 repos list with last event; `~/nope` rejected; a second
  `chronicle` basename rejected with the reason; remove one → save →
  restart hint; MCP-only edits → no restart hint.
- Tests green, clippy clean.

## Roadmap (not in this milestone)

- m18 steps 4–6: calendar MCP (`calendar_events`, timeline meeting blocks,
  standup "in meetings 2h"), GitHub MCP (PR/review evidence), scheduled
  standup + Slack post.
- Allowlist editor in the UI once a second preset needs it.
- Live reload of git repos: stop signal on `GitProvider`, daemon compares
  config.toml mtime in the 60 s refresh pass.
- Import from `claude_desktop_config.json` / `.mcp.json` (`mcpServers` JSON
  shape) — customers already have these.
- Git repo auto-discovery from window titles (m20 roadmap).
- HTTP/streamable transports (rmcp has them; `ServerConfig` gains `url`).
- Onboarding card for connections, once presets exist.

## Out of scope

- Write-back actions (Jira worklog/comment) — post-m16 candidate #2, own
  milestone with its own consent UX.
- Ports, packaging.
