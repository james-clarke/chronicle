# M29 — Legibility: rows people can read, a chat worth demoing

James (Sep 3, reviewing the site before handing builds to other people):
list views are "messy and not aligned"; feed rows have dots on some rows and
not others so nothing lines up; chips don't align; "to confirm" is visible
but not clickable and its meaning is unclear; the timeline task detail uses
italics that are hard to read; "in general we need more space for
characters, people are having a hard time reading"; the app-share text under
the week chart is unreadable; chat needs "a way better surface and
functionality to demo"; the m27 task description is wrong.

Site structural work (hero placement loop, interactive band, seven-step how
it works, sources split into today/next) shipped in 87cec49. This plan is
the app half. Screenshots for the site get re-shot at the end.

## What is wrong, concretely (from today's shots)

- Feed (`home.rs:748–940`): claimed rows get a dot, unclaimed rows get none,
  so titles start at two different x positions. Chips sit on the title line
  in varying counts (`to confirm` / `Terminator` + `unsorted` / nothing), so
  the title's available width changes row to row and long titles wrap while
  short ones don't. Duration is right-aligned in `NUM_COL` but the chips
  cluster floats.
- Unclaimed titles are raw window titles (`sam@workstation:~/dev/contoso`,
  `m27-chunks-2-7-execution`) and wrap over two lines at narrow width.
- `to confirm` (`home.rs:846`) is an amber pill with no affordance. The only
  explanation is under the `?` hover (`home.rs:1022`).
- Working-on rows use a left bar and no dot; recently-closed rows use a dot
  and no bar (`home.rs:595–614`), so the two lists under one heading have
  different left edges.
- Timeline task cards and the detail panel render the AI summary through
  `ai_summary_line` (`theme.rs:188–200`): Small, dim, italic. Three
  legibility penalties at once.
- Type scale (`theme.rs:242–268`): Body 13, Small 11.5, Caption 10.5. Most
  row metadata is Small or Caption. No user control over text size.
- Reports: the app-share and project-mix "legends" are one mono text line
  (`reports.rs:275–280`, `347–358`): `Terminator 75% · Google-chrome 12% ·
  chronicle 5%`. Reads as a sentence, not a table.
- Chat (`chat.rs`): one hint string, one empty-state example, replies are
  markdown text with no links back to the day, history hidden in a menu, no
  sign of what rows the answer was built from. Numbers in answers come from
  the model summing rows itself.

## One row grammar

Every list row in the app follows the same layout, left to right:

```
[time 44] [status 12] [title, wraps to 2 lines .............] [num 64] [… 28]
                      [meta line: state · source · context, Small, one line, elided]
                      [chips: project · ticket · declared]          (task rows only)
```

- **time column** — optional, mono, `TIME_COL` (44). Feed rows lead with
  the block start (`13:54`); task rows omit it. Gives the feed a spine.
- **status column** — always reserved, never collapsed. Dot for a task
  colour, hollow ring for unclaimed, bar for Working on. Titles therefore
  share one x in every list.
- **title line** carries only title, number and menu. No chips. Title gets
  the whole middle width; wraps to two lines before eliding.
- **meta line** replaces the chips-on-title-line habit for feed rows: state
  word first (`to confirm` / `kept` / `live` / `unsorted`, tinted), then
  `placed by a rule · branch ACME-11382 · Terminator: m27-c…`.
- **chips line** only on task rows (Working on, closed, timeline cards),
  fixed order project → ticket → declared/intent/stuck, left-aligned under
  the title, so chips align across rows.
- No italics anywhere. AI summaries render Body size in `TEXT_DIM`, wrap to
  two lines, full text on hover.

`ListRow` (`theme.rs:748–989`) already has title/dot/chips/num/bar/subtitle
and a right-to-left chip layout. The change is: add `.time()`, always
reserve the status slot, move chips under the title, and add a
`.meta(state, text)` line. Callers (`home.rs` task_row/feed_row,
`timeline.rs:953–1001` cards) keep their builder calls.

## Chunks

### 1. Row grammar (theme.rs, home.rs, timeline.rs)

- `ListRow::show`: reserved status slot; `time` column; chips under title;
  `meta` line with tinted state word; `lines(2)` default for titles.
