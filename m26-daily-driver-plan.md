# M26 — Daily driver: right for James first

Status: **planned, approved in outline (2026-09-03), no code.** Written so
a fresh context can plan and build from this file alone. Repo conventions:
one chunk per commit, tests + clippy per chunk, Conventional Commits
single line, stop the systemd unit before `cargo install --locked`
(memory: `chronicle-no-rebuild-under-daemon`), visual pass via the
standalone UI loop (memory: `chronicle-ui-visual-loop`).

## Decision and context

James (Sep 3): not a business yet. Stay on the dev-teams frame but start
with one user; make Chronicle a perfect daily driver for James, user-test
for a while, then decide what changes. `teams-direction.md` and
`monetization-direction.md` are parked. Nothing here needs a server, an
account or a licence.

James's day (live config, 2026-09-03): six repos (`chronicle`, `contoso`,
`mailer`, `acme-ai-agent-backend`, `fabrikam-web`, `portfolio`), ACME
tickets via the Jira MCP allowlist, PRs via `gh`, Claude Code sessions in
Terminator, vim, Firefox, mic calls, Google Calendar (primary calendar at
Acme), atuin shell history. Output is read; posting is an option.

Decisions taken (James, 2026-09-03):

- Order: chunks 1 → 2 → 3 → 4 → 5 → 6 → 7 as below.
- Chunk 1 "very tight" means **spacing**, not type size.
- Calendar: **one calendar, the primary**.
- Posting: **Jira comment first**, Slack later.
- M8 closed. Tasks 60/76 inspected: real closed tasks, `external_ref`
  already cleared by 98e2cf1, journal and checkpoint read correctly;
  nothing deleted.
- Codex/OpenCode not installed here; not for James, later for others.

## How the feed works today (James asked; verified in m24 plan + code)

1. Capture writes focus events; the sessionizer merges consecutive events
   with the same app and similar title into **spans** and closes them on
   AFK ≥ 120 s.
2. Spans with no covering interval fold into **blocks** when the gap
   between them is under 5 min (`unassigned_runs`).
3. Every 60 s (`prepass_secs`) the daemon **pre-pass** runs four rules in
   order on blocks in the unbatched tail, no model: (a) checked-out
   branch carries a ticket key → the task with that `external_ref`;
   (b) the block's repo matches an open task's project; (c) the block's
   titles match past corrections via FTS (`suggest_correction`, IDF
   pruned); (d) otherwise unmatched. A hit writes a **provisional
   interval** (`intervals.source = 'prepass'`, confidence 0.5). The feed
   row reads "placed by a rule · repo chronicle".
4. Unmatched blocks that share a rare title token or a repo cluster into a
   **proposal** once the cluster passes 10 min of focus; a low-priority
   `suggest_task` job names it. Accept = declare the task and claim the
   blocks; "not a task" parks the cluster for the day.
5. Every 30 min (`batch_minutes`) a batch closes; after 5 min idle
   (`derive_idle_secs`) the model derives it. The digest carries the
   provisional hints (`## Pre-pass hints`, `crates/core/src/digest.rs:296`)
   and ejects as `"X" ✗ "task"` lines. The model's intervals replace
   provisional ones ("placed by the model · 86%"). User rows are never
   touched by derive.
6. Clicks: **keep** turns a provisional row into a user row plus an
   `assign` correction; **move** reassigns; **eject** splits the block
   out of the interval and stores a hard negative for the pre-pass and a
   soft few-shot line for the model.
7. Provisional time counts in reports like any interval.

So: blocks are placed twice, instantly by rules and ~35 min later by the
model, and a task (which carries the project) is the target, not a
project directly.

## Anchors (file:line, 2026-09-03)

- Home: `crates/app/src/ui/home.rs` — `resume_card_ui` :14,
  `standup_card_ui` :72 (`standup_open` session flag :93), `home_ui` :141,
  `OpenRow` → `ListRow` builder :454, `proposal_card` :468, `feed_row`
  :580, `feed_section_ui` :771, `standup_body_ui` :931. 1043 lines.
- Theme: `crates/app/src/ui/theme.rs` — `WIDE_W` = 720 :74, `hover_card`
  :487, `ListRow` :700 (`lines(2)` wrapping from m25), `toggle`.
- Timeline detail pane: `crates/app/src/ui/timeline.rs` `detail_ui` :1002.
- Server routes: `crates/server/src/lib.rs` `Router::new()` :110–122,
  AW `heartbeat` handler :268.
