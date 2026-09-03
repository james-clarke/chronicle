# Post-m16 — state, acceptance, next candidates

Handoff doc for the next working session/agent. Written 2026-09-01, right
after m16 code landed and deployed. Read `m15-task-workspace.md` first for
the workspace vision; this doc is what comes after it.

## Where things stand

- **M0–M16 code complete.** Daemon live from `~/.cargo/bin` (systemd user
  unit, migration 008 applied). `workflow.md` governs the dev loop: build and
  test anytime; stop the unit before `cargo install --locked` or debug-daemon
  runs.
- **78 tests green, clippy clean.** m16 notes (design decisions, file map) in
  `progress.md` under "M16 notes".
- Live config: `~/.local/share/chronicle/config.toml` has
  `git_repos = ["~/dev/chronicle"]`; `~/.local/share/chronicle/mcp.toml` has
  `context_calls` (jira_search) + m16 `fetch_calls` (jira_get_issue with
  `{ref}`, comment_limit 10). `fetch_context` smoke-tested live against a
  real ACME ticket 2026-09-01 — MCP auth and tool names verified good.

## Acceptance still open (James generates data naturally; verify, don't rig)

James is finishing one real task and starting another, so the evidence for
both walks will exist soon. When picking this up:

1. **m15 anchor walk:** ≥30 min on a ticketed branch
   (`ABC-123-…`) → derived task gets the anchor chip; commits listed in the
   detail pane; digest shows `## Git activity`. Check:
   `chronicle dump --day <day>` + detail pane. Note: only commits/checkouts
   made while the daemon runs are captured (poll-based).
2. **m16 walk:**
   - Declare a task from a Jira key (or let anchoring set one) → Context
     section appears in the detail pane without touching a terminal
     (fetch_context lands on the next scheduler tick or `derive` poke).
   - Work on it across ≥1 batch → Journal entries appear (journal jobs are
     priority 0: they run at the idle gate, so entries lag until an idle
     window).
   - AFK ≥30 min, return, reopen UI → Home resume card (label + anchor +
     state + next steps); "open workspace" jumps to the task.
   - Detail pane "chat" → scoped chat answers "what's left?" from ticket +
     journal.
   - If something misfires, check `ai_jobs` status/error rows and the daemon
     log before touching code; a real derive/journal failure becomes a
     fixture before a fix (README convention).

## Next milestone candidates (James picks; rough order of leverage)

From `m15-task-workspace.md` "Deferred" plus gaps seen while building m16:

1. **Standup draft.** Morning digest of yesterday's journals across tasks —
   pure read over `journal_entries` + checkpoints, one new ai_job kind +
   Home card/CLI output. Cheapest feature with daily-visible value; also
   exercises journal quality, which feeds every later feature.
2. **Jira worklog/comment write-back (first Action-layer feature).** MCP
   outbound: post a journal-derived worklog or checkpoint comment to the
   anchored ticket. Needs an explicit-confirm UX (never auto-post — doc's
   consent rule) and an `[[action_calls]]`-style allowlist section. Highest
   external value, first write-path risk.
3. **Journal/checkpoint corrections UX.** Journal entries are append-only
   rows; make them editable in the detail pane (edit = correction, feeds the
   same trust loop as renames). Small, rounds off m16.
4. **Intent layer.** Morning "today I'm on X" prompt + evening drift-vs-plan
   in the day digest. New tiny table + Home input + digest section.
5. **Git repo auto-discovery** (watch window titles for repo paths, offer to
   add to `git_repos`) and **PR/review evidence via `gh`**. Capture-side
   deepening.
6. **Ports (M17 macOS / M18 Windows / M19 packaging)** — README milestones;
   big, separate track. Don't start without James.

Suggested default if James doesn't specify: **1 then 3** (standup draft,
then journal corrections) — both single-player, testable with the data his
current tasks are about to generate, no write-back risk.

## Facts a new agent will otherwise rediscover the hard way

- One llama worker at a time, ever (`Scheduler.worker` slot). Interactive
  priority = `AI_JOB_INTERACTIVE` (5); ≥5 bypasses the idle gate.
- `fetch_context` runs LLM-free; `run_ai_job` resolves the model per-arm, so
  never reintroduce a worker-level "no model → bail".
- Journal upserts on `(task_id, batch_id)`; checkpoint/task_context are
  latest-only upserts. Workspace FKs cascade with the task; a task-scoped
  conversation survives task deletion as a general thread.
- Checkpoint trigger lives in the daemon tick arm (once-per-idle-epoch
  in-memory gate), NOT the Afk event branch. `checkpoint_afk_secs = 0`
  disables.
- Chat scope is a property of `ClientMsg::Switch` (`task_id: Option<i64>`),
  one conversation per task via partial unique index; scope resolution on
  history-menu switches comes from `conversations.task_id`.
- UI declare input accepts a bare ticket key or URL: label collapses to the
  key, anchor set, fetch queued.
- `progress.md` is local-only (`.git/info/exclude`) — repo stays agent-free;
  keep design notes there, not in tracked files beyond direction docs.
