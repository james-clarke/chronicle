# m15/m16 — Task workspace (direction doc)

Vision (James, 2026-09-01): Chronicle builds a **work tree in the background of
everything you do**. Each task is a container you can click into: a visual
timeline of how the work happened, the apps and evidence behind it, and prose
documentation of what was done. Add a task from a Jira ticket and Chronicle
fetches context and waits; work, and it journals; step away, and it derives
"where I am / what's next"; come back, and one card shows where you've been and
where you're going. Chat becomes the front door for mapping tasks, docs, and
progress — once it has real material to talk about.

Guiding rule: **single-player value must stand alone.** Team/aggregation
layers, if ever, publish only user-approved derived summaries. Raw evidence
never leaves the machine.

## Why two milestones

Journal/checkpoint quality is capped by evidence quality. A 4B local model
writing "explanation of work" from window titles alone produces mush; branch
names, commit subjects, and files touched give it nouns. So **m15 deepens
evidence and anchors task identity deterministically; m16 builds the workspace
on top.** Ordering is load-bearing — do not start m16 surfaces before m15
evidence exists.

## The loop (target architecture)

```
Evidence  →  Inference  →  Confirmation  →  Action
capture      derive/jobs    corrections      resume card, standup draft,
(focus, url, (tasks, journal, (existing FTS   worklog write-back (post-m16)
 git, MCP)    checkpoints)     loop)
```

Task becomes a container:

```
task
├── anchor        external_ref: ticket key / branch / PR (deterministic first)
├── context       fetched bundle via MCP: ticket desc, comments, links (m16)
├── journal       append-only derived entries, one per batch touching task (m16)
├── checkpoint    latest "where I am + next steps", regenerated on AFK close (m16)
└── chat          task-scoped thread: context + journal injected (m16)
```

---

## m15 — Git evidence + task anchors

### 1. Schema (migration 007)

- `vcs_events(id, ts, repo TEXT, branch TEXT, kind TEXT CHECK(kind IN
  ('checkout','commit')), commit_id TEXT, summary TEXT)` + ts index.
  Separate table, **not** `events`: git events are point markers, must not
  enter the sessionizer's focus stream.
- `tasks.external_ref TEXT` (nullable) — the anchor. One ref per task for now.
- Retention: prune with the same `retention_days` pass as events.

### 2. Collector (crates/capture/src/git.rs)

- Poll-based, no new deps, platform-independent (first non-X11 provider).
  Poll every 20 s: read `.git/HEAD` per configured repo; mtime-gate so the
  quiet path is two `stat`s per repo.
- Branch = `ref: refs/heads/…` (detached → short hash). Head commit resolved
  via loose ref file, falling back to `packed-refs`.
- Branch changed → `checkout` event. Same branch, new head → `commit` event;
  subject line via `git -C <repo> log -1 --format=%s` (shell-out only on
  change; `git` missing → event without summary, warn once).
- New `CaptureEvent::Vcs(VcsEvent)` variant; daemon writes to `vcs_events`.
- Config: `git_repos: Vec<String>` (paths, `~` expanded). Empty = feature off.
  Auto-discovery from window titles: deferred.

### 3. Digest section

`## Git activity` in the batch digest (inside the ≤3 K cap ladder, cap ~10
lines): `HH:MM checkout <repo> → <branch>` / `HH:MM commit <repo> "<subject>"`.
This alone should lift derive label quality — commit subjects are half the
journal already written.

### 4. Deterministic anchoring

Post-link pass at derive time (no LLM): for each task receiving intervals in
this batch, find the branch active for the majority of that interval time
(from checkout events + current branch). Extract ticket key with config
`ticket_regex` (default `[A-Z][A-Z0-9]+-[0-9]+`) from the branch name; if the
task has no `external_ref`, set it. User can edit/clear the ref in the detail
pane (a correction, feeds FTS like renames).

### 5. UI

Detail pane: anchor chip next to the label; "Git" rows in evidence (commits
whose ts falls inside the task's intervals).

**Acceptance:** work 30 min on branch `ABC-123-sending-plans` → derived task
anchored `ABC-123`; its commits listed in the detail pane; digest golden shows
the git section.

---

## m16 — Task workspace

### 1. Schema (migration 008)

- `task_context(id, task_id FK, source TEXT, fetched_ts, content TEXT)` —
  MCP-fetched bundle, plain text/markdown, token-capped at injection time.
- `journal_entries(id, task_id FK, batch_id FK, start_ts, end_ts, entry TEXT,
  evidence TEXT /* json: interval ids */)`.
- `checkpoints(task_id PK, ts, state TEXT, next_steps TEXT)` — latest only.

### 2. Add task from ref + context fetch

- UI: declare-task row accepts a ticket key / URL; MCP search-and-pick later.
- New job kind `fetch_context` in `ai_jobs` (queue is generic; this one does
  MCP calls, no LLM): fetch ticket description, comments, linked issues/PRs
  via the existing allowlisted client; store in `task_context`. Re-fetch on
  demand (button), never on a timer.

### 3. Journal job

After each derive that touches task T, enqueue `journal(T, batch)`: prompt =
that batch's digest slice for T + git activity + task context header; output
1–3 sentences, evidence = interval ids. Append-only; corrections to journal
text are edits to that row, not re-generation.

### 4. Checkpoint job

Trigger: AFK close ≥ `afk_close_secs` escalation (lunch-scale, config
`checkpoint_afk_secs`, default 30 min) or day end — for tasks with intervals
since last checkpoint. Prompt = journal tail + context. Output: `state` (facts
from journal) + `next_steps` (grounded in ticket acceptance criteria when
context has them; otherwise humble: open items only). Overwrites the task's
checkpoint row.

### 5. Resume card (Home)

Most recently active task with a checkpoint newer than last UI open: card =
task label + anchor chip + state + next steps + "open workspace" → detail
pane.

### 6. Task-scoped chat

Chat worker accepts optional task id; injects context + journal instead of
generic FTS retrieval. Entry point: "chat" button in the detail pane.

**Acceptance:** add a task from a Jira key → context lands without touching a
terminal; work on it, leave ≥ 1 h, return → resume card shows journal +
grounded next steps; task chat answers "what's left?" from ticket + journal.

---

## Deferred (post-m16, candidate m-next)

- Jira worklog/comment write-back (MCP outbound) — first Action-layer feature.
- Standup draft (morning digest of yesterday's journals across tasks).
- Intent layer: morning "today I'm on X", evening drift vs plan.
- Browser extension for first-party URL capture (AW extension already covers).
- Git repo auto-discovery; PR/review evidence via `gh`.
- Team relay: publish approved summaries only. Consent-based by construction.

## Constraints

- Daemon systemd-managed from `~/.cargo/bin` — `workflow.md` rules apply
  (build/test safe anytime; stop unit before `cargo install`).
- Every phase lands with tests passing + `chronicle dump`/UI demonstrating it
  (README conventions); real derive failures become fixtures before fixes.
- llama worker stays one-resident, idle-gated; journal/checkpoint jobs are
  normal-priority (below user-facing describe/suggest).
