# M35 — Project-first: silos, sinks and a task manager

Status: draft for James's edit, 2026-09-09. Three fixes shipped the same
morning (below) so the daemon is usable while this is built.

## The short version

Chronicle attributes time to tasks first and projects second. A derived
task takes its project from whichever place anchor dominated the segment
it was minted from, and from then on it is a candidate for every segment
in every repo on the strength of whatever evidence it absorbs. On
2026-09-09 one mis-projected derived task ("Debugging and fixing SMS
welcome message concurrency issues", project chronicle, minted 09-08
12:19 before the Chronicle window became furniture) owned every chronicle
session, reopened itself after each close, and split every morning window
50/50 with the portfolio task. A task declared five minutes earlier could
not win because its profile was empty until something placed into it.

M35 flips the order. Projects are defined by James and matched by rule
(repo dir, ticket prefix, domain, title pattern, app) with no model in the
loop; every span gets exactly one project or "unfiled". Tasks live inside
a project. The task James declared is the project's sink; derived tasks
only subdivide inside a project and can be off per project. Closing a
task closes it. Home groups by project, then task. The time diagnostics
(timeline, report, standup, self-score) stay and gain a project axis. A
task manager view replaces the Home row menus.

## What 2026-09-09 morning says

- Task 126 (derived, project chronicle, label about SMS) carried evidence
  from five repos: SMS tests, fabrikam SEO, the portfolio wrench hologram,
  MEMORY.md, rain-sand-pile-physics. `place chronicle` was its declared
  row, `branch chronicle@main` and three session ids its strongest
  interval rows.
- James closed 126 more than once. Every placement into it reopened it
  (`store_segments`) because closed tasks stayed scoreable on branch and
  session anchors, which are per repo, not per task.
- Tasks 144 (portfolio) and 145 (chronicle), declared 09:18, had zero
  evidence rows at 09:23. The window 09:17–09:23 was placed after both
  declares and still went to 126 and 141.
- Every interval 09:00–09:23 was split 0.5/0.5 between 141 and 126 with
  reason "1 of 2 concurrent sessions": the concurrency split hands a
  session to the existing task whose project equals its place, so one
  wrong-project task owns the whole repo.
- 09:25–09:38: two derived tasks (149, 150) named "ACME-11032 ·
  contoso@ACME-11032" were minted beside declared task 143 "ACME-11032"
  because 143 had no profile.
- "assigned chronicle" was the placeholder label of a task an eject
  created; the split lifts the label from a window title. The task was
  orphan-deleted, its two `name_task` jobs stayed pending.
- Batch 117 ran at 08:59 over 22:30 the previous night to 08:59, so the
  first thing Home showed in the morning was last night placed into 126
  and 120.

## Why the pipeline does this

- Mint: a new task's project is the cluster's top place key
  (`segmenter::decide`, the `Target::New` block). The project is a
  by-product, never a constraint on what the task may later absorb.
- Profiles: `live_profiles` reads the `task_evidence` cache, and the cache
  fills only for tasks a placement or correction touched
  (`refresh_task_evidence` from `place_window` and `rescore_day`). A
  declared task starts with nothing.
- Closing: `store_segments` reopened any closed task a placement targeted;
  `live_profiles` kept closed tasks scoreable on `item`, `change`,
  `branch`, `session`, `event`.
- Concurrency: `session_target` walks best-consistent, ticket holder,
  best-in-repo, new-in-repo. Nothing in that order knows what James
  declared.
- The place veto (M33 chunk C) is negative only: it stops a task from
  taking another repo's segment. There is no positive rule that says who
  owns a repo.

## Shipped first (2026-09-09 morning, before this plan)

1. **Seed on declare.** `segmenter::seed_task_evidence` writes a declared
   task's place and ticket rows into the cache from the Home declare
   form; `place_window` seeds any open declared task the cache never saw
   (`storage::unseeded_user_tasks`) so tasks declared before the fix, or
   through the proposals and MCP paths, catch up on the next tick.
2. **Sticky close.** Migration 028 adds `tasks.closed_by` ('user' |
   'auto'; NULL from before counts as user). `close_task`, merges and
   folds write 'user'; the idle autoclose writes 'auto'. A placement
   reopens only an autoclosed task, and only autoclosed tasks stay
   scoreable, on `item`, `change` and `event` — never on `branch` or
   `session`.
3. **Declared sink.** `storage::declared_sinks` is the newest open
   declared task per project. In `decide`, a segment whose dominant place
   has a sink goes to it (reason "declared in <place>"; the scorer's
   ranking stays underneath, so confidence and runner-up still show its
   view). In `session_target` the sink is checked first. The one override:
   a ticket the sink does not hold and another task does.

These make "declare a task, work in that repo, it goes there" true today.
They do not make projects first-class, and derived tasks still mint in a
repo with no declared task. That is M35.

## Design

### Projects are configuration, matched by rule

```toml
[[projects]]
name = "contoso"
repos = ["~/dev/contoso", "~/dev/mailer"]   # place anchors from cwd, editor path, AI session cwd
tickets = ["ACME"]                            # item anchor prefix
domains = ["contoso.atlassian.net", "app.contoso.com", "localhost:8000"]
titles = ["(?i)contoso", "club 1 basketball"]   # window-title regexes
apps = []                                      # whole apps (Slack workspace windows, a client's VPN)
derive = true                                  # mint derived sub-tasks inside this project
```

Resolution per span, first match wins: repo place → ticket prefix →
domain → title → app. An unmatched span shorter than `project_join_min`
between two spans of one project joins that project (the furniture rule
generalised to "a glance at a tab"). Everything else is `unfiled`.

