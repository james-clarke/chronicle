# M19 — Polish: from engineery to product

Sep 1 feedback (James): work cards are starting to look really good, but the
app as a whole doesn't feel polished — scroll bars, font sizing, wrapper UI,
animations. This milestone is a design-system pass, not a redesign: one
consistent chrome, type scale, and motion language over the existing layout.

## Audit findings (screenshots 2026-09-01, sandbox over live data)

1. **Window chrome — none.** Decoration-less flat rectangle: square corners,
   no border, no shadow; it reads as a dev surface floating over the desktop.
   The top bar now drags and has a close button (98e2cf1), so it already
   *acts* like a title bar — it just doesn't look like one.
2. **Scroll bars — egui defaults.** Thin, dark, near-invisible thumbs; no
   hover feedback. Nothing in `theme.rs` touches `ScrollStyle`.
3. **Type scale — improvised.** Sizes come from egui's stock
   Heading/Body/Small plus ad-hoc `RichText` tweaks per call site; hierarchy
   is inconsistent between views (Reports' big stat numbers vs Timeline's
   `4h04m` vs Home card titles).
4. **Card chrome — three dialects.** SURFACE frame with radius 8 + margin 12
   (standup/resume cards), accent-tinted onboarding cards, and bare rows with
   zebra fills ("Working on", Reports task list). Buttons are stock egui
   rectangles; primary actions (add, send, download model) look identical to
   incidental ones (…, redraft).
5. **Motion — two hover fades exist** (`reports.rs:261`, `timeline.rs:318`,
   0.08s `animate_bool_with_time`); everything else snaps: standup collapse,
   background strip, detail pane open, view switches.
6. **Texture details.** Mid-word truncations ("Comparin…", top-apps line);
   plain-text disclosure rows ("▶ background · 1 task · 5m00s",
   "▶ recently closed"); missing-glyph ✕ boxes (fixed alongside this plan:
   chat scope chip + resume dismiss now ×).

## Design tokens (all in `crates/app/src/ui/theme.rs`)

- **Type scale**: Display 22 (stat numbers), Heading 15, Body 13, Small 11.5,
  Caption 10.5 dim — mapped onto egui `TextStyle`s once in `theme::apply`;
  call sites stop hand-rolling sizes.
- **Spacing**: 4 / 8 / 12 / 16 rhythm; card inner margin 12, card gap 8,
  section gap 16 — audit each view against it.
- **Radii**: window 12, card 8, chip/button 6.
- **Scroll**: `ScrollStyle::solid`-based — 6px rounded thumb,
  `TEXT_DIM`-alpha fill, brightens on hover; bar floats over content edge.
- **Buttons**: primary (ACCENT fill, dark text), secondary (SURFACE fill,
  1px stroke), ghost (no fill until hover). Helper fns in theme.rs so views
  can't improvise.

## Build order

1. **Theme foundation** — type scale, spacing constants, scroll style,
   button helpers, widget hover/active visuals in `theme::apply`. Pure
   theme.rs + mechanical call-site sweeps; no layout changes.
2. **Window wrapper** — transparent window (`with_transparent`), root frame
   painted as rounded rect (radius 12) with 1px border stroke and soft
   shadow; top bar keeps drag/close and gains the rounded top. X11/Openbox
   compositing check first: without a compositor transparency degrades to
   square corners — detect and fall back cleanly.
3. **Card + control sweep** — one card style everywhere; primary/secondary
   button assignment per view; disclosure rows become real toggles with
   chevron + count chip; truncation gets `.truncate()` with hover tooltip
   for the full text.
4. **Motion pass** — `animate_bool_with_time` (0.12s) on: standup collapse,
   background strip, recently-closed, detail-pane sections; hover lift
   (slight bg brighten) on all cards, matching the existing timeline/reports
   fades. No layout animation beyond these — egui repaints stay cheap.
5. **Visual acceptance** — screenshot walk of all five surfaces + settings,
   before/after pairs, James signs off.

Steps 1–3 are the bulk; 4 is small once 1 lands. Settings surface gets the
same sweep but its *content* overhaul stays m18.

## Out of scope

- Settings-as-setup-home content (m18), first-run empty states, week strip,
  keyboard shortcuts, notifications (m17 backlog).
- Any timeline/report layout rework — the card structure James likes stays.
