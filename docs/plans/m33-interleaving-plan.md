# M33 — interleaved projects (draft, after M32)

Status: problem statement and candidate fixes, written 2026-09-08 while
M32 chunk 0 was landing; M32 chunks 0–6 shipped the same day. Chunk C
shipped 2026-09-08 evening (see the end); B and A are open. The one
dependency on M32 is chunk 3's `intervals.share` column (migration 026),
which this plan generalises.

## The short version

James runs two to four projects at once — one Claude session per repo,
plus tabs — and switches between them every minute or two all day.
Chronicle assumes the opposite: that a stretch of focus belongs to one
task until a *contiguous* run of foreign work is long enough to cut it.
Interleaved work never produces that run, so the whole stretch goes to
whichever task won the first segment, and that task then learns the other
projects' anchors as its own evidence. Today the report gave the entire
morning to one project while three repos took commits inside a 32-minute
window.

## What today says

`chronicle report --day 2026-09-08 --format md` at 12:49:

| project | task | 2026-09-08 |
|---|---|---|
| fabrikam-web | new fabrikam-web updates for client site | 1h57m |

Commits landed in three repos over the same stretch:

| time | repo | commit |
|---|---|---|
| 12:11 | fabrikam-web | `feat(content): client revise pass 1 …` |
| 12:15 | chronicle | four `m32 chunk 0` commits |
| 12:37–12:43 | contoso | four `ACME-11382:` test commits |

Every derived interval landed on task 120 (fabrikam-web), including the one
whose own reason string names the contoso ticket:

| interval | kind | conf | reason |
|---|---|---|---|
| 10:30–11:00 | read | 0.68 | `main · 82270dc0…` |
| 11:03–11:33 | agent | 0.71 | `staging · main` |
| 11:33–11:35 | agent | 0.66 | `main · bea66021…#2` |
| 11:53–12:18 | agent | 0.50 | `between new fabrikam-web updates for client site and a new task` |
| 12:19–12:49 | agent | 0.61 | `ACME-11382 · ACME-11382` |

Anchors and focus inside those windows (spans overlapping each window,
anchor counts from `span_anchors`):

- **10:30–11:00** — place=mailer ×14, place=chronicle ×11,
  place=contoso ×8, place=fabrikam-web ×3. Focus: a Telnyx 10DLC tab 8 min,
  `urls.py (~/dev/mailer)` 3 min, Chronicle 2 min, `tasks.py
  (~/dev/contoso)` 2 min. fabrikam-web is the *smallest* place and won.
- **11:03–11:33** — place=fabrikam-web ×18 (the `SEO improvements` session),
  place=mailer ×10, place=chronicle ×7 (`m32-attention-plan.md`,
  `chronicle-m32-attention.md`). The one window fabrikam-web should own.
- **11:53–12:18** — place=chronicle ×8, item=ACME-11382 ×8,
  place=fabrikam-web ×7, place=contoso ×5. Three-way tie; the segmenter saw a
  possible new task and folded it in at 0.50.
- **12:19–12:49** — item=ACME-11382 ×23, branch=ACME-11382 ×21,
  place=contoso ×21, place=chronicle ×16, `AUTOMATED_TESTS.md` ×16,
  `test_sms_inbox.py` ×16. No fabrikam-web anchor in the top ten. Still
  fabrikam-web.

The switch cadence is the tell: 91 `context-switching` spans (7 min) and
184 `focus` spans (113 min) by 12:49, so the median focus span is well
under a minute. Nothing foreign stays on screen for the three contiguous
minutes a cut needs.

### The profile has already learned the wrong thing

Task 120 is a hand-made fabrikam-web task from 09-04 (the M32 plan, cause 4,
already flagged it winning on bare `staging`/`main` anchors). By 12:49
today its `task_evidence` (243 rows, all `source = interval`) includes:

| kind | value | minutes |
|---|---|---|
| branch | `ACME-11382` | 20.5 |
| branch | `staging` | 25.7 |
| branch | `main` | 40.5 |
| doc | `AUTOMATED_TESTS.md` | 17.0 |
| doc | `ACME-11382.json` | 17.0 |
| doc | `SEO improvements and broken link handling` | 17.6 |
| doc | `MEMORY.md` | 13.8 |
| doc | `SmsWelcomeSent concurrent row access` | 10.4 |
| doc | `chronicle-m32-attention.md` | 8.0 |
| doc | `10DLC Campaign Registration` | 8.4 |
| change | `northwind/contoso#9480` | 0.4 |