- Activity: `ActivityEvent` `crates/core/src/types.rs:25`, `ActivityKind`
  :43 (checkout, commit, ai_session, pr_authored, pr_reviewed, call; span
  kinds upsert `end_ts` on `(kind, ext_id)` :105).
  `insert_activity_event` `crates/core/src/storage.rs:94`,
  `activity_in_range` :165, `activity_in_range_by_task` :248,
  `set_meta`/`get_meta` :669/:685. Digest `## Activity` section
  `crates/core/src/digest.rs:178`, `activity_line` :363.
- Collectors: `crates/capture/src/{git,github,ai_sessions,mic}.rs`;
  spawned in `crates/app/src/main.rs` `spawn_mic_capture` :1958,
  `spawn_git_capture` :1983, `spawn_ai_sessions_capture` :2010,
  `spawn_github_capture` :2037. `mic.rs` (160 lines) is the template for a
  span-kind collector (open on start, upsert `end_ts` on close).
- Config: `crates/core/src/config.rs` `Config` :17 (`git_repos` :58,
  `ai_session_dirs` :62, `github_prs` :65, `mic_capture` :68,
  `ticket_regex` :71, `checkpoint_afk_secs` :74); defaults :113–117.
  Settings UI: `crates/app/src/ui/settings.rs`, Connections
  `crates/app/src/ui/connections.rs` (Local sources switches, preset table
  :45–79).
- MCP: `crates/mcp/src/config.rs` `McpConfig` :32 (`context_calls` :36,
  `fetch_calls` :41, `ContextCall` :61); `crates/mcp/src/lib.rs`
  `fetch_context` :65, `probe_server` :197. File of record
  `~/.local/share/chronicle/mcp.toml`, mode 0600, loaded per call.
- Standup: `crates/app/src/main.rs` `standup_digest_text` :896,
  `standup_activity_fallback` :935, `maybe_enqueue_standup` :1639.
  Prompts in `prompts/` (`standup_v1.txt`, `journal_v1.txt`,
  `checkpoint_v1.txt`, `derive_v3.txt`, `narrative_v1.txt`).
- Migrations: `crates/core/migrations/` 001–013; next is 014.

## Chunk 1 — Home: the task list is the page (spacing)

Goal: Working on is the first and biggest thing on Home, with room to
breathe; the standup card stops being the first screen; the feed explains
itself.

- Reorder `home_ui` (:141): resume card (when due) → **Working on** →
  proposals → feed → standup card. At `WIDE_W` the feed and proposals move
  to the right column (already exists via `feed_section_ui`), Working on
  keeps the left column full height.
- Working-on rows (:454): row height up (target ~2× current), 8–10 pt
  vertical padding between rows, a 3 pt project colour bar on the left
  edge (palette from m25 `project_order`), label at the card-title size,
  second line = time today · last touched · anchor chip (ticket / branch /
  PR) · newest checkpoint next-step sentence clipped to one line with
  full text on hover. Sorted by time today; the intent task (chunk 5)
  pinned first once it exists.
- Standup card: after it has been opened once in the session, collapse to
  one line "Standup · N tasks · read" (reuse `standup_open` :93; persist
  read state in meta `standup_read:<date>` so a reopen stays collapsed).
  Expands in place.
- Feed header gets a `?` hover (`hover_card` :487) with the four placement
  rules, one sentence each, and the two timers (60 s rules, ~35 min
  model).
- Spacing knob: Settings › Window & appearance "row spacing:
  comfortable / compact" → meta `ui_density`; one constant read by the
  Home rows and feed rows. Comfortable is the default. Gives the visual
  pass a fallback instead of a rewrite.
- Acceptance: at 400×640 the first screen shows the resume card (if any)
  and at least three Working-on rows with nothing truncated; standup is
  reachable in one click; at 900 pt wide the feed sits beside the list.
  Before/after screenshots at both sizes, James signs off.
- Tests: none new beyond row-model unit tests if the row builder gains
  logic (time-today formatting, next-step clipping).

## Chunk 2 — Google Calendar collector (primary calendar)

Goal: meetings on the timeline and in the digest; AFK gaps explained;
pairs with mic `call` spans.

- New `crates/capture/src/gcal.rs`, spawned from `main.rs` next to
  `spawn_mic_capture` (:1958). HTTP via `ureq` (already a dependency of
  `derive`; add to `capture`).
- Auth: OAuth 2.0 desktop flow, loopback redirect on `127.0.0.1:<port>`,
  scope `calendar.events.readonly`. `chronicle gcal-login` subcommand
  opens the browser, receives the code, exchanges it, writes
  `~/.local/share/chronicle/google.toml` (mode 0600: client_id,
  client_secret, refresh_token). James creates the OAuth client in the
  Acme Google Cloud project as an **internal** Workspace app, so
  refresh tokens do not expire after 7 days as they do for "testing"
  apps.