- `ai_summary_line` → `summary_line`: Body, `TEXT_DIM`, no italics, two-line
  wrap, hover for full text. Both call sites in `timeline.rs`
  (`999`, `1204`).
- Feed rows: `.time(start)`, state word in meta, chips removed from title
  line. Unclaimed titles humanised: terminal titles `user@host:~/a/b` →
  `b · Terminator`; kebab/branch-looking titles keep as is but single line.
- Working on and Recently closed share one left edge: both use the status
  slot (bar for working, dot for closed) with identical title x.
- Timeline cards: same builder; detail panel title (`timeline.rs:1162`) gets
  the same chips-under-title treatment.
- Verification: home narrow (400 pt) and wide (≥720), timeline lanes with a
  task open, all rows' title x identical per list. Screenshots into
  `site/img/src/` for the site re-shoot.

### 2. Type scale and text size setting

- Small 11.5 → 12, Caption 10.5 → 11, Body stays 13; line spacing
  `extra_line` 1 → 2 (`theme.rs:271–277`).
- Settings › Appearance: text size S / M / L = `set_zoom_factor` 0.92 / 1.0
  / 1.1, persisted next to the density toggle (`theme.rs:123–135` already
  has Comfortable/Compact). Window default size scales with it so the widget
  still fits the corner.

### 3. `to confirm` and `unsorted` become buttons

- `to confirm` state word: hover tooltip "A rule placed this from the branch
  name. The model re-reads this batch in about 35 minutes. Click to keep it
  as it is." Click = confirm → state becomes `kept`, row never re-placed
  (same path as the `…` → keep action).
- `unsorted` state word: click opens the same assign popup as `…` → assign.
- `live` and `kept` get tooltips too, one sentence each. The long pipeline
  explainer stays under `?`.

### 4. Reports legends as rows

- Replace the two text lines with a compact legend: swatch · name · thin bar
  · duration · percent, top 5 plus "other", right-aligned numbers in mono.
  Same component for app share (`reports.rs:243`) and project mix
  (`reports.rs:318`). Hover on the bars stays as is.
- Tasks-by-project list below already uses a bar per row; align its columns
  with the new legend so the page reads as one table.

### 5. Chat surface

