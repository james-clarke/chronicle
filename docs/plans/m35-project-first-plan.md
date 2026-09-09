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

## Shipped: chunk 0 (2026-09-09 afternoon)

- Config: `[[projects]]` (`name`, `repos`, `tickets`, `domains`, `titles`,
  `apps`, `derive`) and `project_join_min` (default 2) in
  `crates/core/src/config.rs`; `Config::projects_effective` is one project
  per `git_repos` entry when none is written, so first run needs no edit.
- Matcher: `crates/core/src/project.rs`. Instances are the configured
  paths plus every worktree the shared git dir lists (`worktrees_of`);
  identity is the remote as `host/org/repo` (`remote_of`, `remote_id`).
  Order per span: a path the title shows under an instance (longest
  wins) → a place or `repo@branch` anchor naming an instance folder → an
  item key by ticket prefix, `org/repo#n` by remote slug or folder, or a
  branch carrying `<prefix>-<n>` in any case → a domain anchor equal to or
  under a listed site → title regex → app. `join_short` is the glance
  rule. `localhost:<port>` needs no rule: the ports collector already
  turns it into the repo's place anchor.
- Storage: migration 029 `spans.project`; `storage::file_spans` runs the
  matcher over stored anchors after `anchor_spans` on every tail refresh
  (`sessionizer::refresh`) and from `chronicle project rebuild`.
- CLI: `chronicle project list | test [--days 7] [--top 15] | rebuild
  [--days N]` (`crates/app/src/project.rs`). `test` matches fresh from
  config, never the stored column, and lists unfiled places (the
  discovery list: repos no project claims), unfiled domains and titles.
- Settings › Projects (`crates/app/src/ui/projects.rs`): a row per project
  with the lists as comma-separated fields, derive, remove, add, the join
  minutes, and "test last 7 days" rendering the same report from the rows
  as edited. Save validates the regexes and rejects duplicate names; the
  per-repo defaults are not written unless edited.
- Gate: on a sandbox copy of the live DB, 2026-09-02 → 09-09, 1944 focus
  minutes. Per-repo defaults file 63.7 %; the config below (written to
  James's config.toml at install) files **90.0 %**. What stays unfiled is
  personal browsing (Wordle, YouTube, a sorting quiz), New Tab, Google Meet
  without a title, a markdown viewer tab (58 min) and Memtime (15 min).
  Two silos as answered: `acme` (contoso, mailer, admin-api; ACME;
  atlassian.net, acmeapi.example, telnyx.com, circleci.com, heroku.com,
  amazon.com; titles ` - PB - `, `acme`, `^Meet - `, `mailer|contoso`)
  and `acme-ai` (ACAI); `chronicle` (chronicled.dev, render.com,
  onrender.com, porkbun.com, title `chronicle`, app `chronicle`),
  `fabrikam-web` (fabrikam.example, `FABRIKAM`), `portfolio`, and `sprog` for
  `~/dev/sprog.io`, which `test` surfaced as the top unfiled place.
- Not in this chunk: proposing discovered repos in Home (chunk 3 with the
  Home-by-project work; `project test` lists them today), `#123`
  repo-scoped keys as a configured rule (the matcher resolves
  `org/repo#n` from anchors already), and the Settings chips (fields
  instead).

## Shipped: chunk 1 (2026-09-09 afternoon)

- Spans carry their project into placement: `AnchoredSpan.project` reads
  `spans.project`; a segment's project is the one its spans spent the most
  time in (Chronicle's own window and distractions aside, filed wins a
  tie), `segmenter::project_of`.
- Candidates are the segment project's tasks only (`by_project`; an
  unfiled segment scores against the tasks with no project). The scorer's
  shared-key discount is therefore per project. `place_veto` and
  `declared_sink` are gone; the m33 chunk C evidence rules
  (`profile::keys_in_owned`, `Profile.declared`) stay until chunk 5.
- Tasks resolve to configured names: `Matcher::resolve` maps a name in any
  case, or a repo folder the project has, to the project
  (`contoso → acme`); `project::normalize_projects` does it for the
  task map every window, `storage::normalize_task_projects` rewrites the
  rows from `chronicle project rebuild` and the daily tick (after
  `infer_projects`). A project no rule knows counts as unfiled for
  placement; `rebuild` lists those open tasks (142 "ai server" and one
  derived row at install) for James to map with `task rename --project`,
  which now accepts configured names or their repo folders only.
- Sink order (`segmenter::Sinks`, `sink_order`, and the same in
  `session_target`): the current declared task of the project (migration
  030 `tasks.current`, `chronicle task current <id>`; else the newest open
  declared one) unless the segment carries a ticket it does not hold and
  another task of the project does → that ticket holder → the scorer's
  best among the project's tasks → a new task inside the project (with
  the project's name, never a place key; clusters never cross projects;
  only when the project derives) → the project's general task
  (`Target::General`, `storage::general_task`: `source = 'project'`,
  label "<project>: other work", created on first use, never closed by
  `close_task`, out of `open_tasks` and `live_profiles`, its evidence never
  refreshed; reason "other work in <project>"). Unfiled new work stays
  unplaced as before.
- The concurrency split: a session's project is the one its own spans are
  filed into, else what the rules make of its scope anchors and title
  (`session_project`); it scores against that project's tasks and follows
  the same sink order; the four-step search is gone. A session with no
  project scores against the tasks with none and, called new, stays with
  the segment's target.
