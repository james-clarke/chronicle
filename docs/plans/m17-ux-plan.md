# M17 — UX fixes: standup reliability, window behavior, background noise

Sep 1 review (James): standup card fails silently, `chronicle ui` window landed
half off-screen right while `chronicle` landed top-left, the widget hides when
it loses focus, and short scattered sessions (Wordle, music-mix hunting)
clutter the timeline. Customer lens: open it easily, keep it open, trust what
it shows.

## 1. Standup card — silent failure + no-journal fallback

Root cause found live: `ai_jobs` rows 16/17 failed with
`no journal entries on 2026-08-31 to draft a standup from`
(`crates/app/src/main.rs:860-862` bails when `standup_digest` returns no
rows). The UI poll (`crates/app/src/ui/mod.rs:606-616`) clears the job on
`failed` without reading the error, so the spinner vanishes and the lone
button returns — looks like "loads then does nothing". Journals only exist
since M16, so every fresh install hits this on day one.

- **Worker fallback** (`main.rs` standup job): when there are no journal
  rows, build a digest from the day's activity instead — per task: label,
  project, external ref, total focused time, top evidence apps — marked as
  activity-derived so the prompt stays grounded. Bail only when the day has
  no task activity at all. Skip background-classified tasks (see §4) when at
  least one foreground task exists.
- **Error surfacing** (UI): on a `failed` standup job, keep the job's
  `result` text and render it on the Home card ("couldn't draft: …") instead
  of silently resetting. Clear it on the next draft click.

## 2. Stay open until closed

`autohide` currently defaults on (`crates/app/src/ui/mod.rs:438`, opt-out via
`CHRONICLE_UI_NO_AUTOHIDE`); the window hides on focus loss
(`mod.rs:1152-1159`). Widget-era default, wrong for a product someone manages
work in.

- Default becomes stay-open. Focus loss does nothing.
- `CHRONICLE_UI_AUTOHIDE=1` restores the popover behavior (env only; a
  Settings toggle can come with M18).
- Close (X) and the tray toggle still hide — that's an explicit user action.

## 3. Window placement — persist, clamp, drag

Placement runs once from `monitor_size` (`mod.rs:1129-1142`). Two failure
modes seen: `monitor_size` never arrives → WM default top-left (plain
`chronicle`); `monitor_size` reports the multi-monitor virtual desktop or a
scale-mismatched size → bottom-right math lands half off-screen
(`chronicle ui`). Underlying problem: the window is decoration-less and
unmovable, so any bad placement is unrecoverable by the user.

- **Drag to move**: empty top-bar area drags the window
  (`ViewportCommand::StartDrag`). This is the real fix — placement bugs stop
  being traps.
- **Persist position**: after a move settles, save the outer position to
  `meta("ui_window_pos")`; restore it at boot via `with_position`. Saved
  position wins over the bottom-right default.
- **Clamp**: when restoring or defaulting, clamp so at least the top bar is
  on-screen once `monitor_size` is known.

## 4. Background sessions — collapse the noise

Short, scattered, undeclared sessions (a 2-minute Wordle, a music search)
each become their own task card and crowd real work. They're real time — keep
them recorded and visible in the activity band and reports — but out of the
card list's way.

- Config: `background_minutes` (default 10, `0` = off) in
  `crates/core/src/config.rs`.
- A `TaskGroup` classifies as background when all hold: not declared, no
  journal entries, no checkpoint, no external ref, day total under the
  threshold.
- Timeline: background groups leave the main card list and render as one
  collapsed strip after the cards — "background · N tasks · 32m" —
  expandable to the same task cards (all actions still work: reassign,
  merge, declare; declaring promotes it out of background next reload).
- Activity band and Reports unchanged: totals stay honest.
- Standup fallback digest (§1) skips background tasks.

## Build order

1. §2 stay-open (small, isolated).
2. §1 worker fallback + UI error surfacing.
3. §3 drag + persist + clamp; verify visually (screenshot loop).
4. §4 config + classification + collapsed strip.
5. `cargo test`, clippy, visual pass, deploy (stop unit → `cargo install
   --locked` → restart), progress.md entry.

## UX backlog (not this pass — candidates for M18+)

- **Settings as setup home** — own plan: `m18-settings-plan.md`.
- **Systemic silent failures**: every `ai_jobs` consumer swallows `failed`
  (narrative `mod.rs:595-604`, standup, description). One shared "job failed
  → surface reason" pattern.
- **First-run experience**: onboarding exists (~525u) but day-one is empty —
  seed with "what Chronicle will do" states rather than blank panes.
- **Standup handoff**: copy button / clipboard export on the standup card;
  later Slack post via MCP.
- **Week at a glance**: Reports is per-range; a lightweight week strip on
  Home ("Mon 6.2h · Tue 5.1h …") is cheap and high-signal.
- **Keyboard**: Esc closes detail pane, arrows switch days, `/` focuses
  filter.
- **Notifications**: checkpoint nudge after long AFK return; standup ready
  in the morning (needs a notify path — tray already exists).