- Poll: every 5 min, `GET calendars/primary/events?timeMin=<today-1d>&
  timeMax=<today+1d>&singleEvents=true&orderBy=startTime`. Skip all-day
  events and events James declined (`attendees[self].responseStatus ==
  declined`). Emit `ActivityEvent { kind: Meeting, ts: start, end_ts: end,
  ext_id: event id, summary: title, repo: "" }`. Upsert on `(kind,
  ext_id)` like `Call`; a deleted/moved event updates on the next poll
  (`updated` field; treat `status == cancelled` as delete → tombstone by
  setting `end_ts = ts`).
- Storage: add `Meeting` to `ActivityKind` (`types.rs:43`, string
  `"meeting"`, `Dedupe::Upsert` at :105). No migration: `kind` is text.
- UI: activity row glyph (Phosphor calendar glyph added to the subset, see
  m22 for the regeneration steps), hover names the kind. Digest `##
  Activity` line via the shared `activity_line` (:363) unchanged.
- Config: `google_calendar: bool` (default false) in `Config` :17,
  a Local sources switch in Connections, and a "sign in" button that runs
  the login flow and shows the account email from the token response.
- Follow-up (not this chunk): a `meeting` overlapping a `call` span
  collapses to one timeline row named by the meeting.
- Acceptance: a meeting on today's primary calendar shows as a span row
  on the timeline within 5 min of daemon start; an event deleted in
  Google disappears on the next poll; the digest for a batch containing a
  meeting has the `## Activity` line.
- Tests: JSON fixture of an `events.list` response → expected
  `ActivityEvent`s (skips all-day, declined, cancelled); token refresh
  path with a fake HTTP server or a trait around the client.

## Chunk 3 — Editor heartbeats (Wakapi-compatible)

Goal: editor time placed by project without title heuristics; vim for
James, any editor for anyone (VS Code, Sublime, JetBrains, Emacs and 60
more plugins speak the same protocol and queue offline).

- Routes in `crates/server/src/lib.rs` beside :110–122:
  `POST /api/v1/users/current/heartbeats.bulk` and `POST /api/heartbeat`
  (single). Header `Authorization: Basic base64(api_key)` (no colon); the
  key is generated once into meta `wakapi_api_key` and shown in Settings ›
  Connections with a copy button. Body fields used: `entity`, `type`
  (file/app/domain), `project`, `branch`, `language`, `time` (float
  seconds), `is_write`. Reply `201 {"responses":[[{...},201],…]}`; 401 on a
  bad key. Host-header validation as the AW routes (README non-functional
  requirements).
