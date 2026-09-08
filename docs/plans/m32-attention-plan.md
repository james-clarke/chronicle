# m32 — Attention, not input

Status: **chunks 0, 1 and 2 shipped 2026-09-08** (main, see "Shipped" at
the end); chunks 3–6 are the plan as drafted 2026-09-08, awaiting approval. Written
from a review of last week's live data (2026-08-31 → 09-04) and the chunk
2.5 gate re-run. Hand this file to a fresh context and start at "Handoff".

## The short version

Last week's log is right about the big blocks and wrong in the ways that
matter for a timesheet: about a quarter of captured time is misfiled or
missing, and every miss traces to one of five mechanical causes, none of
them "the model is dumb". On top of that sits a principle James stated
and the data confirms: most of a working day is reading, planning and
watching agents work, with no keystrokes. Chronicle treats 120 s without
input as *away*, and last week that cut 75 daytime gaps (363 min, 12 % of
focus time) out of the record, mostly right after a Claude session or the
Chronicle window itself. The quiet part of the day is the part the tool
is for.

m32 delivers, in order: capture that never lies about presence or gaps;
tool-session evidence that says who James was talking to; a split for the
hours where three agents run at once; anchor hygiene so a bare `staging`
or a Harry Potter tab cannot win a task; narratives built from sources
that cannot hallucinate; and a daily self-score so accuracy is watched by
the app, not by a bench run.

## What last week says

Live DB, derived rows vs git history, transcripts and `.remember` notes.

| day | focus captured | placed | what went wrong |
|---|---|---|---|
| 09-01 | 21h04m | 4h55m | a 15h37m "✳ Claude Code" span 16:51 → 08:28 |
| 09-02 | 11h00m | 11h07m | best day; 13 manual assigns |
| 09-03 | 8h54m | 9h14m | 6h22m to ACME-11382, ~2h of it chronicle |
| 09-04 | 7h26m | 4h52m | 15:30–18:20 never derived until today |

Five causes, each with the evidence that names it:

1. **Capture integrity.** The X server died at 18:25 on 09-04; the focus
   thread logs and exits (`crates/app/src/capture.rs:27`,
   `crates/capture/src/x11.rs:302`), the open span stays open, no AFK
   events follow. Same path made the 15h37m span on 09-01. Batch 93 was
   mid-derive at the shutdown 10 s later and sat `failed`; 94–98 sat
   `pending` over the weekend. The report showed Thursday as 4h52m with no
   hint 3h was missing (a 90-minute Meet, the sprog.io session, RDS work).
   The screen-lock signal the README promises (`LockSignal`, logind
   `LockedHint`) is not implemented.
2. **Whole-batch winner-takes-all under parallel agents.** 09-03
   15:00–16:44 was four Claude sessions (chronicle, contoso, mailer, the
   dotfiles audit) with chronicle commits at 15:31, 16:06, 16:22, 16:35.
   Anchors split about half and half (item ACME-11382 ×119 vs place
   chronicle ×86). The model tier gave all 104 minutes to mailer. Same
   at 07:36–08:25: 49 of 55 minutes were the "App monetization strategy"
   session in chronicle; the model chose the recent open task.
3. **Distinctive noise names the task.** `harrypotter.com — updating hero
   loop and layout for Harry Potter Sorting Experience site` is a
   5-minute sorting-hat tab fused with the chronicle "Webpage update"
   session in a window whose anchors were place=chronicle ×13.
   `northwind-memberships invoice reminder` came from a 0.6-minute Jira tab
   plus a stale `pr_authored` row for PR #8506 that the `gh` poll
   re-listed (`crates/capture/src/github.rs:41` searches anything updated
   in 24 h). The segmenter's first hour today did the same: task 119
   `Game Room · Any free party website games…` from a reddit tab over a
   22-minute sprog.io terminal.
4. **Catch-all tasks, label and ref pollution.** 09:05–13:52 on 09-04
   (m30 chunks 2–6, m31 chunks 0–1, the 10441 epic session) is one task
   labelled with a raw session title including the ✳ glyph, project
   chronicle, external_ref ACME-11385. `m30 dev` carries ACME-11382,
   `fix derivation/task accuracy` carries ACME-11381. Refs come from tabs
   open in parallel (`crates/core/src/anchor.rs:26` picks by majority
   branch across the window), not from the task's own repo. The glyph
   strip (`crates/core/src/evidence.rs:16`) runs at display
   (`crates/app/src/ui/theme.rs:417`) but not where labels are written.
   Today the segmenter placed 17:20–17:59 on Thursday and 10:30–11:12
   today on the brand-new `fabrikam-web` task: its only evidence is bare
   `staging` and `main` branch anchors plus place=mailer/chronicle it
   absorbed *after* winning. A fresh user task with no declared evidence
   scores on generic anchors, wins, and then learns the wrong profile.
5. **Narrative jobs starve.** 12 `task_description` failures are `no span
   evidence for task N` (`crates/app/src/ai_job.rs:348`) for hand-made
   tasks; 11 checkpoint and 8 journal failures; the 09-02 standup ends
   "next step is unclear based on the journal"; the 09-01 one invents a
   UX team.

And the quiet-time numbers (08:00–19:00, since 08-31):

| daytime AFK gap | count | minutes |
|---|---|---|
| 2–5 min | 45 | 132 |
| 5–15 min | 30 | 231 |
| 15–30 min | 6 | 107 |
| 30–60 min | 1 | 31 |

What was on screen when a 2–15 minute gap opened: Chronicle 41 min, the
"ACME-10441 work organization" session 35, bare "✳ Claude Code" 26, the
"flow dev ACME-10770" session 20, a chronicle shell 14, a Slack DM 13.
That is reading the app's own output, watching an agent, and reading a
message: attention, cut as absence. Zero `call` rows overlap an AFK span
only because the Meet tab kept the pointer busy.

### Gate re-run (2026-09-08, chunk 2.5 schedule)

`backfill-anchors --since 2026-09-04` (428 spans), `backfill-evidence`
(64 rows), then `bench --replay --scorer` on a sandbox copy of the live
DB:

| window | probes | scorer | with `--segment` |
|---|---|---|---|
| 4 days | any | no scorable corrections | – |
| 5 days | direct | 1/2 | 1/2 |
| 7 days | direct | 7/19 strict, 10/19 lenient | 6/19 |
| 7 days | all | 39/132 strict, 49 lenient, 26/70 unique | 37/132 |

