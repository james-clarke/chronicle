# M20 — UX pass 2: rhythm, alignment, richer views

Sep 2 walkthrough (James) of the m19 build. m19 gave us one chrome and type
scale; m20 fixes the layout defects that survived it and makes the three
"information" surfaces — working-on list, task detail, reports — actually
show the shape of the work instead of listing rows.

## Root causes (verified in code / sandbox)

1. **Hover pushes content down 2px** (suggest, redraft, generate summary,
   every ghost/disclosure row). egui 0.36 `Button` lays out through a `Frame`
   whose `total_margin()` includes `stroke.width`
   (`egui-0.36.1/src/containers/frame.rs:327-331`), and the frame it uses is
   the *state's* frame (`widgets/button.rs:364-370`). Our theme gives hovered
   and active a 1px `bg_stroke` but inactive none
   (`crates/app/src/ui/theme.rs:223,227`; `ghost_button` :449), so the button
   grows 2px on hover. Reproduced in sandbox: rows below "suggest" shift 2px.
   Fix: every widget state carries a 1px stroke (transparent when invisible).
2. **Nav buttons grab the window drag.** `mod.rs:1557` fires `StartDrag` on
   the first non-zero `drag_delta`. egui marks the bar as the drag target on
   press (buttons only sense click), so a 1px jitter during a click hands the
   pointer to the WM and the release never lands. Gate on
   `pointer.is_decidedly_dragging()` (egui's own 6pt/0.8s click-vs-drag
   decision) so jittery clicks stay clicks.
3. **Standup text jumps on toggle.** `home.rs:101-113` wraps the body in a
   `ScrollArea.max_height(220)`; while `fade_body` ramps, the scroll area's
   floating bar and the `add_space(4)` + `small_button` row change the card's
   height frame-to-frame. Header is `Small`/`TEXT_DIM` (`disclosure_header`,
   theme.rs:355) — why it reads faint.
4. **Working-on never reaches the edge / titles always cut.** `Grid`
   (`home.rs:203-206`) sizes columns to content; the title column gets
   whatever's left after project + status + "…" and `truncated_label` clips
   it. Grid can't right-align a trailing column.
5. **List rows misalign** (timeline cards `timeline.rs:490-555`, reports
   rows `reports.rs:56-85`, journal `timeline.rs:850-874`): times/durations
   are proportional-font labels with no fixed column, so "8:28–09:49" and
   "10:04–10:03" land at different x; journal date column width varies with
   the glyphs in it.
6. **Merge-into is a hover submenu** (`timeline.rs:1006-1019`
   `menu_button` inside `menu_button`): opens on hover, overlays the card,
   first click lands on a candidate.
7. **Detail action bar overflows** (`timeline.rs:920-965`): five buttons in
   one `horizontal`; at 320pt "re-fetch context" collides with the
   right-to-left "close task".
8. **Chat history lists empty days.** `chat.rs:226` creates a conversation
   row on view open; `list_conversations` (storage.rs:1740) doesn't filter
   on message count. No delete path exists (only retention prune).
9. **Reports empty week shows a phantom "· total" project row**
   (`reports.rs:92-108` renders the grid header/total even for zero
   projects). `most_fragmented_hour` is computed (`insights.rs:57-67`) and
   never shown.

## Design decisions

### Global (theme.rs)
- `PAGE_MARGIN = 12`: every view's root content sits inside
  `Margin::symmetric(12, 8)`; cards no longer touch the window edge, chat
  composer gets the same inset. Right detail panel: same.
- **Numeric column style**: durations, clock times and time ranges render in
  `TextStyle::Monospace` (12.5) at `TEXT_DIM`; helper `theme::num(text)`.
  Every list row uses a fixed-width right column (`NUM_COL = 64`) so numbers
  align across rows at every zoom (0.9 / 1.0 / 1.15 — the "density" modes,
  settings.rs:270, are zoom factors, so a points-based layout survives them).
- **`theme::list_row`** helper: full-width row (`allocate_ui_with_layout`
  over `available_width`), slots = leading dot / title (fills, truncates) /
  chips / numeric column / trailing control. Replaces Grid in Working-on,
  the reports task list, and the two-row timeline card body. One helper, one
  alignment rule.
- `theme::card_header(title)`: Heading/TEXT title with optional chevron —
  disclosure for *cards* (standup, recently closed) vs the existing
  Small/dim `disclosure_header` for incidental rows.
- Hover stroke fix (root cause 1) in `apply` + `ghost_button`.

### Home
- **Standup card**: `card_header` "Standup · Mon 1 Sep" (Heading, TEXT);
  body rendered structured: the draft is already one paragraph per task
  (prompt `prompts/standup_v1.txt`, fallback `main.rs:923`) — split on blank
  lines, first sentence up to the task label boundary becomes a Body/MEDIUM
  line, rest Body/TEXT wrapped, 8pt between blocks; "Next steps:" / "Next:"
  sentences pulled out as an indented line with a `▸`. No ScrollArea: the
  card grows; the page scrolls. Redraft becomes a ghost button in the
  card header row (right-aligned) so it never changes body height. Prompt
  tweak: ask for `Next: …` as the final sentence so the parser has a stable
  hook (optional, fallback already does this).
- **Working on** → `list_rows`: dot · title (fills) · project chip ·
  status chip · "…" pinned right. Declare row: input fills, project 110pt,
  primary "add". Suggest becomes a ghost button on the section header line.
- **Spans** → **"Unassigned"**: spans of the day not covered by any
  `intervals` row (join spans × intervals on time, `mod.rs:1189` loader
  gains the anti-join), clustered by app+title, sorted by duration, top 8,
  each row with duration + a "declare" ghost action that pre-fills the
  declare input. Raw spans list goes away (kept behind the "…" menu as
  "raw spans" if you still want it — default off).