- Folding: `crates/core/src/heartbeats.rs` — heartbeats fold into
  `activity_events` kind `edit` spans per `(project, branch)` with a
  15-minute gap rule (WakaTime's own "durations" rule), `summary` =
  current file basename, `repo` = project name, `ext_id` =
  `<project>@<branch>#<span-start-ms>`. Upsert `end_ts` on every heartbeat
  inside the gap. Pre-pass rule (b) (repo match) then places editor
  blocks.
- James's side: `api_url = http://127.0.0.1:5600/api` and `api_key` in
  `~/.wakatime.cfg`; `vim-wakatime` plugin. Document both in README under
  Local sources.
- Acceptance: with vim-wakatime installed, editing in `~/dev/contoso`
  produces an `edit` span with repo `contoso` that grows while editing and
  closes 15 min after the last heartbeat; a block of Terminator focus in
  that window is placed under the contoso task by the pre-pass.
- Tests: bulk parse of the documented payload; folding across the gap
  boundary; auth failure.

## Chunk 4 — Shell history (atuin)

Goal: what the terminal time was, without storing commands.

- `crates/capture/src/shell.rs`: read-only `rusqlite` connection to
  `~/.local/share/atuin/history.db` (WAL by default, safe to read while
  atuin writes), `PRAGMA query_only`. Poll every 60 s for rows with
  `timestamp > last_seen`. Store only `cwd`, `argv[0]`, `exit`,
  `duration`; never the full command line (secrets).
- Fold per (repo derived from `cwd` against `git_repos`, 10-minute gap)
  into `activity_events` kind `shell` spans; `summary` = top three
  `argv[0]` by count ("cargo ×12 · git ×5 · rtk ×3"), `repo` = matched
  repo basename or "" for outside-repo work, `ext_id` =
  `<cwd>#<span-start-ms>`.
- Config: `shell_history: bool` (default false) + Local sources switch.
  Fallback parsers for zsh `EXTENDED_HISTORY` / bash `HISTTIMEFORMAT` are
  out of scope here.
- Acceptance: a burst of commands in `~/dev/chronicle` yields one `shell`
  span with repo `chronicle`; commands outside any repo yield a span with
  no repo and are not used for placement.
- Tests: folding + summary from a fixture history table.

## Chunk 5 — Intent: morning and evening

Goal: the single-player half of the alignment frame (`teams-direction.md`
m26): what James meant to do, whether the day went there, what is stuck.

- **Morning:** Home shows "Today I'm on …" above Working on when no intent
  is set for today: a picker over open tasks plus the ACME issues assigned
  to James (parse the newest Jira `fetch_context` result in `ai_jobs`; if
  none, tasks only) and a free-text line. Stored in meta `intent:<date>`
  as JSON `{task_ids: [...], text: "..."}`. Intent tasks pin first in
  Working on with an "intent" chip.
- **Evening:** `build_digest` (:36) gets a `## Plan` line from the day's
  intent; `standup_v1.txt` and `narrative_v1.txt` get one rule: when a
  plan is present and the top task by time is not in it, write one
  sentence "planned X, spent most of the day on Y" and nothing else about
  it. Fixture + expectation before the prompt change (bench convention).
- **Stuck:** task open, no interval in `task_stuck_days` (config, default
  3), and its newest checkpoint's `next_steps` unchanged since the last
  interval → `stuck` chip on the Working-on row and in the timeline detail
  pane. No score, no alert, no colour beyond the chip.
- **Resume:** `resume_card_ui` (:14) prefers the intent task's checkpoint
  when several qualify.
- Acceptance: set an intent, work on something else all day: the evening
  standup draft carries the drift sentence; leave a task idle for three
  days: it shows `stuck`; both chips disappear when the condition clears.
- Tests: intent JSON round-trip; stuck predicate over synthetic intervals
  and checkpoints; digest `## Plan` line.

## Chunk 6 — Post through connectors: Jira comment first

Goal: still read-only by default; one explicit way to push a journal
entry or checkpoint into the ACME ticket.

- `mcp.toml` gains `[[action_calls]]` beside `context_calls` /
  `fetch_calls` (`McpConfig` :32): `server`, `tool`, `args` template with
  `{key}` and `{body}`, `label`. Jira preset: `jira_add_comment`
  (mcp-atlassian) with `issue_key = {key}`, `comment = {body}`.
- Task detail pane (`timeline.rs` `detail_ui` :1002): when the task has an
  `external_ref` and an action call exists, a "comment on ACME-…" button
  on each journal entry and on the checkpoint. Opens a confirm dialog
  showing the exact tool, key and body (editable), "post" runs the call
  on a thread via the existing MCP client, result line in the pane
  (`posted · 12:04` or the error). Never scheduled, never batched.
- Connections: action calls listed under their own header with the
  caption "posts only when you click"; Settings › Storage gains a "what
  leaves this machine" line listing model download, MCP context fetches
  (count today) and posts (count today).
- Slack standup post: same mechanism later with
  `korotovsky/slack-mcp-server` `conversations_add_message`; not in this
  chunk.
- Acceptance: comment a journal entry onto a real ACME ticket from the
  UI; the ticket shows it; nothing is posted without the dialog.
- Tests: template substitution; action calls never appear in
  `fetch_context`/`context_calls` paths.

## Chunk 7 — Housekeeping and soak

- Click-test with James at the mouse (never done): Connections `test` /
  remove / repo add; chat history × delete.
- One real day of feed soak after chunk 1; every wrong grouping becomes a
  fixture + expectation before it is fixed.
- README: Local sources section lists calendar, editor heartbeats, shell
  history with the config keys and the three setup steps each.

## Out of scope

Ports, sync, accounts, licence, teams UI, prompt changes beyond the plan
line, light theme, tray, Codex/OpenCode collectors (same shape as
`ai_sessions.rs` when wanted: `~/.codex/sessions/**/*.jsonl`).

## Open for James (answer when they come up, not blocking)

- Chunk 2: should the calendar account email show on the Connections row?
  (Assume yes.)
- Chunk 5: intent picker offers Jira issues only when the newest fetch is
  under 24 h old. (Assume yes.)
- Chunk 6: Jira comment body = journal entry verbatim, or the entry plus a
  "via Chronicle" trailer? (Assume verbatim, no trailer.)