Flat against chunk 2.5 (7/19, 39/137). Twelve of the fifteen `unsure`
direct probes are margins ≤ 0.18 between the two catch-alls (task 60
"user experience review and tweaks" and 73 "11342 storing gif/img");
their profiles overlap because both hold everything. The gate stays
unmet, and the reason is identity and evidence quality, not the scorer's
arithmetic. Clean cwd data exists only for 09-04 and today (the machine
was off over the weekend).

`derive_mode = "segmenter"` went live at 11:11 today. Batches 93–99
reconciled at zero tokens on restart; the Meet call became its own task
(kind `read`, not `meet`: `span_kind` at `crates/core/src/segmenter.rs:414`
only sees calendar `Event` anchors and meeting apps, never the `call`
rows from the mic collector). `verdict_log` holds its first 9 rows.

## Principle: attention ≠ input

The design rule for every chunk below.

- **Presence has three states, not two.** *Active*: input in the last
  couple of minutes. *Quiet*: screen unlocked, no input, and a live
  context (an agent session writing to its transcript, a call, a document
  or page on screen). *Away*: locked, lid closed, or quiet with no live
  context for longer than `away_secs`. Quiet time belongs to the open
  segment. Only away closes it.
- **Quiet has a kind, and the kind is work.** `supervise` (an agent
  session attached to the window is writing), `meet` (a call row
  overlaps), `read` (document, PR, page), `plan` (tracker item, plan
  document). Reports show hands-on and hands-off side by side and sum to
  wall time. There is no productivity score, no "active %" tile, nothing
  that makes a quiet hour look worse than a typing hour.
- **Presence signals never include content.** Per-minute counts of
  keys, buttons, motion and scroll from XI2 raw events; window focus
  changes; a transcript user record; mic or camera in use; the lock
  state. Never a key code, never a character, never a screenshot.
