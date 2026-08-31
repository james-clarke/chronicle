# m13 — UI redesign plan (from 2026-08-31 review)

Mockups (built from real Aug 31 data): https://claude.ai/code/artifact/c4942f69-69ae-4d5e-b003-984379f3ce7d
Artboards: widget "Today" view (360×560), expanded window (760×600), week report (680×500), plus a low-fi tray-popover alternate. Direction not yet picked by James — default to the widget-first direction unless he says otherwise.

## Root-cause findings

1. **Interval rows carry no content.** `tasks_in_range` selects only task label/project + interval times/confidence (`crates/core/src/storage.rs:759-769`). Spans load separately and are never joined to intervals (`crates/app/src/ui/mod.rs:423-446`), so interval rows render only `HH:MM:SS–HH:MM:SS dur conf% [move]` (`crates/app/src/ui/timeline.rs:375-416`).
2. **No real column alignment.** Rows are single `ui.horizontal` calls; duration aligned by string padding `format!("{dur:>7}")` (`timeline.rs:393`) mixing monospace 12.5px with body 14px.
3. **Interval rows are noise.** Today: 23 intervals for 3 tasks, each row with its own `move` button and confidence %. Adjacent intervals should merge into display "sessions".
4. **Not widget-shaped.** 720×800 default, 560×600 min (`crates/app/src/ui/mod.rs:32-38`); persistent `edit / merge into / close / move` text buttons everywhere.
5. Minor: seconds in all timestamps; fixed `row_height` for virtualized rows clips any wrapped row (`timeline.rs:113`); timeline labels lack `.truncate()` (reports has it, `crates/app/src/ui/reports.rs:67`); reports week is a dot grid; settings is an unconstrained floating `egui::Window` (`crates/app/src/ui/settings.rs:121-125`).

## Design decisions (mockups embody these)

- **Widget "Today" as default face**: total-focus header, horizontal activity band (segments colored by task, gaps = away), task cards: color dot + label + project pill + duration + one evidence line (top apps/titles). Actions behind hover `⋯` menu.
- **Expanded window on demand**: master-detail. Left: task list. Right: merged session chips, "where the time went" evidence bars (single-hue bars in the task's color), rename/merge/close at bottom.
- **Reports**: stacked per-day bars in task colors + task table; drop the dot grid.
- **Task color = identity everywhere** (dot → band → report bars). Categorical palette validated for CVD on `#131418`: `#5e87ea` / `#27a97f` / `#bd8827`. Existing theme accents (amber/orange/green in `crates/app/src/ui/theme.rs:10-21`) fail adjacency checks as a categorical set — keep them for status, not series identity.
- **Confidence**: surface only when low (tinted dot on card); drop per-interval %.
- Timestamps `HH:MM`, no seconds. Tabular numerals for all durations/times.

## Execution order

1. **Data layer**: extend `load_groups` (`crates/app/src/ui/mod.rs:306-338`) with
   a) evidence aggregation — overlap-join focus spans to each task's intervals, group by app (and top titles), order by overlap time;
   b) merge adjacent/near-adjacent intervals into sessions for display.
   Verified query shape (ms timestamps): `SELECT s.app, s.title, SUM(MIN(s.end_ts,i.end_ts)-MAX(s.start_ts,i.start_ts)) FROM spans s JOIN intervals i ON i.task_id=?1 AND s.end_ts>i.start_ts AND s.start_ts<i.end_ts WHERE s.kind='focus' GROUP BY ...`.
2. **Timeline rebuild** (`timeline.rs`): task cards + activity band, hover-reveal actions, drop per-interval rows and `Spans` debug section from the default view (keep behind a toggle if useful for bench debugging).
3. **Expanded detail pane**: sessions + evidence bars; becomes the correction surface for the bench loop (rename/merge/move feed corrections table as today).
4. **Reports** bars; then window sizing (compact default ~360×560, remember size) last.

## Constraints / notes

- Daemon is systemd-managed from `~/.cargo/bin` — see `workflow.md`: `cargo build`/`test` safe anytime; stop unit before `cargo install`.
- Visual loop: `./target/debug/chronicle ui` standalone against live DB. Screenshots of current UI from the review: session scratchpad `shots/tab-*.png` (temporary).
- Timeline in-frame rollup still sums intervals unclamped in `load_groups` — known latent double-count for midnight-spanning intervals; fix opportunistically during step 1.
- m13 acceptance (README): fresh install to first derived task without touching a terminal — onboarding cards already exist; visual pass should keep them.