### Timeline
- **List cards**: two rows become one `list_row` + a sub-row: title / duration
  (num col) / "…"; second line: chips + time range (num) + evidence
  (truncates). Chips get a fixed 4pt gap; evidence starts at the same x as
  the title.
- **Context switching**: add a **lanes** chart mode next to the band: one
  row per task (task colour, label at left, 12pt tall), blocks at session
  times, switch points drawn as 1px vertical ticks across lanes; header
  chip "N switches · fragmented hour HH". Toggle band ⇄ lanes in the day
  bar; remembered in meta. Third mode "hours" (stacked task colour per
  clock hour) if lanes lands cheaply — same segment data.
- **Merge into**: replace the hover submenu with a click-opened picker
  panel: "merge into…" item closes the menu and opens an inline chooser
  under the card (list of candidates + cancel); nothing merges until a
  candidate is clicked. Same picker reused by the detail "move session"
  menu later.
- **Detail pane**: rhythm 4 / 8 / 16 — title, 4, chips, 8, duration line,
  16, sections; `section_header` gets 16 above / 8 below.
- **Sessions** → session strips: each session is a row: time (num col) +
  a strip whose width ∝ duration (relative to the longest session), filled
  with per-app segments from spans (app → stable colour via hash into
  SERIES), commit/journal ticks as markers, confidence as strip opacity.
  Hover segment → app · title · duration. Needs one loader: spans per
  session window (`load_spans` already returns app/title/start/end).
- **Where the time went** stays.
- **Commits** → **Activity** section: same list, but the row type is
  generic (`kind` icon + time + summary). This milestone: commits only;
  the section is the seam for other collectors (roadmap below).
- **Journal**: fixed `NUM_COL` for "Mon 09:41", text starts at one x for
  every entry.
- **Context**: render the stored markdown (`chat.rs:54-109` builds `#`/`##`
  sections) as sections: `##` → Small/MEDIUM header, `-` → bullet rows,
  paragraphs wrapped; source chip (jira / slack) from `external_ref`
  prefix; "fetched 2h ago · re-fetch" as a ghost action *in the section
  header*, removed from the bottom bar.
- **Action bar**: chat / rename as ghost buttons left, "close task"
  right; merge + fetch move into the "…" menu and section header
  respectively. Never overflows at 320pt.

### Reports
- **Generate summary**: hover fix covers the push-down; button moves to
  the right of the "Focus" section header line.
- **Stat tiles**: 3-column grid of tiles (icon · Display value · caption),
  icons from a bundled Phosphor TTF subset (font + ~10 codepoint consts,
  no crate dependency): longest block, deep work, switches, fragmented
  hour (new, exists in `FocusMetrics`), busiest day (from `by_day` sums),
  active days, vs prior week. Top apps become a mini horizontal share bar
  under the tiles.
- **Tasks grouped by project**: project header row (name chip, total,
  share %, thin share bar) then its tasks as `list_rows` indented 12.
  Sorted by project total. The separate Projects grid goes away; a
  single-row "project mix" stacked bar sits above the list. Fixes 9.
- Reports task rows get a click → jump to timeline day / detail (cheap,
  data present).

### Chat
- Conversation rows created on first *send*, not on view open
  (`chat.rs:226,279,328`): keep `Option<conversation_id>` in state, create
  in the send path. One-off cleanup on startup: delete conversations with
  zero messages.
- Delete: "×" ghost on each history row → second click confirms ("delete?"
  → "×" red), calls new `storage::delete_conversation(id)` (messages +
  row). Deleting the open one starts a fresh scope.
- Composer inset = `PAGE_MARGIN`.

## Build order (each chunk reviewable + committed on its own)

1. **Theme**: hover-stroke fix, `PAGE_MARGIN`, `num`, `NUM_COL`, `list_row`,
   `card_header`, Phosphor subset font. Nav drag gate (mod.rs:1557).
2. **Home**: standup structure + header + redraft placement; working-on
   rows; unassigned activity loader + section.
3. **Timeline list + menu**: card rows on `list_row`; merge picker; page
   margin.
4. **Detail pane**: spacing rhythm; session strips (+ spans-per-session
   loader); journal column; context markdown; action bar split.
5. **Timeline lanes chart** (+ hours if cheap).
6. **Reports**: tiles + icons + new stats; project-grouped tasks + mix bar;
   empty-week fix.
7. **Chat**: lazy conversation, delete, inset.
8. **Visual acceptance** at zoom 0.9 / 1.0 / 1.15, before/after pairs.

Chunks 2–7 are independent once 1 lands; order above is by how much you
look at each surface.

## Roadmap (brainstorm, not in this milestone)

- **Activity collectors beyond git** — the "Commits" seam generalises to
  timestamped artefacts per task: file saves in the task's workspace
  (inotify on the repo path), browser URLs (spans already carry url meta
  since 003), terminal commands (shell history hook), PR review events
  (gh), calendar meetings (MCP), ticket transitions (Jira MCP), documents
  touched (LibreOffice/Google Docs titles). Each is a `vcs_events`-style
  table with `kind`, rendered by the same Activity row. That is what makes
  the app fit non-dev roles: a designer's Figma files, a PM's tickets and
  meetings, a writer's documents — same timeline, different collectors.
- Session strip → full-day "story" view (one strip per session in order,
  with journal/commits inline) as a third timeline mode.
- Unassigned activity → one-click "declare from cluster" that also creates
  the interval retroactively (needs derive cooperation).

## Out of scope

- Settings content (m18), notifications, tray, prompt rewrites beyond the
  one-line standup hook.