- **The retroactive rule.** If input resumes on the same window (or the
  same segment's anchors) within `away_secs`, the gap was quiet. The
  sessionizer can only know this after the fact, so quiet is a marker on
  the open span, not a cut.
- **Agents count as context, not as the person.** Transcript *assistant*
  writes prove the agent is busy; only *user* records prove James was
  there. Attach and split by prompts, extend by writes.

## Design

### 1. Capture ledger and span integrity

A `daemon_runs` table (start, end, end reason: shutdown, provider exit,
lock, crash) written on start, on every clean stop, and on provider exit.
Any span open when the focus provider dies is closed at the last event
plus `afk_close_secs`; the same clamp runs at startup over any span that
outlives its last event. Reports and status print the gaps ("no capture
Thu 18:25 → Mon 10:30", "2h50m captured, not yet derived") instead of
shrinking the day. Glyphs are stripped where labels are written, not only
where they are shown.

### 2. Presence model

`afk_loop` keeps XScreenSaver idle as the input clock and adds XI2 raw
event counts per minute into a `presence` table (minute, keys, buttons,
motion, scroll; counts only). Lock state comes from logind `LockedHint`
over the session D-Bus (the `LockSignal` trait exists on paper). The
sessionizer stops closing on `idle=true`; it marks the span quiet and
closes on away. `away_secs` default 30 min with a live context, 10 min
without. `span_kind` gains `supervise` and consults call rows for
`meet`. Reports render the hands-on / hands-off split.

### 3. Tool-session evidence v2

The collector keeps a second minute list, `prompt_minutes`, for user
records that are not tool results, and parses `summary` records into a
session `title` (the text Claude puts in the terminal title, glyph
aside). `gitBranch` becomes a repo-qualified branch anchor. Attachment
order in `extract::from_activity`: title match, then nearest prompt, then
nearest write. A bare terminal with no session and no cwd row stays
unattached rather than borrowing the last one.

### 4. Concurrency = supervision split

When two or more sessions wrote within the last 2 minutes of a segment,
its agent/supervise minutes split across their tasks in proportion to
prompt counts (focus share as fallback). Interval rows carry a `share`
so reports still sum to wall time. The timeline gets an agents lane: one
bar per live session, prompts as ticks, focus as the highlight.

### 5. Anchor hygiene

Branch anchors are emitted as `repo@branch`; bare `main`/`staging` never
match across repos. URLs are parsed by structure: `github.com/<org>/<repo>`
is a strong place, `atlassian.net/browse/<KEY>` an item,
`meet.google.com/<code>` an event, `localhost:<port>` a place via a
port→repo map built from `/proc/net/tcp` listeners and their cwd. PR rows
count only when the head branch was checked out or in a cwd that day.
`external_ref` attaches only from the task's own place (its branch,
commit or prompt), never from a tab. A label is built from the majority
anchor cluster; a doc or domain under 25 % of a segment cannot name it.
A user task with no declared or kept evidence is not scoreable until it
has some; placements it wins while `unsure` do not feed its profile.

### 6. Narrative from ground truth

Descriptions, journal and standup draw first from sources that cannot be
wrong: commit subjects, session prompts and the session's final assistant
message, and the per-repo `.remember/today-*.md` entries (a new `note`
activity kind read from every `git_repos` entry). A task with no span
evidence is described from its activity rows or skipped, never failed. A
claim in the standup carries the source it came from.

### 7. Self-score

A daily job computes coverage, uncaptured and undrived minutes, tasks
minted vs merged within 24 h, eject and rename rates, and confident-wrong
rate from `verdict_log`; Settings › Derivation and `chronicle status`
show the last seven days. `bench --calibrate` stays as the offline check.

## Chunks

Each chunk: files with anchors as of 436d7ae, migration numbers (next is
024), the gate that ends it, non-goals. Order is by how much time each
recovers per line changed.

### Chunk 0 — integrity (small, first)

- `crates/app/src/capture.rs:27`: on focus-provider exit emit
  `Afk { idle: true }` and a `daemon_runs` end row; `crates/app/src/daemon.rs:146`
  shutdown path writes the same. `crates/core/src/sessionizer.rs:165`
  already closes at stream end; add the startup clamp beside
  `reset_stale_running()` at `crates/app/src/daemon.rs:355`: any span
  whose end exceeds its last event by more than `afk_close_secs` is cut
  there.
- Migration 024 `daemon_runs(id, start_ts, end_ts, reason)`.
- `crates/core/src/report.rs:151` and `crates/app/src/status.rs:119`:
  "not captured" and "not yet derived" lines from `daemon_runs` and
  `batches`.
- Strip glyphs at write: `crates/app/src/ai_job.rs:254` (suggest),
  `:384` (name_task), the UI declare path that pre-fills a label from a
  title, and `segmenter.rs:564` placeholder labels.
- Lock signal: logind `LockedHint` via the session bus → `Afk` idle plus
  a `lock` reason; this is the first real `LockSignal` impl.
- Gate: a fixture stream that ends with a dead X connection yields no span
  longer than `afk_close_secs` past its last event; the weekly report for
  09-04 prints the gap line.
- Non-goals: no presence model yet, no kind changes.

### Chunk 1 — presence

- Migration 025 `presence(minute_ts, keys, buttons, motion, scroll)`.
- `crates/app/src/capture.rs:334` `afk_loop` → presence loop: XI2 raw
  events (x11rb `xinput` extension, `XISelectEvents` on the root window
  for `RawKeyPress`, `RawButtonPress`, `RawMotion`), counted per minute,
  never decoded. Idle clock unchanged.
- `crates/core/src/sessionizer.rs:146`: `idle=true` marks the open span
  quiet instead of closing it; close on `Lock`, on idle ≥ `away_secs`
  with no live context, or on idle ≥ 10 min with none. Live context =
  a session attached to the window wrote in the last 5 min, a `call` row
  is open, or the window's family is Meeting. Config keys `away_secs`
  (1800) and `quiet_secs` (600) beside `afk_close_secs`.
- `crates/core/src/segmenter.rs:397` KINDS gains `supervise`; `:414`
  `span_kind`: Terminal with a session → `agent` if the span has prompt
  minutes, `supervise` if quiet and the session wrote; any span
  overlapping a `call` row → `meet`.
- `crates/core/src/report.rs:128` kind mix and `crates/app/src/ui/reports.rs`:
  "hands-on 3h10m · hands-off 4h20m (supervise 2h · read 1h · meet 1h20m)".
- Gate: replay last week's sessionized stream with the new rules: the 75
  daytime gaps under 15 min fold into their spans; a 10-minute read of a
  document on screen continues the span as `read`; a lock closes within
  one poll. Daemon RSS unchanged within noise.
- Non-goals: no webcam, no per-app input stats in the UI, no active
  percentage anywhere.

### Chunk 2 — tool-session evidence v2

- `crates/capture/src/ai_sessions.rs:323` `absorb_detail`: add
  `prompt_minutes` for `user` records whose content is not a
  `tool_result`; `:365` `parse_line` also keeps `summary` records (the
  `summary` field becomes the session title); `:378` `gitBranch` is kept
  on the segment. Detail keys: `prompts`, `paths`, `writes`,
  `prompt_minutes`, `title`, `branch`.
- `crates/core/src/extract.rs:418`: attach a terminal span to the session
  whose title equals the span's stripped title; else the session with the
  nearest prompt at or before the span end; else nearest write. `:430`:
  a bare terminal with no live cwd row stays unattached.
- `crates/core/src/extract.rs:321`: session branch → `repo@branch`.
- `backfill-anchors` re-run for the week.
- Gate: `bench --replay --scorer --since 7 --probes direct` ≥ 10/19
  strict (7 today); the 09-03 15:00–16:44 window attaches every terminal
  span to the session that owned its title.
- Non-goals: other tool families (Cursor, Codex) are table rows later.

### Chunk 3 — concurrency split

- Migration 026 `intervals.share REAL NOT NULL DEFAULT 1.0`.
- `crates/core/src/segmenter.rs:781` `reconcile` and the tail writer:
  when ≥ 2 sessions wrote within 2 min of a segment, emit one row per
  task with `share` by prompt count (focus share as fallback), kinds
  `agent`/`supervise` per row.
- `crates/core/src/report.rs:151` and the UI totals multiply by `share`.
- `crates/app/src/ui/timeline.rs:583` lanes: an agents lane, one bar per
  live session, prompt ticks, focus highlight.
- Gate: the 09-03 15:00–16:44 window replays to chronicle ≥ 35 % and
  contoso+mailer ≤ 60 %; weekly totals still sum to captured time.
- Non-goals: no split for non-agent work; no fractional user rows.

### Chunk 4 — anchor hygiene

- `crates/core/src/extract.rs:321`: repo-qualified branches; URL
  structure rules (github path, atlassian browse, meet code, localhost
  port).
- Port map: beside `crates/capture/src/x11.rs:53` `terminal_cwd`, a
  60-second `/proc/net/tcp` scan mapping listening ports to the owning
  pid's cwd; emitted as `Cwd` rows with `port` in detail.
- `crates/capture/src/github.rs:41`: a PR row is strong only when its
  head branch appears in a `checkout` or `Cwd` row within 24 h; else
  weak.
- `crates/core/src/anchor.rs:26`: `external_ref` only from the task's own
  place (branch, commit, prompt); tabs never set it; existing wrong refs
  on tasks 98, 109, 111 cleared by a one-off.
- `crates/core/src/segmenter.rs:564` and `crates/app/src/ai_job.rs:384`:
  label from the majority cluster; doc/domain anchors under 25 % of the
  segment excluded from naming.
- `crates/core/src/profile.rs:760`: a task with no declared or kept
  evidence is skipped; `unsure` placements do not feed profiles until
  kept or passively accepted (`crates/core/src/storage.rs:3236`).
- Gate: `--probes all` ≥ 45/132; over a week no task minted from an
  anchor under 25 % of its first segment; `fabrikam-web` never wins a
  chronicle terminal again.
- Non-goals: embeddings stay off.

### Chunk 5 — narrative from ground truth

- New `note` activity kind: a collector reads `<repo>/.remember/today-*.md`
  for every `git_repos` entry (timestamp headers → rows, body clipped).
- `crates/app/src/ai_job.rs:348`: describe from activity rows when spans
  are missing; `:424` journal and `:530` standup take commit subjects,
  prompts, the session's last assistant text and `note` rows, and the
  prompt requires a source per claim.
- Gate: the standup for a replayed day carries a source line per bullet
  and no claim without one.
- Non-goals: no cloud model required; the 4B stays the default.

### Chunk 6 — self-score

- Daily job over `daemon_runs`, `batches`, `intervals`, `corrections`,
  `verdict_log`: coverage, uncaptured, undrived, minted vs merged < 24 h,
  eject and rename rates, confident-wrong rate.
- `crates/app/src/ui/settings.rs:614` Derivation section and
  `crates/app/src/status.rs:122`: last seven days.
- Gate: the numbers agree with `bench --calibrate --since 7`.

## Risks

- Quiet counted as work when James walked away without locking: bounded
  by `away_secs` and the retroactive rule; the lock signal is the real
  fix and ships in chunk 0.
- XI2 raw events on X11 only; the README's platform matrix gets a
  presence row (macOS: `CGEventSourceCounterForEventType`, Windows:
  `GetLastInputInfo` plus a low-level hook for counts).
- Fractional rows make CSV timesheets sum in tenths of a minute; round
  at render, never at write.
- The port map reads `/proc/<pid>/cwd` for other users' processes and
  gets `EACCES`; skip silently.
- Presence counts are a new privacy surface: counts only, documented in
  the "leaves the machine" table as "nothing", and a config switch.

## Out of scope

Webcam, keylogging, screenshots, a productivity score, cloud models,
other agent tool families, Wayland.

## Handoff for a new context

State on 2026-09-08 11:16:

- Deployed binary is main at 436d7ae (installed 09-04); live DB at
  migration 023. `derive_mode = "segmenter"` in
  `~/.local/share/chronicle/config.toml` since 11:11 today, daemon
  restarted, healthy; batches 93–99 reconciled; `verdict_log` filling.
- Gate numbers above are from `bench --replay --scorer` on a sandbox copy:
  `XDG_DATA_HOME=<scratch>/sbx` with `chronicle/chronicle.db` copied via
  python's sqlite backup API, `config.toml` copied, `models` symlinked.
  Logs in the session scratchpad as `gate-<since>-<probes>[--segment].log`.
- Backfills already ran on the live DB today (428 spans, 64 evidence
  rows). Re-run both after chunk 2 and chunk 4.
- Start with chunk 0. Read `docs/plans/m30-derivation-v2-plan.md`
  "Shipped" for the segmenter's vocabulary; `AGENTS.md`/`CLAUDE.md` for
  conventions; stop the systemd unit before `cargo install --locked`.
- Verification of chunk 1 needs a real quiet stretch: open a document,
  hands off for 10 minutes, then lock the screen; expect one `read`
  segment and a close within a minute of the lock.

## Shipped (2026-09-08, chunk 0)

Four commits on `main` (1055c55 → 44e6d24), 237 tests (7 new), clippy
clean. Installed 12:18 the same day (unit stopped, `cargo install
--locked`, restarted healthy; live DB at migration 024; the startup clamp
cut spans 2969, 422 and 4405).

- **What the 15h37m span really was.** Not a dead X connection. The events
  around span 2969 (09-01 16:51 → 09-02 08:28) are a logout dialog, then
  nothing, then ordinary title events the next morning with no `afk` row
  in between: the laptop suspended and the AFK poller slept through it.
  `std::thread::sleep` runs on the monotonic clock, which stands still
  through a suspend, and the first keypress after resume reset X's idle
  counter before the next poll saw it. Span 422 (08-26, 629 min) is the
  same shape. So the fix is a wall-clock jump detector, not only the
  provider-exit marker.
- **Ledger** (`1055c55`): migration 024 `daemon_runs(id, start_ts,
  end_ts, reason)`; `open_run` / `close_run` / `close_crashed_runs` (a
  row left open ends at the last event, never before its own start,
  reason `crash`); `runs_for_report` (rows touching a range plus the
  ledger's first start — time before it is unrecorded, not a gap);
  `underived_ms` (non-AFK span time no `done` batch covers).
- **Startup clamp** (`1055c55`): `clamp_quiet_spans` cuts a non-AFK span
  at its last focus/title/afk event plus `afk_close_secs`, but only when
  the quiet stretch before its end exceeds an hour (`QUIET_CLAMP_SECS`).
  Deviation from the design's literal rule ("end exceeds its last event by
  more than `afk_close_secs`"): on the live DB that rule would cut 117
  spans, about 10 h of static-title work (typing in one window emits no
  event, and the poller's silence is the presence evidence). Measured
  quiet stretches top out under 50 min except three nights; the clamp
  cut exactly those three (2969, 422, 4405) on the sandbox copy, to 2 min
  each. Runs every start, idempotent. `url` events do not count as
  presence (extensions report audible tabs).
- **Daemon** (`4e76769`): ledger row opens after the clamp, closes with
  `shutdown` / `provider exit` on exit, `lock` on a lock edge, `sleep`
  when a tick lands more than `SESSIONIZE_EVERY + afk_close_secs` late on
  the wall clock (new row at resume). Shutdown also inserts `afk
  idle=true` so the open span ends with capture. The focus thread sends
  the same marker plus `CtrlMsg::CaptureLost` when the provider exits.
  The AFK poller (`afk_loop`) marks a suspend on its own: a poll more
  than `AFK_POLL + afk_close_secs` late emits `idle=true` backdated to
  the previous poll; the next poll's normal transition closes it.
- **Lock signal** (`4e76769`): `chronicle_capture::LockSignal` trait and
  `lock::LogindLock` on the system bus via zbus 5 (already in the tree
  through ksni; same `tokio` feature set). It resolves the user's display
  session from `user/self` `Display` (the daemon runs under
  `user@.service`, outside any session scope, so `session/auto` would not
  resolve) and reads `Lock`/`Unlock` signals plus `LockedHint` changes.
  No locker on this box sets the hint, so the signals are what fire;
  `loginctl lock-session 1` / `unlock-session 1` exercise it without a
  visible lock. Unavailable bus → warning, capture unaffected.
- **Report and status** (`6b93c85`): `RangeReport` gains `tz`, `gaps`,
  `underived_ms`; `report::capture_gaps` folds ledger rows into gaps of
  at least `AFK_SPLIT_MINS` (a restart's seconds print nothing);
  `to_md` appends `- not captured: Thu 18:25 → Mon 10:30` and
  `- captured, not yet derived: 2h50m`; `chronicle status` prints
  `not captured today` and `captured, not yet derived` (also in
  `--json`). CSV unchanged.
- **Glyphs at write** (`44e6d24`): `suggest_task` output (covers both
  `suggest_task` and `name_task`) and the segmenter's placeholder label
  (both the evidence description and the tie-word fallback) go through
  `evidence::strip_glyphs`. The UI declare pre-fill already did
  (`theme::display_title` at `home.rs:798`), no change there.

Gate: `fixtures/dead_x.jsonl` + `dead_x_ends_span_at_provider_exit`
(no focus span outlives its last event by more than `afk_close_secs`;
the six hours after the marker are one AFK span). Verified live on a
sandbox daemon (`XDG_DATA_HOME` copy, `XDG_RUNTIME_DIR=/tmp/chr-sbx-run`,
port 5601, 100 s under `timeout`): startup cut 3 spans; `loginctl
lock-session 1` → ledger row 1 closed `lock` at 12:12:24 and an AFK span
12:12:24 → 12:12:44; `unlock-session` → row 2; SIGTERM → row 2 closed
`shutdown` at 12:13:39 with the AFK marker. **The gate's second half
("weekly report for 09-04 prints the gap line") cannot come from real
data**: the ledger starts with the first run of this build, and the
09-01 → 09-02 night predates it. The line is pinned by
`md_prints_gap_and_underived_lines`; the first real one appears after the
next suspend, lock or stop of five minutes or more.

Open: batches keep their bounds when a member span is cut (the batch end
is only `t0` for the next refresh); intervals were not touched (none
overlapped the three cut spans). Chunk 1 can read `daemon_runs` for its
presence model.

## Shipped (2026-09-08, chunk 1)

Two commits on `main` (7e9b026 core, de43866 app/capture), 248 tests (13
new), clippy clean. Installed 13:16 the same day; live DB at migration
025, daemon healthy, release RSS 34 MB after two minutes (19.5 MB before
the install, 50 MB budget); a `presence` row landed for injected pointer
motion within the minute, and `chronicle report --week 2026-09-08` prints
`hands-on 1h55m · hands-off 30m00s (read 30m00s)`.

- **Retroactive quiet, not a timer** (`7e9b026`): `sessionize_with` keeps
  the span open on `afk idle=true` and remembers the idle start with a
  snapshot; the next `idle=false`, a lock or the stream end settles it.
  Under the threshold the stretch folds into the span as `quiet_ms`; past
  it everything since rolls back to the snapshot and the span closes at
  the idle start, the AFK span and the resume-in-last-focus exactly as
  before. `refresh` rewrites the tail every tick, so an idle stretch that
  is still running stays "quiet" until it is long enough to be "away".
  Threshold: `away_secs` (1800) when `LiveContext::live_at` holds (a
  Meeting window; a `call` row over the idle start; a Terminal or Editor
  with an AI session write in the five minutes before), else
  `quiet_secs` (600). Both 0 = the m31 rule, which is how the gate
  compares. Deviation from the plan text: the screen may change while
  quiet — focus and title events keep cutting spans (a terminal title
  turning into "42 passed" mid-read is a new span), with the quiet time
  attributed on each side; the first draft dropped those events and lost
  the return-to-window moment in the `day4_heroku` fixture.
- **Live context is by time, not by window** (`7e9b026`):
  `storage::live_context` loads every AI session's `writes` and every
  `call` row from the tail's start; the per-window repo matching of
  `extract::from_activity` is not repeated in the sessionizer.
- **Lock is its own event** (`7e9b026`, `de43866`): `CaptureEvent::Lock`
  stored as `events.kind = 'lock'` (idle column reused). The logind
  signal and the focus provider's exit send it; the sessionizer settles a
  pending quiet stretch first, then closes whatever is open at the lock.
  `daemon` feeds `idle_since` from it too; `status --events` prints
  `[lock] locked=…`.
- **Migration 025** (`7e9b026`): `spans.quiet_ms` and
  `presence(minute_ts, keys, buttons, motion, scroll)`. `replace_tail`
  writes `quiet_ms`; `anchored_spans` reads it and sets `wrote` (the
  Session anchor's row has a write inside the span) from one query over
  overlapping `ai_session` rows — no new anchor kind, anchors stay
  scored evidence.
- **Segmenter and report** (`7e9b026`): `KINDS` gains `supervise`
  (Terminal with a Session anchor, `wrote`, quiet share ≥ half; else
  `agent`); `Call` rows attach as `Event` anchors, so `span_kind`'s
  existing `has(Event) → meet` covers calls. `RangeReport.by_kind` sums
  the tasks; `report::hands_split` renders `hands-on 3h10m · hands-off
  4h20m (supervise 2h00m · read 1h00m · meet 1h20m)` — hands-off =
  supervise + read + meet, `break` in neither — in `to_md` and under
  the Tasks header in the UI. Kinds only exist in segmenter mode (the
  live config since 11:11 today), so the line is empty on the model path.
- **Presence capture** (`de43866`): `chronicle_capture::PresenceProvider`
  and `presence::X11PresenceProvider` (own connection, x11rb `xinput`
  feature — pulls `xfixes`/`render`/`shape` protocol code, accepted).
  `XISelectEvents` on the root for `XIAllDevices` raw key press, button
  press and motion; a 250 ms drain loop counts them per minute and emits
  `CaptureEvent::Presence` only for minutes with input; storage upserts
  by adding. Buttons 4–7 count as scroll (smooth scrolling also bumps
  motion). Keycodes are never read. `capture_presence = false` or a
  server without XInput 2 → warning, nothing else changes. Settings
  gains `quiet`, `away` and a `presence counts` checkbox beside `afk
  close`.
- **Gate tool** (`de43866`): `chronicle bench --gaps --since N`
  re-sessionizes the stored events twice and prints the daytime (08–19)
  AFK gap histogram plus the folded quiet minutes.

Gate, sandbox copy of the live DB, last 7 days (8253 events, 255 session
writes, 3 calls):

| | 2–5 min | 5–15 min | 15+ min | quiet folded |
|---|---|---|---|---|
| m31 rule (0/0) | 28 (96 min) | 24 (187 min) | 4 (5350 min) | 0 |
| configured (600/1800) | 0 | 4 (50 min) | 4 (5350 min) | 255 min |

The four 5–15 min gaps that remain are 10–15 min stretches with nothing
live on screen; the 15+ class is untouched. Golden fixtures with idle
under 10 min (`day1` 7 min, `day2_web` 8 min, `day4_heroku` 2+3 min,
`agency_day`/`pm_day` 3+4 min) now fold those into the neighbouring
spans; `day1_batches` closes at the 30-minute cap instead of the
7-minute gap. Live on a sandbox daemon (`XDG_DATA_HOME` copy, socket
bind needs the tool sandbox off): three `presence` rows in three minutes
(1100 keys, 26 buttons, 8540 motion, 26 scroll; James was typing,
`xdotool mousemove_relative` for the motion), `loginctl lock-session 1`
→ `[lock] locked=true` at 13:11:53 and ledger row 3 closed `lock`,
unlock → `locked=false` and row 4. Debug-build RSS 61 MB is not
comparable to the release daemon's 19.5 MB; the installed release sits
at 34 MB (above).

Open: `AFK_SPLIT_MINS` (5) stays in `assign_batches`, `afk_gaps_min` and
`clamp_intervals` — an `afk` span now only exists past `quiet_secs`, so
those effectively split at 10/30 min. The unbatched tail lives longer
(a quiet span stays open up to `away_secs`), so `replace_tail` churn and
the pre-existing `span_embeddings` orphaning (no FK) cover more spans per
tick; watch `embed_new_spans` volume. Chunk 3 can read `presence` for
per-minute hands-on inside a span; chunk 1 derives nothing from it.

## Shipped (2026-09-08, chunk 2)

Two commits on `main` (74557e7 core, 89dbaea app/capture), 252 tests (4
new), clippy clean. No migration: everything new lives in the
`ai_session` row's `detail` JSON.

- **Titles, not summaries** (`89dbaea`): the plan said to parse `summary`
  records; the transcripts on this box have none. The terminal title
  comes from `{"type":"ai-title","aiTitle":…,"sessionId":…}` records
  (no timestamp, no cwd), written every turn or so from about line 23 of
  the transcript, so the head scan usually sees the first one and the
  tail sees renames. The collector keeps them per segment as `titles`
  (distinct, oldest first, up to 8, a segment opened after a gap starts
  with the latest); a title before the first line waits for it.
- **Prompt minutes** (`89dbaea`): `prompt_minutes`, same shape and cap as
  `writes`, for `user` records that are not tool results and not
  `isMeta` (skill files, local command output); slash commands and task
  notifications count as typing. 11 of 14 recent sessions carry titles,
  and 2 of 3 non-tool-result user records in a transcript are typed.
- **Touch log** (`89dbaea`): `touches` as `[[minute, path index], …]`,
  one per file and minute, newest 240 kept. Without it every session
  named its first eight files to every span it touched, and a full-read
  backfill turned that into a doc flood (1831 → 6943 doc anchors over
  the week, 8/19 → 6/19 on the gate). `extract::session_docs` now takes
  the files touched at or before the span's end and within 15 min of its
  start, else the latest touch minute; legacy rows fall back to the first
  paths. 4467 doc anchors after.
- **Attachment order** (`74557e7`): `from_activity` takes the span title.
  A session whose `titles` contain the cleaned title owns the span
  (deviation: overlap is not required — the title stays on screen after
  the transcript's last write, and 14 of 55 titled spans in the 09-03
  window had their session end minutes earlier); else the overlapping
  session with the nearest `prompt_minutes` at or before the span's end;
  else the nearest write as before; rows with none of those all attach.
  Multi-session spans over the week: 380 → 0.
- **Repo-qualified branch** (`74557e7`): `push_branch` writes
  `chronicle@main` for every branch anchor with a known repo (sessions,
  edits and the trailing checkout, which now passes its repo) — not only
  the session's, so the same branch is one key everywhere. The work-item
  key is still taken from the bare branch. Ablation on the gate: neutral.
- **Bare terminal** (`74557e7`): a terminal that names no place takes its
  place only from a cwd row still alive at the span's end (or a one-shot
  shell fold); with none it stays unattached instead of borrowing the
  place a shell left earlier.
- **Backfill** (`89dbaea`): `chronicle backfill-sessions --since DAY`
  re-reads every transcript modified since the day in full
  (`ai_sessions::replay_transcripts`, no head/tail skip) and
  `storage::replace_session` swaps the session's rows (`<id>`, `<id>#N`)
  in one transaction; the daemon re-upserts the sessions it still tracks
  under the same ids. Run `backfill-anchors` after it. On the week's
  copy: 111 sessions, 113 → 138 rows (full reads find the pauses the
  bridge skipped).

Gate, sandbox copies of the live DB, `bench --replay --scorer --since 7
--probes direct`, anchors re-run since 09-01 on each:

| build | sessions | strict | lenient |
|---|---|---|---|
| installed chunk 1 | as captured | 8/19 | 11/19 |
| chunk 2 code, rows as captured | as captured | 8/19 | 11/19 |
| chunk 2, sessions backfilled, first-paths docs | backfilled | 6/19 | 9/19 |
| chunk 2, sessions backfilled, no session docs | backfilled | 8/19 | 11/19 |
| **chunk 2 shipped** (touch docs, titles) | backfilled | **8/19** | 11/19 |

Flat at 8 (the plan's 10 is not met; 7 was the morning's number before
chunk 1). One probe each way against the baseline: c58 (21–22 min, task
73 over 60) passes, c75 (an eject from "start dev on ACME-11382") fails
— the window's terminal spans now carry that session's branch and item
and the scorer places them back in it with margin 0.11 (the baseline's
0.00 was a coin flip). Second half of the gate: every titled terminal
span in the 09-03 15:00–16:44 window (55) attaches the session whose
title it shows; before, 41 right, 14 wrong (the session had ended).

Installed 14:02 the same day (unit stopped, `cargo install --locked`,
restarted healthy, release RSS 29 MB at start). Live backfills right
after: `backfill-sessions --since 2026-09-01` replaced 111 sessions (138
rows: 110 with titles, 87 with touches, 135 with prompt minutes),
`backfill-anchors --since 2026-09-01` anchored 2699 spans,
`backfill-evidence` rebuilt 355 rows. The session running this chunk
shows `titles: ["Grab next task"]` within a minute of the restart.

Open: `MAX_PATHS` (40) still bounds which files a touch can name — a
session past 40 distinct files records touches only for the first 40.
The 240-touch cap covers about four hours of edits; older spans in a
long session get the latest touch minute before them. Chunk 3 can read
`touches` and `prompt_minutes` per minute for the supervision split.


## Shipped (2026-09-08, chunk 3)

Two commits on `main` (core, app), 258 tests (6 new), clippy clean.
Migration 026 `intervals.share REAL NOT NULL DEFAULT 1.0`.

- **Split after the verdict** (`segmenter::split_concurrent`): the
  segmenter still cuts and scores whole segments; a placement of kind
  `agent` or `supervise` with two or more AI sessions writing within 2
  min of its edges becomes one row per task over the same range, `share`
  by the prompts typed into each task's sessions in that window (their
  focus minutes when no session kept prompts). Rows over one range sum
  to 1; every other placement passes through at 1.0. `storage::
  live_sessions` feeds it (`writes`, `prompt_minutes`, title and the
  row's own scope anchors per session row; a `<id>#N` row is its own
  session, and two rows of one conversation land on one task, so the
  grouping folds them).
- **A session's task**: what its own spans in the range score to, or,
  for a session that wrote without being on screen, its scope (session,
  repo, branch, item) spread over the range. Deviation from the plan's
  "their tasks": with the profiles as of the 09-03 window, task 85
  "start dev on ACME-11382" [mailer] already held chronicle=107 min
  and chronicle@main=104, so every session, chronicle ones included,
  scored to it and nothing split. The **place rule** fixes that without
  waiting for chunk 4: a session whose repo is not its verdict task's
  project, and whose ticket that task does not hold, is foreign to it and
  goes to the best-ranked task in its own repo, else becomes a new task
  there (one per repo across the window, given `segment_new_task_min`
  of shared time; a sliver folds back into the segment's target). A task
  that holds the session's ticket keeps it whatever the repo (ACME-11382
  spans contoso and mailer). A task with no project constrains nothing.
- **Kind per row** is its own sessions' spans' (`supervise` for one never
  on screen); reason reads "3 of 9 concurrent sessions, 12 of 38 prompts".
- **Totals** multiply by `share`: `Task.share` + `Task::weigh` for the
  report (tasks, projects, kinds, grand total), the UI groups and today's
  open-task minutes, the checkpoint aggregate, `SegmentRow` (re-score
  snapshots deserialize old ones at 1.0; the undo re-inserts it). `chronicle
  status --day` prints a row's share when under 100 %.
- **Agents lane** (`timeline::agent_lanes`, under the chart in every
  band mode): one bar per AI session on the day (`storage::agent_lanes`:
  start to end or last write), the stretches its terminal was on screen
  in full accent, each prompt a tick; hover for title, repo, prompts and
  on-screen time. At most six sessions, the day's longest.
- **`chronicle bench --window START..END`**: places a window the way
  `reconcile` would, dry, against profiles built as of the window's start
  the replay's way (`profile::build_profiles` at `lo`), prints every row
  with its share and kind, the sessions the split saw with where each
  one's own evidence lands, and the window's wall time by project and by
  task.

Gate, `bench --window 2026-09-03T15:00..2026-09-03T16:44` on a sandbox
copy of the live DB, profiles as of 15:00 (4 tasks with evidence):

| build | chronicle | contoso + mailer | rows |
|---|---|---|---|
| chunk 2 (whole segments) | 0 % | 93.7 % | 2, both task 85 |
| chunk 3, scorer alone | 0 % | 93.7 % | 2 (every session → 85) |
| **chunk 3 + place rule** | **39.9 %** | **53.7 %** | 4: 85 at 68 % / 39 %, task 104 "optimize and clean up" [chronicle] at 32 % / 61 % |

Both halves met (≥ 35 %, ≤ 60 %); 97 of 104 min placed, the rest an AFK
gap. The chronicle share landed on an existing chronicle task (104, 10
declared minutes) rather than a new one, because the place rule takes the
best-ranked task in the repo first. Weekly sums: `report::build` test
`shared_rows_sum_to_wall_time` (two rows 0.6 / 0.4 over 104 min report
104 min by task, project and kind).

Open: the split reads today's `titles`/`prompt_minutes`; sessions
captured before chunk 2 have no prompts and fall back to focus shares.
A session with writes but no prompts and no focus in the window drops
out (0 weight). The naming job for a task the split creates sees the
whole range's evidence, not only its sessions'. The 84 px lane label
truncates long session titles (same as task lanes).

## Shipped (2026-09-08, chunk 4)

Two commits on `main` (core; app and capture), 266 tests (8 new), clippy
unchanged (the five pre-existing warnings are in files this chunk did not
touch). No migration.

- **A ref from the task's own place** (`anchor::anchor_tasks`, new
  `projects` argument): a task with a declared project takes branch
  time, checkouts, commits and PR rows from that repo alone (`a/b`
  names two); a task with no project constrains nothing, and a prepass
  run has none. `derive` reads each placed task's project from `tasks`.
  The wrong refs on tasks 98 (ACME-11381), 109 (ACME-11382) and 111
  (ACME-11385), all chronicle tasks named by agent commits in contoso and
  mailer, were cleared by hand on the live DB.
- **PR rows are strong only when the person was on them**: `gh search
  prs` carries no head branch, so the rule reads the PR's title key
  instead — a checkout or commit on a branch carrying that key in the
  PR's repo, or a cwd / shell row in that repo, within 24 h of the PR's
  update (`PR_NEARBY_MS`). A weak PR row neither gates a branch majority
  nor names a task on its own. `derive` now passes cwd and shell rows
  along with vcs and PR; the prepass adds `storage::place_rows_in_range`.
- **Listener map** (`capture::ports::PortMapProvider`, its own thread,
  60 s): LISTEN rows of `/proc/net/tcp{,6}` → socket inode → owning pid
  via `/proc/*/fd` → that pid's cwd → `place_from_path`. Each listener is
  an upserted `Cwd` row keyed `port:<port>:<place>` with `{"path","port"}`
  in detail; one whose cwd names no place (sshd, cups, a server started
  from `~`) is dropped. On this machine today: fabrikam-web, chronicle and
  mailer dev servers, all on ephemeral ports.
- **`localhost:<port>` is a place through the map**: `parse_url` accepts
  `localhost`, `127.0.0.1`, `0.0.0.0` and `[::1]` and keeps their port;
  `browser` emits `Domain localhost:<port>` (the page title stays the
  document); `from_activity` turns it into `Place <repo>` from the port
  row alive while the span was open, and the repo's checkout then names
  the branch. Live backfill: 53 spans now carry `localhost:<port>`
  domains (8000, 8001, 8002, 8004, 54323); they only resolve to places
  going forward, once port rows exist.
- **A minor document cannot name a segment** (`profile::NAMING_SHARE`
  = 0.25): `Segment::describe` skips a `Doc` key under a quarter of the
  segment's focus, so the segmenter's placeholder labels come from the
  majority cluster; the `name_task` job (`ai_job::naming_spans`) drops
  focus spans whose only anchors are documents or sites under a quarter
  of the range before building the digest, keeping anchorless spans and
  never dropping the last focus span.
- **Unsure placements feed no profile** (`IntervalRow.pending`): a
  `segment` row with `confident = 0` and no closed `verdict_log`
  outcome is left out of `build_evidence` and of the centroid /
  recency intervals until it is kept, corrected or passively accepted
  (a day later, `passive_accept`). `replay_rows` computes it with a
  `NOT EXISTS` on `verdict_log`.
- **A task with no evidence has no profile**: `build_profiles` no longer
  adds zero profiles for live tasks, so a bare label with no key, project
  or kept time is not scoreable and cannot win on the recency bonus.
- Live backfill after install: `backfill-anchors` (3899 spans),
  `backfill-evidence` (417 rows), daemon restarted healthy.

Gate, `bench --replay --scorer` on a sandbox copy of the live DB (the
plan's 45/132 was a 09-08 morning probe set; the window has moved since,
so before/after on today's set):

| probes | before | chunk 4 |
|---|---|---|
| `all --since 7` | 32/85 | 30/85 |
| `all --since 14` | 59/208 | 49/208 |
| `direct --since 14` | 8/36 | 8/36 |

Every lost `all` probe is a merge into task 60 "user experience review
and tweaks" that used to pass *by label* because the new-task
placeholder carried the doc anchor `m14-ui-restructure-review`, whose
"review" token matched the wanted label; the 25 % rule drops that doc
from the placeholder, so those passes were coincidences and the direct
set is flat. `fabrikam-web` appears in the chunk 4 log only on probes that
want it (c84); it wins no chronicle terminal.

Open: `gh search prs` has no head branch, so PR strength keys on the
title's ticket rather than the branch; a PR whose title carries no key
is never strong (it never anchored anyway). The listener map reads only
processes the daemon's user can inspect. Historical `localhost` spans
have no port rows to resolve against. The `name_task` filter matches
drafts to anchored spans by `(start, end)`; a draft the sessionizer
re-cut since the anchors were stored keeps its place.

## Shipped (2026-09-08, chunk 5)

Narrative from ground truth, on `main`. No migration; `activity_events`
takes the new kind as a string.

- **`note` activity kind** (`ActivityKind::Note`, upsert on `(kind,
  ext_id)`): `capture::notes::NotesProvider` reads every `git_repos`
  entry's `.remember/today-*.md` (`.done.md` too) every 60 s, re-parsing a
  file only when its size or mtime moves; the first poll reads them all,
  so a fresh daemon backfills every note it can see. A `## HH:MM | branch`
  or `## HH:MM–HH:MM | branch` head is one row: `ts` (and `end_ts` for a
  range) in the local zone from the file's date, `branch`, `ext_id`
  `note:<repo>:<date>T<HH:MM>`, summary the body on one line (300 chars),
  detail `{"body","file"}` (body 2000 chars). `now.md` (the buffer) is not
  read. `chronicle backfill-notes` (hidden) is one pass of the same reader
  for a DB the daemon has not started on yet. Timeline rows render it with
  the note-pencil glyph; the digest's activity line as `note <repo>@<branch>
  "…"`.
- **Sessions keep their last reply** (`ai_sessions`): the newest assistant
  text block, clipped to 300 chars, lands in the row's detail as
  `last_assistant` on every upsert. Rows captured before this have none
  until `backfill-sessions` re-reads their transcripts.
- **`digest::ground_truth`**: the rows that cannot be wrong, one line
  each ending in a source tag — `commit <repo>@<branch> "subject" [commit
  <7 chars>]`, `session … [session HH:MM]` with up to four `prompt:` lines
  and a `reply:` line, `note … [note HH:MM]`, `PR authored/reviewed …
  [pr <number>]`, `meeting … [meeting HH:MM]`. Checkouts, shells, cwd
  probes, edits and calls say where and how long, never what, and are left
  out; an interrupt marker or pasted-image placeholder is not a prompt; a
  session asked nothing that lasted under a minute (a resumed transcript's
  stub) is nothing. `ground_truth_within(max_chars)` keeps commits first,
  then notes, PRs, meetings, sessions, until the budget is spent, and
  emits what it kept by time — the local model's window is small and the
  tokenizer cuts the tail, so the digest chooses what goes.
- **Descriptions** (`task_description`): window titles first, then the
  task's ground truth over its intervals (1600 chars); a task with neither
  is skipped (`SkipJob`), never failed. The prompt names ground truth as
  the stronger signal.
- **Journal**: the batch's ground truth (1600 chars) replaces the
  activity-line list; a batch with truth but no window titles still gets
  an entry; only a batch with neither bails. The prompt's `{git}` slot is
  now `{truth}`.
- **Standup**: per task, the last three journal entries (clipped to 220
  chars at a sentence or word end) tagged `[journal HH:MM]`, the checkpoint
  tagged `[checkpoint]`, then the task's ground truth for the day; a task
  with truth but no journal entries gets a block too. The whole digest is
  budgeted (`STANDUP_DIGEST_CHARS` = 2600 tokens' worth, shared per task,
  600 chars minimum) so the last task of a busy day is not the one the
  tokenizer cuts, and the local describer gives the standup 512 output
  tokens (`MAX_GEN_STANDUP`) instead of 256. The prompt asks for a block
  per task with one bullet per thing done (≤ 20 words) ending in the
  source tag copied exactly, and `Next:` from the checkpoint;
  `prompts::keep_sourced` then drops every bullet without a `[…]` tag and
  every block left with none, and the job fails rather than store a draft
  with no sourced claim. The digest and, when lines were dropped, the raw
  draft log at debug (`RUST_LOG=chronicle=debug`).

Gate — standup for a replayed day on a sandbox copy of the live DB
(`backfill-notes` first, `ai-job` on the local 4B):

| day | blocks | bullets | unsourced dropped |
|---|---|---|---|
| 2026-09-04 (journals + truth) | 3 tasks, `Next:` from the checkpoint | 7, every one `[journal HH:MM]` or `[checkpoint]` | 0 |
| 2026-09-08 (no journals; truth only) | 1 task | 13, every one `[commit …]` or `[session …]` | 1 (the 14th, cut by the output cap mid-tag) |

Before the budget the 09-04 digest was 18 K chars: the 4B saw one task,
copied its journal entries whole and ran out of output mid-bullet; after,
6.8 K chars and three blocks.

Open: the 09-08 draft carries none of the day's 24 chronicle commits —
they sit under task 120 (`fabrikam-web`), whose repo filter in
`activity_by_task` keeps only fabrikam-web rows, so the day's chronicle work
has no task to be true under (the placement problem, M33). A day with many
commits under one task yields one bullet per commit and the output cap cuts
the last; a cloud route has no such cap. Sessions captured before today
have no `last_assistant` until `backfill-sessions`. The Connections panel
has no row for notes (on whenever `git_repos` is set, no switch).
