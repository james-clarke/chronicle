# docs/plans

One design document per milestone, written before the code. The
[README](../README.md) describes the code as it stands. These documents
describe why it is shaped that way.

## What a plan is

Each plan opens with a Status line saying what had shipped when the file
was last touched, then a few paragraphs on the problem and the intended
fix. Below that are the decisions taken and the work split into numbered
chunks. Each chunk has a gate, which is a concrete condition it has to meet
before it counts as done. Most plans end with an "Owed" section listing what
was deliberately left out. Claims about what the code does at the time of
writing carry a `file:line` so they can be checked.

The plans were not cleaned up after the fact. Several still have a Status
line saying a chunk is open while a "Shipped" section further down records
it landing the same day. The plans keep that history instead of being
cleaned up afterwards.

## Where to start reading

- [m13-ui-plan.md](plans/m13-ui-plan.md) is the earliest and shortest,
  about forty lines: root-cause findings with a `file:line` each, the
  decisions they led to, and an execution order.
- [m35-project-first-plan.md](plans/m35-project-first-plan.md) reverses an
  earlier decision. Time had been attributed to tasks first and projects
  second since M9. This plan traces one mis-projected task taking over a
  day's report back to that ordering and flips it.
- [m36-accuracy-plan.md](plans/m36-accuracy-plan.md) logs three attempts to
  make corrections work as few-shot examples, with the score each produced.
  None cleared the gate, and the plan says so.
- [m42-open-source-plan.md](plans/m42-open-source-plan.md) covers going
  public: the licence, the synthetic fixture data and the history rewrite,
  which is why the repository looks the way it does.

## Milestone history

Linux shipped first. macOS (M38) and Wayland (M39) followed as ports, and
Windows (M40) is planned but not started. M0 through M12 predate
the plan documents; this table is their record. It is the original plan
through M28, kept as written, including where a later milestone renumbered
it.