- `bench --window` prints "cross-project whole rows: N of M" (a row
  sharing its range is one session's, placed inside that session's
  project, so it is not judged against the range's mixed spans) and shows
  a general target as "<project>: other work".
- Why task 152 minted beside declared 145 (the amendment): batch 119
  (09:30:57–10:00:59, reconciled 10:01) cut 09:30:57–09:34:15 as one
  segment — portfolio 87 s, chronicle 57 s, contoso 40 s. Its dominant
  place was portfolio, whose sink 144 existed, but the 40-second glance at
  the contoso terminal carried `ACME-11032`, which 144 did not hold and
  143, 145, 149 and 150 did: the one override fired and the sink was
  dropped. The scorer then called the segment new (126 had just been
  closed by hand, so 09:49–09:57 was new as well and akin), the cluster
  cleared `new_task_min`, and its top place key — chronicle, because the
  portfolio spans before 09:30:57 carried `place=contoso` from a
  mis-resolved session cwd — became the project. Under chunk 1 the
  segment's project is portfolio, the ticket rule looks inside portfolio
  only, and the replay of batch 119 reads 09:30–09:34 "declared in
  portfolio" [144].
- Gate, on a sandbox copy of the live DB after `project rebuild` (26 tasks
  renamed to configured projects, 4662 spans filed), `bench --window
  --live`: 2026-09-08 10:30–12:49 places 122 of 139 min on 143, 120 and
  145 with **0 of 10** whole rows cross-project; 2026-09-09 09:00–09:40
  places 39 of 40 min, **0 of 2**, the chronicle sessions on 145, the
  contoso ones on 143, the portfolio one on 144, and 09:25–09:29 reads
  "declared in acme" [143] where 149 and 150 were minted.
- Not in this chunk: partitioning the window by project (chunk 2; the
  split rows still divide a mixed range among the sessions live in it),
  the general task's rendering in Home and reports (chunks 3 and 4 —
  today it is a task row named "<project>: other work"), a "set current"
  control in the UI (chunk 3; the CLI has it), the declared seed rows,
  which still write the project name as a `place` key.

## Shipped: chunk 2 (2026-09-09 afternoon)

- `decide` partitions the window's spans by `spans.project`
  (`segmenter::partition`; Chronicle's own window and a distraction go
  with the span before them, else after — time on what they interrupted,
  as a glance always was), cuts each project's spans with `segment` on
  their own, scores each segment against its project's tasks, and gives
  every row a `share`: its focus minutes over every span's inside its
  range, exactly 1 when the project was alone on screen. Interleaved
  projects are each their own rows over the same stretch. Clustering, the
  excursion fold (3b) and the whole-row merge stay inside one partition;
  rows come out in time order.
- The concurrency split is per project: a row is its target's project,
  and only the sessions of that project (their spans' filing, else their
  scope through the rules) divide it; a contoso session never touches a
  chronicle row. The four-step search was already gone in chunk 1.
- Chunk B removed: `Seg.strands`, `fold_run`, `unravel`,
  `SegParams.accumulate`, `Placement.strand` and the config key
  `segment_switch_mode` (not set in James's config; `Config` denies
  unknown fields, so a config that still carries it would need the line
  removed). `segmenter::project_of` went with it — a segment's project is
  its partition.
- Gate (sandbox copy, `bench --window --live`, chunk 1 binary vs this):
  09-08 10:30–12:49 on screen acme 52 % / fabrikam-web 25 % / chronicle
  15 % (self window 6 %, unfiled 2 %); placed acme 47.5 / fabrikam-web
  23.7 / chronicle 15.5 (chunk 1: 47.6 / 23.3 / 17.1), 121 of 139 min
  (chunk 1: 122), 0 of 7 whole rows cross-project. The M33 chunk B gate's
  "≥ 20 % chronicle" was never reachable on this window (chronicle is 15 %
  of the screen); the placements now follow the screen within 4 points.
  09-09 09:00–09:40 on screen acme 39 / portfolio 29 / chronicle 20;
  placed 41.0 / 32.6 / 22.9 (chunk 1: 33.5 / 33.9 / 30.8), 39 of 40 min.
  Single-project hour 09-03 07:00–08:00: identical before and after (34 of
  60 min, one whole row, share 1). Weekly totals were not replayed; the
  three windows place the same minutes as before within 1 min.
- Not in this chunk: the report's and timeline's reading of overlapping
  rows with shares (they already multiply by `share`, as the split rows
  needed since m32 chunk 3; the timeline draws them as the split rows).

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

## Amendments (2026-09-09, from `dev-tools-direction.md`)

Proposed answers to the open questions: two silos by ticket prefix (ACME:
contoso, mailer, admin-api; ACAI: acme-ai-agent-backend); `derive =
true` by default; the general task is not shown as a task — the project
line carries "40 m not on a task" and reports say "<project>: other work".

Six changes to the chunks, argued in the direction doc:

- Chunk 0: project identity is the git remote (`host/org/repo`), with
  paths and `git worktree list` entries as instances; a path basename is
  what a place is today (`extract::place_from_path`,
  crates/core/src/extract.rs:1023; worktrees split at
  crates/capture/src/git.rs:47). Repos are discovered from every place
  source (AI session cwd, port cwd, editor heartbeat project, titles) and
  proposed in Home, not only pre-filled from `git_repos`. The matcher takes
  a path, not a basename. `localhost:<port>` leaves `domains` and resolves
  through the ports collector. Ticket keys add repo-scoped `#123` and match
  branch names case-insensitively.
- Chunk 1: replay the mint of task 152 (`chronicle@main · 94020a60…`,
  minted beside declared 145 after the sink fix) and say why the sink lost
  before the gate is called.
- Chunk 3: a Sources row per project (which collectors fed it this week).
- Chunk 5: "unfiled minutes" and "cross-project placements: 0" go on the
  site's proof block.

The milestones after this one are M36 (`m36-accuracy-plan.md`) and M37
(sources and connections, in the direction doc).