The project of a span is stored (`spans.project`, migration 029) so
Home, the report and the self-score never re-match, and `chronicle
project rebuild` re-files history after a config edit. `chronicle
project test [--days 7]` prints minutes per project and the top unfiled
titles, which is how James tunes the matchers.

The Settings window gets a Projects card: one row per project, the
matchers as editable chips, "test against the last 7 days" inline, and
the six `git_repos` entries pre-filled as six projects on first run.

### Tasks live inside a project

- `tasks.project` must name a configured project or be NULL (unfiled).
  Rename's project field becomes a picker. Existing rows are mapped by
  name; the unknowns are listed once for James to map or leave.
- Every project has an implicit general task (`source = 'project'`,
  label "<project> · general"), created on first use, never closed,
  hidden from the task list but present in reports as the project's
  unassigned time. Time with a project and no better task goes there.
  Unfiled time keeps today's "unassigned" run behaviour.
- **Sink order inside a project:** the declared task James marked
  current (a "current" flag; default the newest declared) → a task
  holding the segment's ticket → the best-scoring open task of the
  project → a new derived task if `derive = true` and the cluster clears
  `new_task_min` → the general task.
- Derived tasks never cross projects: the candidate set for a segment is
  the segment project's tasks, period. The place veto and the m33 cross-
  project evidence rules become unnecessary and are removed.
- Closing: as shipped. A closed task's evidence never reopens it; the
  autoclose stays for derived tasks.

### Derivation runs per project

`place_window` partitions the window's spans by project, segments each
partition on its own, and scores against that project's profiles. Spans
of different projects interleaved minute by minute are no longer one
segment fighting over a winner; each project's minutes are its own
segments with their own share of the window, which is what M33 chunk B
tried to recover after the fact. Chunk B's `segment_switch_mode` switch
is removed.

The concurrency split reduces to: a live session's project is its cwd's
project. Its prompt and write minutes go to that project's sink through
the same order as above. `session_target`'s four-step search goes.

### The UI

**Home groups by project.** A project header (name, today's minutes, a
live dot when a session or a window of it is on screen now, the current
task and a "set current" picker), then its tasks with minutes and the
same … menu as today, then unfiled at the bottom. Declaring a task from a
project header pre-fills the project. Collapsing a project persists.

**Task manager** (new view, from Home's header): every task in a table —
project, label, status, source, minutes today and this week, last
activity — with filters (open / closed / all, per project, declared /
derived), inline rename, project move, multi-select for merge, close,
reopen and delete-derived, and "make current". This is the management UI
James asked for; the Home row menus stay for one-off edits.

**Timeline** colours blocks by project and adds a project row above the
task lanes. **Reports, standup and the self-score** group by project then
task; the self-score gains "cross-project placements: 0" as an invariant
and "unfiled minutes" as the number to drive down.

### CLI

`chronicle project list | test | rebuild`, `chronicle task list
[--project P] [--all]`, `chronicle task current <id>`, `chronicle task
close <id>` (the close the UI does, for scripts and for me).

### What the models still do

Name derived sub-tasks inside a project, write descriptions and standup
narratives, suggest tasks from unfiled runs. Never choose a project.

## Chunks and gates

0. **Projects and matching.** Config schema, matcher, `spans.project`
   (migration 029), `chronicle project test | rebuild`, Settings card.
   Gate: the last 7 days file ≥ 90 % of focus minutes into a project with
   James's matchers; `project test` names the top unfiled titles.
1. **Sinks and per-project candidates.** General task, current flag,
   sink order, candidate set limited to the segment's project, place veto
   retired. Gate: replaying 2026-09-08 10:30–12:49 and 2026-09-09
   09:00–09:40 yields no interval whose span project differs from its
   task project; the chronicle sessions land on 145, the contoso ones on
   143; tasks 149 and 150 would not have been minted.
2. **Per-project derivation and the trivial split.** Partitioned
   `place_window`, sessions by cwd, chunk B removed. Gate: the M33 chunk
   B gate (09-08 10:30–12:49 → chronicle ≥ 20 %, contoso ≥ 20 %, fabrikam-web
   ≥ 20 %, no project > 60 %) holds with no switch; single-project days
   keep placements within 2 %; weekly totals still sum to captured time.
3. **Home by project and the task manager.** Gate: a screenshot loop of
   Home and the manager at the standalone-UI scale; every action the row
   menu offers is reachable in the manager with multi-select.
4. **Diagnostics by project.** Timeline colour and row, report and
   standup grouping, self-score invariants, CLI. Gate: `chronicle report`
   for 09-08 reads project → task → minutes and sums to the day.
5. **Cleanup.** Placeholder labels from window titles go (a new task
   inside a project is "<project> · new work" until named); pending
   `name_task` jobs for deleted tasks are dropped; the m33 cross-project
   evidence rules and `segment_switch_mode` are deleted with their tests.

Order 0 → 1 → 2 → 3 → 4 → 5. Chunks 0 and 1 are the accuracy win and
ship first; 3 is the UI James asked for and can start once 0's config
exists.

## Open questions for James

- One silo or four for contoso, mailer, admin-api and
  acme-ai-agent-backend? The ticket prefixes suggest two (ACME,
  ACAI). The config above lets either be true.
- Default `derive = true` per project, or off until a project earns it?
  Off means a project with no declared task logs to its general task
  only; that is the "just group my time" mode.
- What the general task is called in reports.

## Out of scope

Threads finer than a project (M33 direction A stays shelved), multiple
users, tickets as projects, and any change to capture.