Only `SEO improvements…` and the S3/Render/fabrikam.example rows are fabrikam-web.
Every later window that carries `ACME-11382` or `MEMORY.md` now scores
*higher* for fabrikam-web than it did this morning. This is the feedback loop
the M32 plan's cause 4 describes, and interleaving feeds it every day.

## Why the pipeline does this

1. **One target per segment.** `crates/core/src/segmenter.rs:378`
   `Placement` carries one `target`; `decide` (`:508`) scores a whole
   segment and keeps the winner. There is no notion of two tasks sharing
   a window except M32 chunk 3's agent/supervise split, which fires only
   when two *tool sessions* wrote within 2 min and splits by prompt
   count.
2. **Cuts need a contiguous foreign run.** `SegParams::switch_min`
   (`crates/core/src/segmenter.rs:69`, default 3 min, config
   `segment_switch_min`): "a run of spans sharing nothing with the segment
   must reach this many minutes before it becomes a segment of its own;
   shorter excursions fold back in." With sub-minute focus spans cycling
   across three repos, no run reaches 3 min, so every excursion folds
   and the segment stretches to the next AFK gap (`AFK_GAP_MS`, `:28`,
   5 min) or the 30-minute batch edge.
3. **Absorption after the win.** The winning task's `task_evidence`
   takes every anchor in the segment, including the losers' places and
   items (`task_evidence.source = 'interval'`). A user task with thin
   declared evidence is the worst case: it wins on generic branches,
   then inherits the whole desk.
4. **Ties resolve by recency and profile weight, not by place.** In the
   12:19–12:49 window `place=contoso` outnumbers `place=fabrikam-web` and the
   task's own project is `fabrikam-web`, yet nothing in `decide` treats
   place-versus-project disagreement as a veto.

## Design directions

Ranked by how much they recover per line changed. C is small enough to
ship as an M32 side-fix if chunk 3 is being touched anyway.

### C. Place veto and no cross-project absorption (small, first)

- In `decide`: a candidate task whose `project` is set loses when the
  segment's dominant `place` anchor is a different known project, unless
  the task carries that place as *declared* evidence. Tie → "new task"
  verdict, not the incumbent.
- In the evidence writer: an interval only feeds `place`/`branch`/`item`
  anchors into `task_evidence` when they agree with the task's project
  (or the task has no project). Cross-project docs and domains still
  flow, so shared references keep working.
- One-off: a `chronicle evidence prune --cross-project` (or a rescore of
  the week) to strip the rows task 120 and its siblings picked up.
- Gate: rescoring 2026-09-08 10:30–12:49 gives task 120 no interval whose
  dominant place is contoso or chronicle; task 120's evidence loses every
  `ACME-11382`, `AUTOMATED_TESTS.md`, `MEMORY.md` row.
- Non-goals: no change to how segments are cut. This stops the bleeding,
  it does not make the report right.

### B. Non-contiguous switch accounting (medium)

- `switch_min` counts *accumulated* foreign minutes per signature inside
  the current segment instead of one contiguous run. When any foreign
  signature reaches `switch_min` in total, the segment splits into one
  segment per signature that cleared the bar; the leftovers stay with
  the incumbent.
