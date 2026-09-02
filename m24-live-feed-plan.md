# M24 — Live feed ("the brain at work")

Status: **shipped** (2026-09-02). Chunks 1–4 landed: `intervals.source` (migration 011), `split_interval` + `eject` corrections, `suggest_correction` skips ejected tasks; nullable `intervals.batch_id` + `reason` (migration 012), `prepass` module on a 60 s daemon timer (`prepass_secs`), derive replaces provisional rows and clips around user rows; Home's Unassigned section is now the feed (`storage::feed_blocks`, newest 12 blocks of the day with state chip + reason line, `…` menu: keep / move to / eject on claimed blocks, declare / assign to on runs, arrival fade, `keep_interval` turns a provisional row into a user row plus an `assign` correction); proposals (migration 013 `proposals`, `proposals::refresh` on the pre-pass timer clusters unassigned runs over the last 12 h by shared distinctive title token — rare across the day's titles — or shared repo, keyed by the earliest run, proposed at 10 min of focus, named by a `suggest_task` job bounded to the cluster; Home card with accept = declare + claim runs, not a task = parked for the day); digest (chunk 5: `## Pre-pass hints` minute ranges → open-task index with the rule, hinted tasks appended past the open cap; `## Ejected` `"work" ✗ "task"` lines on their own budget beside the renames; prompt rules for both). All chunks landed; derive quality before/after is the remaining check. Decisions recorded at the end, build order final.

## Context

After m23 the Unassigned section on Home behaves like a feed: once the day is organized, every new focus cluster shows up there and sits until a batch derive claims it or James assigns it by hand. That is already the interesting part of Chronicle — the system deciding, in near-real time, what the user is working on — but today it is invisible. Decisions happen inside `derive` on a 30-minute batch, the user only sees the result (rows moving from Unassigned to a task), and there is no way to see *why* a block landed where it did, to correct a wrong grouping at the moment it happens, or to see the system propose a task before the user declares one.

The idea: make the feed the front door. Show blocks arriving, show the system grouping them (into declared tasks, into a proposed new task, or left unmatched), let the user accept, redirect, or eject with one click, and let every one of those clicks teach the next decision.

## Today (verified in code)

- Unassigned = focus spans with no overlapping interval (`crates/app/src/ui/mod.rs` `load_unassigned`), top 8 clusters by app+title, header total for the day.
- Intervals are created only by batch derivation (`derive` job, `crates/app/src/main.rs`): the sessionizer writes spans continuously (`replace_tail`), batches close every `batch_minutes` (30), and derivation waits for `derive_idle_secs` (300) of idle. So a block is "fresh" for up to ~35 min before the LLM even looks at it. Batches never re-derive once `done`.
- Linking to open tasks happens inside the model prompt (digest `## Open tasks`, `merge::link_intervals`) plus deterministic anchoring by branch → ticket key (`anchor.rs`). Corrections reach the prompt as few-shot lines (`similar_corrections`, FTS over past ctx).
- m23 added the deterministic side: `unassigned_runs` (contiguous folding), `suggest_correction` (IDF-pruned FTS over corrections), `assign_unassigned` (user intervals + `assign` correction).
- `suggest_task` ai_job proposes a label/project/description from the last 15 min of spans, on demand from Home ("suggest").
- Reassign/merge exist at interval and task grain; there is no "eject this span from that task" (split an interval) and no negative signal ("never put this under X").

## Design decisions (proposed)

### 1. Feed model: every block has a state

A block is an unassigned run (m23 folding, gap < 5 min) or, once claimed, the interval that covers it. States:

| State | Meaning | Who moves it |
|---|---|---|
| `fresh` | spans exist, no batch closed yet | time |
| `provisional` | deterministic pre-pass matched it to a task; shown under that task with a dotted tint | pre-pass (new) |
| `proposed` | pre-pass found no task but the block clusters with other unmatched blocks; a new-task card with a suggested label | pre-pass + `suggest_task` |
| `assigned` | interval exists (derive, user assign, or accepted provisional) | derive / user |
| `unmatched` | derive ran and left it uncovered | derive |
| `ejected` | user pulled it out of a task; negative correction stored | user |

