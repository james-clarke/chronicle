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