- Each split segment is placed by `decide` on its own and written as its
  own interval, with `share` = its focus share of the window so totals
  still sum to captured time (this is the generalisation of chunk 3's
  `share`; chunk 3's prompt-count split becomes one special case).
- Timeline: overlapping intervals render as stacked bars in one lane,
  the way chunk 3 planned for the agents lane.
- Gate: replay 2026-09-08 10:30–12:49 → chronicle ≥ 20 %, contoso ≥ 20 %,
  fabrikam-web ≥ 20 %, no project > 60 %; the M32 chunk 3 gate (09-03
  15:00–16:44, chronicle ≥ 35 %, contoso+mailer ≤ 60 %) still holds;
  weekly totals still sum to captured time. Single-project days (09-02)
  keep their current placements within 2 %.
- Risk: a "desk" of related tabs (Jira + repo + docs for one ticket) has
  several signatures; if they split, one ticket becomes three rows. Fold
  rule: signatures that share a `place` or `item` stay together.

### A. Thread model (large, later)

Model the desk as N concurrent threads, one per project with a live tool
session or an open terminal in that repo, and attribute every focus span
to the thread it belongs to at capture time (place from cwd or path,
session id, item from the title). Segments become per-thread; the
"segment" of today is the union. This is what a multi-project user
actually does, and it makes the agent lane, the supervision split and
the report all fall out of one structure — but it changes the storage
model (spans gain a thread id, intervals become per-thread) and is a
milestone on its own. B gets most of the value with the tables we have.

## Order and timing

1. Ship M32 chunks in flight. Do not touch `decide` or the evidence writer
   under two sessions at once.
2. C as the first M33 piece, or folded into M32 chunk 3 if that chunk is
   open when this is read (both edit `reconcile`/`place_window` and the
   share column).
3. B behind `segment_switch_min` and a new `segment_switch_mode =
   contiguous | accumulated` config so the old cut rule stays selectable
   for the gate comparison.
4. A only if B leaves the report wrong on a real week.

## Also in scope (2026-09-08 follow-ups)

### The Chronicle window is furniture, not work

Since 09-01 the app's own window (`app = chronicle`, title `Chronicle`)
holds 130.8 of 1928.4 focus minutes (6.8 %). `extract::family` files it
under `Family::Other`, so `span_kind` calls it `read` and it counts toward
whichever task wins the segment — the app's own task-management time
feeds the profile of the task being reviewed. Fix: treat the self window
like a distraction in `Segment::from_spans_skipping` (skipped for scoring,
carries no anchors, feeds no evidence) but report it as one fixed `admin`
line, "Chronicle", outside any task, so the day still sums to captured
time. Stopgap until then: `distraction_patterns` can match it, at the cost
of the time reading as `break`.

### Editing a declared task's label and project

Already exists on the timeline task card ("rename" ghost button,
`crates/app/src/ui/timeline.rs:1994`, and the "…" menu at `:2045`): label,
project and description, saved through `storage::insert_correction`
(`crates/core/src/storage.rs:2535`), which also records the rename as a
correction for few-shot. It is not gated on `declared`. Missing: the Home
list rows (`crates/app/src/ui/home.rs:631`), where declared tasks are
created, have no rename affordance, and there is no CLI. Add rename to
the Home row menu; a `chronicle task rename <id> --label --project` is
cheap on top of the same function.

## Out of scope

Per-window focus timestamps from Claude Code itself, screen scraping,
cloud models, Wayland.

## Shipped (2026-09-08, chunk C)

One commit on `main` (core, app), 277 tests (2 new), clippy unchanged
(the five pre-existing warnings), no migration.

- **Place veto** (`segmenter::place_veto`, applied in `decide` right
  after `profile::score`): a candidate whose task has a project loses
  when the segment's dominant place (the `place` key with the most focus
  minutes) is another repo, unless its own project ties for the top or
  the segment carries a ticket the task's *label* names. The verdict
  settles again on what remains (`Verdict::retain`: best, margin over
  the runner-up with "new task" as a runner, confident on the same rule
  as `score`), so a vetoed incumbent is no runner-up either and the
  segment goes to the next task in its repo or clusters into a new one.
  A task with no project, or a segment with no place anchor, is never
  vetoed. `decide` now takes the tasks' projects; `place_window` fetches
  them before scoring, `place_dry` and the fixture bench pass theirs.
- **Declared keys on the profile** (`Profile.declared`, filled from
  `Source::Declared` rows by `from_rows`): the label's tickets and the
  project. The veto's ticket exception reads these, not the interval
  rows — the whole point is that what an interval taught cannot excuse
  the next one. `session_target` (m32 chunk 3) still reads `minutes` for
  its own ticket rule; untouched.
- **No cross-project absorption** (`profile::keys_in_owned`): under a
  task with a project, a span whose place is another repo feeds only its
  repo-free keys — documents, sites, people, calendar entries, title
  terms. `Key::repo_bound` names what is held back: place, branch,
  item, **change and session** as well as the plan's three, since a PR
  (`northwind/contoso#9480`) and a tool session are as repo-bound as a
  branch. A span with no place anchor (a Jira tab, a doc) is not
  constrained, nor is a task with no project. Corrections still spread
  over every key of the range: a human assignment is not an absorption.
- **No prune command**: `task_evidence` is a cache, so `backfill-evidence`
  (and the daily rebuild) strips the rows. Run on the live DB after
  install.