- Empty state: three suggested questions as buttons, built from real data
  (today's top task, the last call, yesterday): "what did I work on this
  morning?", "how long on ACME-11382 this week?", "what was I doing before
  the 09:40 call?". After each answer, two follow-ups the same way.
- Replies: time ranges `HH:MM–HH:MM` and known task labels become links.
  Time → timeline at that day/hour; task → task detail. Parsed after the
  markdown pass (`chat.rs:569–690`), no model change.
- "Read 14 blocks · Thu 3 Sep 08:00–14:56" footer under each answer, click
  to expand the exact rows given to the model (same idea as Settings'
  "show digest"). Builds trust for a first-run user.
- Input: multiline, Enter sends, Shift+Enter newline; stop button while
  streaming; copy button on answers.
- Wide mode (≥ `WIDE_W`): history as a left column instead of the menu
  (`chat.rs:320–394`).

### 6. Chat correctness: numbers come from SQL, not the model

- Context builder (`core/src/chat.rs:21–45`) detects quantity questions
  ("how long", "how much", "total", "per project") and appends a
  `RangeReport` table (`core/src/report.rs:65`) for the parsed range:
  task · project · total. Prompt (`prompts/chat_v1.txt`) told to quote
  totals from that table verbatim and never add rows itself.
- Range parsing gains "this week", "last week", "Tuesday", "yesterday
  afternoon", "before the call" (call = first mic block that day).
- Fixture test per phrase → expected range; one test that a totals answer
  contains the table's figure.

### 7. Wrong task description (m27)

Diagnosed. The describe pipeline is fine; the block is filed on the wrong
task.

- Task 100 `m27 task` (chronicle, created 11:59, closed 14:51) carries
  `external_ref = ACME-11382`, the same ref as task 85 `start dev on
  ACME-11382` (mailer). Both were open at 13:58 when the model placed
  interval 513 (12:17–12:30, confidence 0.9, batch 72) on task 100. The
  spans under that interval are Sublime Merge in `~/dev/mailer`,
  `rest-api.md … VIM`, and `✳ 11381 and 11374 merged and deployed`. The
  describe job (`ai_job.rs:97–108` → `storage::task_evidence_text`
  `storage.rs:1687–1711` → `describe.rs:44–58`) summarised exactly those
  spans. Accurate summary, wrong task.
- Enabling condition: two open tasks with one ticket ref. `open_task_by_ref`
  (`storage.rs:2547`) resolves a ref with `ORDER BY id LIMIT 1`; the derive
  ref/key anchoring has the same ambiguity.
- Fix: (a) declare/MCP path refuses or warns when an open task already owns
  the ref, offering "add to existing" instead; (b) ref → task resolution
  prefers the task whose project matches the span's repo/folder, then the
  most recently touched, never lowest id; (c) one-off: move interval 513 to
  task 85 and re-run describe for both. Test: two open tasks, same ref,
  spans in one's project folder → interval lands on that one.

### 8. Site re-shoot

- Home wide, feed narrow, timeline with task open, reports week, chat with
  suggested questions and one linked answer. Swap into `site/img/`, update
  alt text. Hero keeps `home-wide.webp` name so the loop needs no change.

## Order and size

1 → 2 → 3 are one PR each, small. 4 is CSS-like egui work, medium. 5 and 6
are the chat milestone, medium each; 6 can ship before 5. 7 depends on the
diagnosis. 8 last. Each chunk: tests + clippy, screenshot check, deploy via
`cargo install --locked` with the daemon unit stopped first.

## Out of scope

- New collectors (GitHub, Calendar, Slack). Listed on the site as next;
  separate plan.
- Standup body layout beyond what chunk 1 changes.
- Mac / Windows builds. The site shows placement; the footer still says
  Linux today.

## Shipped

### Chunk 1 — row grammar (2026-09-03)

What landed: `ListRow` (`theme.rs`) rebuilt on the grammar above — an
optional mono `.time()` column (`TIME_COL` 44), an always-reserved status
slot (`STATUS_COL` 12: `.dot()`, `.ring()` for unclaimed, `.bar()` painted
inside the slot rather than at the row edge), titles wrap to two lines by
default, chips moved to their own line under the title, and a `.meta(state,
text)` line (tinted Medium state word, then dim text) with `.hover()` for the
pipeline detail; `.subtitle()` is `.meta(None, ..)`. Feed rows lead with the
block start, claim rows wear the task dot, unclaimed rows a hollow ring; the
state word is `to confirm` / `live` (amber), `kept` (green), `unsorted` /
`new` / `moved out` (dim); the span and source/confidence moved to the meta
hover. `theme::humanize_title` turns a shell-prompt title
(`sam@workstation:~/dev/contoso`) into `contoso · Terminator`; other raw
titles keep one line. `ai_summary_line` → `summary_line`: Body, `TEXT_DIM`,
no italics, two lines then `…` with the whole text on hover (cards), full
wrap in the detail pane. Timeline cards use the same builder (range and top
evidence as the meta line, project/declared as chips, summary indented to
the title x); the detail pane's chips start at the title x too. Working on
(bar) and Recently closed (dot) share one title x.

Verified in the sandbox at 400×640 and 900×700: Home (Working on, Recently
closed expanded, feed with claimed and unclaimed rows), timeline lanes with
cards. Shots in `site/img/src/` (`home`, `home-wide`, `timeline-lanes`,
`timeline-wide`) for chunk 8. 168 tests, clippy clean.

### Chunks 2–4, 6, 7 — scale, buttons, legends, chat numbers, ref resolution (2026-09-03)

Chunk 2 (33580a4, a0a16f9): Small 12, Caption 11, `extra_text_line_spacing`
2 (`theme.rs` `style()`). The existing "ui scale" row in Settings › Window &
appearance became `text size` S / M / L = 0.92 / 1.0 / 1.1 on the same
`ui_zoom_factor` meta. The window follows the zoom: boot scales the default
card and the minimum by the saved zoom (`mod.rs` `BootPrefs.zoom`), and a
runtime change (S/M/L click or Ctrl +/−/0) sends `InnerSize` scaled by
new/old zoom plus a `MinInnerSize` floor; the meta write settles once the
zoom stops moving, and the shadow pad is not scaled.

Chunk 3 (33580a4, a0a16f9): `ListRow::show` returns `RowResponse` (derefs to
the row response, carries the state word's response); `.state_tip()` for a
hover-only word, `.meta_action()` for a clickable one (underline + pointing
hand on hover). Feed rows: `to confirm` click = `Action::KeepBlock`, the
`…` → keep path; `unsorted` / `new` click opens the assign popup with the
same candidates as `…` → assign; `live`, `kept`, `moved out` get one-line
tooltips. Only pre-pass placements route to keep.

Chunk 4 (c0d3cbe, a0a16f9): `reports.rs` `legend()` / `legend_row()` — swatch
· elided name (100) · thin bar · duration (`NUM_COL`) · percent (34), top 5
plus "other" summed from the true total. Same row function draws the
tasks-by-project headers, and task rows reserve the percent column so every
duration on the page shares one right edge. `WeekInsights.top_apps` cap 3 →
5.

Chunk 6 (459ae3f, 1d20c74): `core::chat::build_context` returns
`(prompt, ChatContextInfo { blocks, start_ms, end_ms, rows })`. Quantity
questions ("how long", "how much", "total", "per project", "how many
hours") get a `## Time per task` table from `report::task_totals` clamped to
the exact range (a phraseless quantity question defaults to this week);
`prompts/chat_v1.txt` says to quote those figures verbatim. Ranges: "this
week", "last week", weekday names, "yesterday afternoon" / "this morning",
"before / after the call" (first `Call` activity block that day; running
call ends now; ignored over multi-day ranges; empty result falls back to
the parsed range). The worker sends a `Context` message before the first
token; task-scoped chats send none. Tests: phrase → range fixtures,
call-edge cases, totals-in-context.

Chunk 7 (ea7548a, fd35628): `open_task_by_ref(conn, key, projects)` prefers
the open task whose project matches a repo/folder from the run (vcs repos ∪
cwd repos parsed from span titles, read before the branch rule), then the
most recently touched, then `id DESC` — never lowest id. `set_task_external_ref`
refuses a key another open task owns; derive's anchoring logs and skips.
There is no MCP declare tool; the UI declare path (`Action::Declare`) checks
`open_tasks_by_ref` first and shows "task N «label» already owns KEY" with an
"add to it" jump instead of creating a duplicate. Golden test: two open tasks
on one key, spans in one's folder → interval lands there. One-off for
interval 513 (task 100 → 85, re-describe 85) run against the live DB after
deploy.

### Chunks 5 and 8 — chat surface, site re-shoot (2026-09-03)

Chunk 5 (1998e3b, 4803810, plus the galley commit): `chat_worker` sends
`WorkerMsg::Context { blocks, start_ms, end_ms, rows }` before the first
token; the panel keeps it on the answer and draws "Read N blocks · Thu 3 Sep
08:00–14:56" (or "Searched N blocks") under it, click to expand the exact
rows. `Seeds` (today's top task or ticket ref, the last call's `HH:MM`,
today's task labels) feed three openers in the empty state and two
follow-ups after each answer; rebuilt on spawn and on `Done`, not on the 5 s
tick. Answers: `link_spans` marks today's task labels (ticket-ref shape or
≥ 8 chars and 2+ words, every occurrence) first, then `HH:MM` / `HH:MM–HH:MM`
in the gaps (skipped when the answer's range spans more than one day, or
when followed by `:`); a line with links is one `LayoutJob` with an
underlined accent format per link, hit-tested with `cursor_from_pos` on
click — no per-piece widgets. Time → timeline at that day; task → task
detail; either path drops the resident chat worker like the tab bar does.
Pipe tables render as an `egui::Grid`, last column mono right-aligned, cells
never wrap. Input: multiline, Enter sends unless an IME event is in flight,
Shift+Enter newline; `stop` reaps the worker and respawns it (no cancel path
in `ChatSession::answer`); `copy` on answers; history as a left panel at
`WIDE_W`.

Chunk 8: sandbox copy of the live DB (private spans deleted, saved window
position dropped so the widget parks in the corner) rendered by the release
binary at `WINIT_X11_SCALE_FACTOR=1.6`, zoom 1.0: `home-wide` (900×700),
`task-wide` (timeline lanes, first card clicked open), `reports-wide`,
`chat` (seeded conversation with a linked answer and a table), `feed`
(narrow, four wheel clicks). Sources in `site/img/src/`; alt text updated for
reports and chat.