| M | Deliverable | Acceptance |
|---|---|---|
| 0 | Workspace, core types, config (TOML), SQLite + migrations, logging, `dump`, toolchain pin + CI | `chronicle dump` on empty DB |
| 1 | X11 capture + AFK; daemon writes events | 10 min of real window/tab switching → correct app/title/AFK stream in `dump`; RSS < 30 MB |
| 2 | Sessionizer + digest + golden tests | fixture-day digest matches golden; ≤ 3 K tokens |
| 3 | egui timeline (spans only) as `ui` child + single-instance `toggle` socket | idle CPU ~0 % with window open; toggle spawns/raises UI child; window close exits it fully, daemon RSS unchanged |
| 4 | Derivation end-to-end + **model benchmark gate** | work 30 min → go idle → tasks appear; daemon RSS unchanged |
| 5 | Corrections loop (FTS few-shot) | a correction changes the next batch's output on a crafted fixture |
| 6 | AW endpoint + Host/CORS hardening | stock AW extension → per-site spans, not just "firefox" |
| 7 | Chat | "what did I do this morning?" answers grounded in DB |
| 8 | MCP | with a jira MCP server, derived labels reference ticket keys |
| 9 | **Derive v3 task identity:** tasks=identity + intervals schema, open-task ref linking, declared tasks, eval harness, grouped UI | eval fixtures score (day3 9/9); live: declared task accumulates intervals across batches, strays get own tasks |
| 10 | **Task lifecycle + project layer:** task-level merge (all intervals + `merge` correction), derived-open auto-close (default 3 days idle), reopen, per-project rollup/totals | merge folds a task in one action and teaches the next digest; stale derived tasks leave the open list unaided; "chronicle: 4h today" visible |
| 11 | **Daily-driver ops:** systemd user unit + autostart, SIGTERM clean shutdown, `chronicle status`, size-based log rotation | reboot → daemon up without a terminal; `chronicle status` reports healthy |
| 12 | **Reports:** week/day summary view, timesheet export (CSV/md), chat aggregate queries | "how long on chronicle this week?" answered both in UI and chat; export opens in a spreadsheet |
| 13 | **UI + onboarding polish:** settings visual pass, confidence tints, search, chat panel visuals, in-UI model download | fresh install to first derived task without touching a terminal |
| 14 | **AI layer:** task descriptions, declare suggestions, week narratives, `ai_jobs` queue + idle-gated worker, insights (sessions, focus metrics, deltas) | descriptions appear on tasks unaided; insights strip + week narrative in UI |
| 15 | **Git evidence + task anchors:** `vcs_events` capture (HEAD/commit polling), digest git section, deterministic ticket-key anchoring (`tasks.external_ref`), anchor chip + commit evidence in detail pane — see [m15-task-workspace.md](plans/m15-task-workspace.md) | work 30 min on branch `ABC-123-…` → derived task anchored `ABC-123`; its commits listed in the detail pane |
| 16 | **Task workspace:** MCP context fetch on task add, per-batch journal entries, AFK checkpoints ("where I am / next steps"), Home resume card, task-scoped chat | add task from a Jira key, work, leave ≥ 1 h, return → resume card shows journal + grounded next steps |
| 21.5 | **More connections:** presets for GitHub (`github-mcp-server`, PR search ×2), Google Calendar (`@cocal/google-calendar-mcp`, today's `list-events`), CalDAV (`caldav-mcp`); `{today}` / `{tomorrow}` / `{now}` placeholders in call args (local RFC 3339); `mcpServers` JSON import from `~/.claude.json` / Claude Desktop / Cursor / `.mcp.json` as config-only rows; preset hint line in the add form | add → Google Calendar → test passes after the one-time auth; today's meetings show in the digest `## Workspace context` |
| 22 | **Activity events + local collectors:** `vcs_events` → `activity_events` (`kind`, `ext_id`, `end_ts`, per-kind dedupe); Claude Code session watcher (`ai_session_dirs`), `gh` PR poller (`github_prs`, opt-in), PipeWire mic-in-use → `call` (`mic_capture`); digest `## Activity`, timeline rows per kind — see [m22-collectors-plan.md](plans/m22-collectors-plan.md) | a Claude session on a ticketed branch shows under its task within 20 s with a growing duration; a PR update and a call land as rows and reach the journal digest |
| 23 | **Unassigned triage:** Home › Unassigned › `organize` takeover — the day's unassigned focus folded into contiguous runs (gap < 5 min), task pick per run pre-filled from past corrections (FTS, ubiquitous terms pruned), bulk assign / new task; `assign_unassigned` writes confidence-1.0 intervals per batch + an `assign` correction | two hours of unassigned time organized in under a minute; the next derive on similar work lands under the same task |
| 24 | **Live feed:** Unassigned as the front door — block states (fresh / provisional / proposed / assigned / unmatched / ejected), deterministic pre-pass (branch → ticket, repo signal, corrections FTS) writing provisional intervals before the batch derive, proposal cards for unmatched clusters, eject with negative corrections — see [m24-live-feed-plan.md](plans/m24-live-feed-plan.md) | a block on a ticketed branch shows under its task within one tick; an ejected block never returns to that task |
| 25 | **Polish 3 — cleaner, easier to read:** one duration rule (`2h41m` / `30m` / `41s`), resizable window (corner grip, size remembered) with wide layouts ≥ 720 pt, standup card folds to the first task + "N more", two-line titles before truncation, human words on feed rows, project-hued task colours, short sessions folded, switches instead of checkboxes — see [m25-polish-plan.md](plans/m25-polish-plan.md) | every view reads at 400×640 and at 900×700 without a truncated label that has no hover |
| 26 | **Daily driver:** Home task list first with time today / last touched / next step, density switch, standup card that remembers it was read; Google Calendar collector (`gcal-login`, `meeting` spans), Wakapi-compatible editor heartbeats (`edit` spans), atuin shell spans, morning/evening intent + `## Plan` digest section + `stuck` chip, `[[action_calls]]` (Jira comment behind a confirm), Storage "what leaves this machine" — see [m26-daily-driver-plan.md](plans/m26-daily-driver-plan.md) | a meeting, an editor session and a shell run all land as rows under the right task; the standup drafts from the intent |
| 27 | **Derivation quality:** interval coalesce + v4/v5 grammar (cap 8), derive metrics + corrections replay eval (`bench --replay`) + bench parity, resident derive worker with a cached instruction prefix, natural batch boundaries at AFK ≥ 5 min + 5-min live tier + streaming "deriving…" row, title/URL ticket-key and cwd rules with a gated repo rule, day-tier consolidation (undoable tidy), Settings › Derivation Pipeline inspector — see [m27-derivation-plan.md](plans/m27-derivation-plan.md) | replay score above the 76/180 baseline; `cached_prefix_tokens` ≈ 1.1 K on every derive after the first; a block on a ticketed page is placed before the model runs |
| 28 | **Clean up + optimize:** loose ends from m21–m27 closed, plan docs under `docs/plans/`, README/progress in sync, five-lens codebase review with the surviving findings applied — see [m28-cleanup-plan.md](plans/m28-cleanup-plan.md) | tests + clippy + fmt green; no open worktrees; every plan doc's status line matches main |

Since M28 the plan documents carry the detail and this list carries the
shape:

| M | Deliverable |
|---|---|
| 29 | **Legibility:** rows a person can read, and a chat worth demoing |
| 30 | **Derivation v2:** evidence first, the model last — span anchors, task evidence, the claims grammar |
| 31 | **Cloud models:** BYOK and a backend registry beside the local model |
| 32 | **Attention, not input:** presence counts, quiet time, capture-gap accounting, the daily self-score |
| 33 | **Interleaved projects:** context switching shown on the timeline |
| 34 | **The site:** `site/`, built by `site/build.sh`, deployed to Render |
| 35 | **Project-first:** projects as the organising unit, silos and sinks, a task manager view |
| 36 | **Accuracy:** API models where they buy something, measured per backend |
| 37 | **Sources:** session formats, the shell hook, git hooks, browser history, link files, ICS calendars |
| 38 | **macOS:** the same daemon on the second platform, written without a Mac and not yet run on one |
| 39 | **Wayland:** wlroots and KWin behind the existing traits |
| 40 | **Windows:** the remaining port, planned in [m40-windows-plan.md](plans/m40-windows-plan.md) and not started |
| 41 | **Connections:** the connector registry, per-platform probes, a health per connector |
| 42 | **Open source:** the licence, a synthetic corpus, a clean history |
| 43 | **The site and the docs, caught up:** the screenshots, and this index |

Prompt and model quality is continuous and bench-gated rather than a
milestone: every real derivation failure becomes a fixture before it gets
fixed.

Deferred past v1: Wayland on GNOME (needs a Shell extension), presence
counts on Wayland, a global hotkey, task split, SQLCipher at rest, signed
macOS and Windows builds, MCP from chat.

## The milestones

| Plan | What it is | State |
|---|---|---|
| [m13-ui-plan.md](plans/m13-ui-plan.md) | Widget-first UI redesign: task cards, evidence bars, one report view | shipped |
| [m15-task-workspace.md](plans/m15-task-workspace.md) | Git evidence anchors (m15) and the task workspace: MCP context, journal, resume card (m16) | shipped |
| [post-m16-direction.md](plans/post-m16-direction.md) | Handoff after m16: where the build stood and the candidate next steps | direction paper (not a milestone) |
| [m17-ux-plan.md](plans/m17-ux-plan.md) | UX fixes: standup reliability, window placement, background noise | shipped |
| [m18-settings-plan.md](plans/m18-settings-plan.md) | Settings redesigned as the setup home, with status and a test per integration | shipped, split into m21 (steps 1–3) and later milestones (steps 4–6) |
| [m19-polish-plan.md](plans/m19-polish-plan.md) | Design-system polish pass: window chrome, scrollbars, type scale, card chrome | shipped |
| [m20-ux-plan.md](plans/m20-ux-plan.md) | UX pass 2: layout rhythm and richer information surfaces | shipped |
| [m21-settings-plan.md](plans/m21-settings-plan.md) | Settings gains a Connections section: MCP servers and git repos, with status | shipped |
| [m22-collectors-plan.md](plans/m22-collectors-plan.md) | `activity_events` generalised; Claude Code, GitHub PR and mic-in-use collectors | shipped |
| [m24-live-feed-plan.md](plans/m24-live-feed-plan.md) | The Unassigned feed becomes the front door: pre-pass, proposals, eject | shipped |
| [m25-polish-plan.md](plans/m25-polish-plan.md) | Polish 3: one duration rule, a resizable window, easier-to-read feed rows | shipped |
| [m26-daily-driver-plan.md](plans/m26-daily-driver-plan.md) | Daily driver: Home task list, calendar, editor/shell heartbeats, intent, Jira write-back | shipped, chunk 7 open (click-tests and a feed soak) |
| [m27-derivation-plan.md](plans/m27-derivation-plan.md) | Derivation speed and accuracy: interval coalesce, resident worker, live tier, an inspector | shipped; a replay regression and a soak day are still open |
| [m28-cleanup-plan.md](plans/m28-cleanup-plan.md) | Repo cleanup: docs synced, loose ends from m21–m27 closed, a five-lens codebase review | shipped |
| [m29-legibility-plan.md](plans/m29-legibility-plan.md) | Legibility pass: list rows, chips, timeline detail, a chat worth demoing | shipped |
| [teams-direction.md](plans/teams-direction.md) | Research: a teams/alignment product built on the derived layer | direction paper (not a milestone) |
| [m30-derivation-v2-plan.md](plans/m30-derivation-v2-plan.md) | Derivation v2: anchored spans, evidence profiles, scorer-first segmentation, embeddings | shipped, chunk 7 open (a soak week before the pre-pass/live-tier default is decided) |
| [m31-cloud-models-plan.md](plans/m31-cloud-models-plan.md) | Cloud models: a backend trait, an Anthropic backend, `models.toml` routing | chunks 0–1 shipped; chunks 2–6 (routing table, secrets/UI, sign-in, hosted tier) open |
| [m32-attention-plan.md](plans/m32-attention-plan.md) | Attention, not input: presence, quiet time, capture-gap accounting, the daily self-score | shipped |
| [m33-interleaving-plan.md](plans/m33-interleaving-plan.md) | Interleaved projects: context switching shown on the timeline | shipped, chunk A open |
| [site-plan.md](plans/site-plan.md) | Research behind the marketing site: mechanism over claims, one plain page | direction paper (not a milestone), folded into m34 |
| [m34-site-plan.md](plans/m34-site-plan.md) | The site: a build pulse, a coming-soon CTA, one different shape per section | shipped |
| [m35-project-first-plan.md](plans/m35-project-first-plan.md) | Project-first: projects as the organising unit, silos, sinks, a task manager view | shipped |
| [dev-tools-direction.md](plans/dev-tools-direction.md) | Research: developers-only focus, accuracy as the product | direction paper (not a milestone) |
| [m36-accuracy-plan.md](plans/m36-accuracy-plan.md) | Accuracy: a deterministic scorer with a frontier advisor over it, measured per backend | shipped |
| [m37-sources-plan.md](plans/m37-sources-plan.md) | Sources: session formats, shell/git hooks, repo discovery, remote MCP servers | shipped, small follow-ups open (remote-URI resolver, OAuth 2.1) |
| [m38-macos-plan.md](plans/m38-macos-plan.md) | macOS: the same daemon ported, written and compiled without a Mac | shipped; real-hardware verification and signing open |
| [m39-wayland-plan.md](plans/m39-wayland-plan.md) | Wayland: wlroots and KWin focus routes behind the existing traits | shipped; verification on a real Wayland login open |
| [m40-windows-plan.md](plans/m40-windows-plan.md) | Windows: the last port — a message-pump capture thread, named pipe, `Run` key, tray, MSVC target — written without a Windows machine | plan written 2026-09-11; not started |
| [m41-connections-plan.md](plans/m41-connections-plan.md) | Connections: the connector registry, per-platform probes and health, the setup view, what you work with, requests and the tools page | shipped; in-app visual checks of the mined list and the compose box open |
| [m42-open-source-plan.md](plans/m42-open-source-plan.md) | Open source: the licence, a synthetic corpus, a clean git history | shipped; repository public and v0.1.0 released 2026-09-11 |
| [m43-site-and-docs-plan.md](plans/m43-site-and-docs-plan.md) | The site and the docs caught up: new screenshots, and this docs index | shipped |

## Generated files

`docs/connectors.json` is generated from the connector registry and checked
by `crates/core/tests/connectors_json.rs`. Do not edit it by hand; the
regeneration command is in [CONTRIBUTING.md](../CONTRIBUTING.md#tests).