- **`bench --window START..END --live`**: the same dry placement
  against `storage::live_profiles` and `task_projects` — the cache as the
  daemon's tick reads it — instead of profiles rebuilt as of the
  window's start, which for this morning held only four tasks and no
  task 120 at all.

Gate, `bench --window 2026-09-08T10:30..2026-09-08T12:49 --live` on a
sandbox copy of the live DB (profiles as they stand, 120 of 139 min
placed, the rest AFK):

| build | task 120 (fabrikam-web) | contoso | chronicle | mailer |
|---|---|---|---|---|
| chunk 6 live rows (plan table) | 4 rows, every minute | 0 | 0 | 0 |
| chunk C, cache as it was | 47 min: `fabrikam-web@main` segments and its session's split shares | 37 min, new task `ACME-11382 · contoso@ACME-11382` | 14 min, task 126 | 22 min, new task |
| chunk C, cache rebuilt | 47 min, same rows | 32 min | 23 min (126 also takes two unsure reads) | 18 min |

Task 120 takes no segment whose dominant place is contoso, chronicle or
mailer on either cache. Its evidence after the rebuild: `place`
chronicle (19.8 min), contoso (21.5) and mailer (46.2) gone;
`ACME-11382` 18.7 → 1.3 min (a placeless Jira tab under one of its
intervals — not zero, as the plan's gate wanted, but under the
saturation floor); `AUTOMATED_TESTS.md` (2.6) and `MEMORY.md` (6.8)
stay, as the rule keeps documents. Rows 232 → 214; 328 rebuilt in all.
The as-of profiles (`--window` without `--live`) place the window with
no incumbent at all: every stretch is a new task in its own repo, task
112 and 113 (northwind-memberships, harrypotter.com) no longer take
mailer and chronicle segments.

Deviation: the plan's writer rule named place, branch and item; change
and session are held back too (above). The plan's "known project" test
for the dominant place is dropped — any place that differs from the
task's project vetoes, since a repo with no task yet is exactly the case
where the incumbent used to win.

Open: this morning's rows 592–595 stay on task 120 until a re-score (a
correction on the day triggers `rescore_day`); reconciled batches are
not re-placed by an install. A placeless span carrying a ticket still
feeds whatever task wins the stretch. The mailer/chronicle stretches
cluster as one new task labelled `mailer@staging · chronicle@main`
because the cut rule still needs a contiguous foreign run — that is B.

## Shipped (2026-09-08, chunk B) — behind a switch, off by default

One commit on `main` (core), 278 tests (1 new), clippy unchanged, no
migration. `segment_switch_mode = "contiguous" | "accumulated"` in
`config.toml`, default **contiguous**: the live daemon cuts and places
exactly as chunk C left it (row-for-row identical on the three gate
windows). The gate below is why it is off.

- **Strands** (`Seg.strands`, accumulated mode only): a foreign run that
  folds back into the open segment, and a short excursion
  `fold_excursions` absorbs, also become strands by what they share (an
  anchor in common or a leading word; the run keeps one piece per span
  so a run that mixed two repos parts again). The segment's cuts do not
  change — the contiguous run, the strong-anchor swap, the AFK gap and
  the excursion fold all stay — only what a segment turns into once it
  closes.
- **Unravel** (`Seg::unravel`): every strand with `segment_switch_min`
  minutes in all becomes its own `Seg` over the segment's range with
  `share` = its focus minutes over the segment's; the incumbent keeps
  the rest (distractions, bare spans, strands under the bar) or, when
  itself under the bar, folds into the largest strand. `Seg.ids` names
  each row's spans.
- **`decide`** scores a strand on its own spans, gives it the kind of
  those spans, never folds it into a sandwiching task (it cleared its
  own bar), and adds the shares of same-range rows that land on one
  target; whole rows merge as before. `split_concurrent` leaves a strand
  alone (`Placement.strand`) and scales the incumbent's session shares
  by its own share, so a range still sums to 1.

Gate, sandbox copy of the live DB, `bench --window`, accumulated versus
contiguous (chunk C), placed minutes by project, with the on-screen
minutes per `place` anchor for the window as the yardstick:

