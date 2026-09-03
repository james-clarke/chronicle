# M25 — Polish 3: cleaner, easier to read

James (Sep 2): "UI polish, clean up, easier viewing." m19 gave one chrome and
type scale, m20 fixed rhythm and made the information surfaces richer, m24
added the feed. This pass is about density and legibility of what is already
there. Candidates below come from the current site screenshots (all views,
2026-09-02); James picks and strikes.

## Candidates, roughly by leverage

1. **One duration rule.** Seconds show everywhere: `29m41s`, `6m00s`,
   `44m00s`, `1m00s`, `13m29s`. Rule: ≥1h → `2h41m`; ≥1m → `30m`; <1m →
   `41s`. One formatter, used by timeline cards, feed rows, task sessions,
   working-on, reports. Biggest noise reduction for the least code.
2. **Resizable / wide layout.** The window is fixed 400×640
   (`ui/mod.rs:40,120-125`). "Easier viewing" most likely means this. Allow
   resize (keep the widget size as default and minimum); at ≥720 pt switch
   timeline to two columns with the task detail pane beside the cards
   instead of replacing them, and let reports use the width for the week
   chart. Biggest item on the list; the layout code was written for one
   width, so budget a chunk per view.
3. **Standup card dominates Home.** On the shot it fills the whole first
   screen and the feed is below the fold. Default to the first task plus a
   "N more" toggle, or cap the body with a fade and "show all"; body text one
   step smaller than card titles.
4. **Truncation.** Working-on labels cut at ~30 chars while chips take the
   rest of the row ("connections features improve…"); card summaries cut
   mid-sentence; the standup "Next:" line ends at "for use by". Labels wrap
   to two lines before truncating; summaries get a full-text tooltip; the
   Next line is capped by sentence, not by pixel.
5. **Internal vocabulary in the feed.** "no batch yet", "derive left it",
   "provisional", "fresh", "unmatched", "model, 86%" are pipeline words.
   Human words on the row (e.g. "waiting for the model", "placed by a rule",
   "kept by you") and the pipeline detail in the row's `…` menu or tooltip.
6. **Activity glyphs without a legend.** ✳ / ◑ / ☐ before app names on cards
   and feed rows mean AI session / repo / PR to us, nothing to a new user.
   Either a legend in the timeline header row or drop the glyph from the
   card line and keep it in the detail pane where the label spells it out.
7. **Task detail sessions list.** Seven rows each with a `move` button and a
   mini bar. Fold sessions under 2 minutes into one "and 3 short sessions"
   row; `move` into the row's `…` menu; keep the bar. The "Where the time
   went" bars are good, leave them.
8. **Task colours.** Only three hues (blue, green, orange) so unrelated tasks
   share a colour on the band and in lanes. Colour by project with a
   distinct palette of 6–8, tasks within a project as shades. Pairs with the
   `projects` table in `teams-direction.md` m26 but can start from the
   string.
9. **Reports week chart.** Stacked bars have no legend or hover; the
   project colours only make sense once you scroll to the task list. Hover
   tooltip per segment and the project mix line moved up under the chart.
10. **Settings density.** One long scroll; the note "repo and source changes
    apply on the daemon's next start" is body size, should be caption; the
    Local sources toggles look like check icons, not switches; a right-side
    section index (Connections · Model · Capture · …) once the window can be
    taller.
11. **Timeline card meta line.** Monospace time range beside proportional
    app/activity text reads uneven; align the time range as a column or set
    it in the body face with tabular figures.
12. **Empty and loading states.** Not visible in the shots, worth a walk: a
    day with no data, reports with no prior week, chat before the model is
    downloaded, feed with nothing pending.

## Build order (if all of 1–11 are picked)

1. Formatter + truncation rules + vocabulary (1, 4, 5): pure text, no layout.
2. Home card and feed density (3, 6).
3. Colours and legends (8, 9).
4. Task detail and settings (7, 10, 11).
5. Resizable / wide layout (2): last, since it re-lays every view and
   benefits from the earlier cleanups being done at one width first.
6. Visual acceptance: screenshot walk of all views at 400×640 and at the
   wide breakpoint, before/after pairs, James signs off.

## Shipped (2026-09-02)

All of 1–11 plus the empty-state walk (12), one commit. Decisions taken
while building, for James to strike:

- **1 duration:** `fmt_dur` in `ui/mod.rs` is the one rule (`2h41m`,
  `30m`, `41s`, `0m` for nothing); digest/chat/CLI formatters untouched
  (prompt text is out of scope).
- **2 resize:** decoration-less window, so a corner grip
  (`ViewportCommand::BeginResize`) is the handle; 400×640 stays the minimum
  and default, last size in meta `ui_window_size`. `theme::WIDE_W` = 720 pt:
  timeline keeps its side pane (was 700), Home puts the feed in a resizable
  right column (`feed_section_ui`), settings gets a sticky section index,
  reports spread the chart and mix bar over the width. Chat unchanged
  (bubbles already cap at 85%).
- **3 standup:** first task block + "N more" / "show less" (session state);
  the Next line is cut to whole sentences at 180 chars, full text on hover.
- **4 truncation:** `ListRow::lines(2)` on Working-on rows, timeline card
  titles and feed rows — the row grows by a text line only when needed;
  hover shows the whole title when it still elides. Badges are painted by
  hand now (a Frame inside a horizontal layout stretched to the row height).
- **5 vocabulary:** rows say `placed by the model` / `placed by a rule ·
  repo chronicle` / `kept by you` / `placed by you` / `moved out of X` /
  `not placed by the model` / `waiting for the model`; chips `to confirm`,
  `new`, `unsorted`, `moved out`; source + confidence + rule on the sub
  line's hover.
- **6 glyphs:** the ✳ ◑ ☐ were Claude Code's own terminal-title status
  glyphs, not ours — `theme::display_title` strips them for display
  (stored titles and the digest keep them). The activity-row Phosphor
  glyphs now name their kind on hover.
- **7 sessions:** sessions under 2 min fold into one "and N short sessions"
  row (when two or more) after the real ones; `move` lives in each row's
  `…` menu.
- **8 colours:** eight hues; a project takes the hue at its first-seen
  position (`storage::project_order`, refreshed on every reload, so the
  first eight projects never collide), tasks within it are three shades by
  id; untagged tasks cycle hues by id. Palette order interleaves warm and
  cool so neighbours differ.
- **9 reports:** project mix bar + legend sit under the week chart; the
  day tooltip leads with the segment under the pointer.
- **10 settings:** the restart note is a caption; Local sources and the
  two window toggles are switches (`theme::toggle`).
- **11 meta line:** the time range leads the card's meta line in the
  session-row `RANGE_COL`, so digits align card to card.
- **12 empty states:** Reports hides the six zero tiles until the week has
  time; Home says where tasks come from when there are none; timeline,
  chat and connections empties were already fine.

## Out of scope

Light theme, notifications, tray, any change to derivation or prompts.