The feed view lists blocks newest first with their state, the task they sit under, and the one-line reason ("branch ACME-11381", "matches 3 past corrections", "same repo as task", "model, 62%").

### 2. Deterministic pre-pass, before the LLM

Runs in the daemon on every sessionizer tick over the unbatched tail (cheap, no model): for each fresh run, in order,

1. branch → ticket key → task with that `external_ref` (anchor rule, already deterministic);
2. repo signal → open task whose `project` matches the run's repo (m22 repo-aware rule, reused);
3. `suggest_correction` over the run's titles → task by label;
4. otherwise unmatched.

A hit produces a **provisional interval** (`confidence` 0.5, new `source` column = `prepass`) so every existing surface shows the block under the task immediately. When the batch derives, the prompt receives provisional links as hints; the model's interval replaces the provisional one (or confirms it). A user click on a provisional block ("keep" / "move" / "eject") converts it to a user interval (confidence 1.0) plus a correction, and derive never overrides user intervals.

Why: the 35-minute blind window is what makes the feed feel dead. Three deterministic rules already exist in the codebase, they just run too late.

### 3. Proposed tasks

Unmatched runs that share distinctive title tokens (same IDF pruning as `suggest_correction`) or the same repo cluster into a proposal card: suggested label from `suggest_task` (LLM, low priority, only when the cluster passes 10 min), project from repo. "accept" declares the task and assigns the cluster; "not a task" dismisses the cluster for the day (blocks stay unmatched, no correction).

### 4. Eject and negative corrections

"eject" on a block under a task splits the covering interval at the block's bounds and stores a correction of kind `eject` (task_id, ctx). `suggest_correction` and the pre-pass skip a task that has an `eject` correction matching the run's distinctive tokens; the digest lists ejects as `"X" ✗ "task"` lines so the model stops repeating them.

### 5. Where it lives

Option A: the Unassigned section on Home becomes the feed (states inline, proposals as cards above the unmatched list). Option B: a new top tab `feed` between home and timeline, Home keeps a compact "N blocks waiting" line. Leaning A for the first cut; B if the section outgrows Home.

### 6. Motion

Blocks fade in on arrival (theme fade helpers exist); a block moving from unmatched to a task animates out of the list rather than vanishing. Nothing else moves. Wheel scroll unchanged.

## Build order (each chunk its own commit, tests + clippy)

1. **Storage:** `intervals.source` column (`derived` | `user` | `prepass`), migration; `split_interval` (eject), correction kind `eject`; `suggest_correction` honours ejects. Goldens.
2. **Pre-pass:** daemon tick over the unbatched tail, rules 1–3, provisional intervals; derive treats them as hints and replaces them. Test: a run on a ticketed branch is provisional within one tick; derive replaces it; a user "keep" survives derive.
3. **Feed UI (Home):** block rows with state + reason, `keep` / `move` / `eject` on provisional and assigned blocks, fade-in.
4. **Proposals:** clustering, `suggest_task` on a cluster, proposal card with accept / not a task.
5. **Digest:** provisional hints and eject lines in the prompt; check tomorrow's derive quality before and after.

## Acceptance

- Start work on a ticketed branch: the block appears under that task within one sessionizer tick, tinted provisional, reason "branch ACME-…".
- Open an unrelated tab for 12 minutes: a proposal card appears with a plausible label; accept declares and assigns it in one click.
- Eject a wrongly grouped block: it leaves the task, the next derive on the same titles does not put it back.
- Unassigned total at end of day is what the system genuinely could not place, not what it never got to.

## Decisions (James, 2026-09-02)

1. Lives on Home: the Unassigned section becomes the feed.
2. Provisional intervals count in reports and timesheets (tinted on Home/timeline; reports show them like any interval).
3. Eject: hard negative for the deterministic pre-pass (it would otherwise re-link the same tokens on the next tick), soft for the model (an `"X" ✗ "task"` few-shot line; the model may still choose the task when the wider context says so). Recommended by Claude, accepted pending build.
4. Pre-pass on a slower timer than the sessionizer tick (start at 60 s) so a growing block settles before it is placed.
5. Sequenced after m21.5 presets.

## Roadmap (not in this milestone)

- Feed as an MCP resource so a chat client can ask "what am I doing right now".
- Cross-day proposals (the same unmatched cluster three days running is a task).