| window | on screen (place) | contiguous | accumulated |
|---|---|---|---|
| 09-08 10:30–12:49 `--live` (139 min; 57 placeless) | fabrikam-web 27, contoso 23, chronicle 15.5, mailer 13 | fabrikam-web 47, contoso 32, chronicle 23, mailer 18 | **identical** |
| 09-03 15:00–16:44 (104 min; 50 placeless) | contoso 23, chronicle 15.5, mailer 9 | mailer 57 (54.6 %), chronicle 40 (38.9 %) | mailer 59 (56.8 %), chronicle 32 (**31.1 %**) |
| 09-02 09:00–18:00 (540 min; 266 placeless) | chronicle 124, agent-backend 58, mailer 57 | chronicle 228, ACAI 215 | chronicle 232, ACAI 204 (within 2 points) |

- The plan's 09-08 target (chronicle, contoso and fabrikam-web each ≥ 20 %)
  was written when the whole window sat on task 120; chronicle is 11.5 %
  of the window on screen, so 20 % is not a target the cut can reach
  without inventing time. Chunk C plus the existing strong-swap cut and
  excursion fold already give each repo its on-screen minutes plus a
  share of the placeless ones; accumulated mode finds no strand over
  the bar there — the interleaved bits are under three minutes per
  segment (the chronicle bits inside 10:48–10:54 total 73 s).
- 09-03 fails the M32 chunk 3 gate (chronicle ≥ 35 %) by four points,
  not through the strands themselves: the 15:19–15:36 segment unravels
  (a 22 % `chronicle@main · ACME-11382` strand, unplaced under
  `new_task_min`), so its incumbent is no longer a whole row and no
  longer merges with 15:00–15:19, which stays a `plan` row at 100 % on
  task 85 instead of joining an `agent` row the session split gave 30 %
  chronicle. Which of the two is right for a Jira-reading stretch with
  chronicle sessions writing behind it is a judgement the gate cannot
  make; it was calibrated on the merged behaviour.
- 09-02 moves within two points; the one visible change is the same
  kind of unravelled agent stretch (15:27–16:10, a 14 % agent-backend
  strand).

Decision: shipped behind the switch, default off. Flip `segment_switch_mode
= "accumulated"` to try it live; `bench --window … --live` prints the
strands as rows under 100 % sharing a range. Chunk A (per-thread model)
stays on the shelf: on these windows the report's remaining error is the
placeless time (41–51 % of each window: browser tabs, the Chronicle
window, terminals without a cwd), not the cut.

Open: a strand under `new_task_min` that the scorer calls new is dropped
(unplaced) rather than folded, so a range's rows can sum to less than 1;
labels of a segment with strands still come from all its keys (the
excursion fold keeps merging them, for cut parity).

## Shipped (2026-09-08, the Chronicle window is furniture)

- `evidence::is_self_window(app)` (`app` equals `chronicle`, case-free;
  the live DB has 371 such spans, all titled `Chronicle`, 198 minutes) and
  `evidence::is_furniture(app, title, patterns)`, the self window or a
  distraction. Every site that skipped distractions now skips furniture:
  `segment()` stretches the open segment instead of cutting (the title's
  leading word is `chronicle`, so before this the window could cut, and
  seed, the chronicle repo's segments), `Segment::from_spans_skipping`
  keeps its minutes but drops its keys and vector, `prepass` and
  `proposals` never place or seed from a run that starts in it.
- `span_kind` returns `admin` for the window (the kind existed in `KINDS`
  and migration 021 unused). `kind_of` ranks it under every work kind and
  above `break`; `hands_split` counts it in neither hand.
- The report gets one fixed line, `report::self_line`: "Chronicle, its own
  window: 2h10m (admin, inside the rows above)", from
  `storage::self_window_ms` (non-AFK spans, clipped to the range), in
  the markdown report, `chronicle status`'s report and the Reports view
  under the hands split. `RangeReport.self_ms` is filled by the callers
  like `underived_ms`; `build` leaves it 0.
- Tests: `the_self_window_stretches_and_carries_no_evidence`,
  `kinds_follow_family_and_anchors` (admin cases), `hands_split_groups_kinds`,
  `md_prints_gap_and_underived_lines`, `self_window_ms_sums_the_apps_own_spans_clipped`,
  `is_furniture` in `evidence::tests`. Workspace green, clippy clean.

Deviation: the plan said "outside any task, so the day still sums to
captured time". The minutes stay inside the task rows. Taking them out
would mean cutting an interval around every glance at the window, and
the window is opened for seconds between stretches all day; the report's
total would fragment into slivers the tidy pass then folds back. The
line names the minutes instead and says where they sit. Existing
intervals keep their old kind until a re-score.
