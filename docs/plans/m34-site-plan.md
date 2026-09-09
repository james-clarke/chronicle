# M34 — the site, hyped and honest (draft)

Status: drafted 2026-09-08; chunks 0, 1 and 2 shipped the same evening
(see "Shipped" at the end). Chunks 3 and 4 are next; chunk 5 waits for
the M32/M33 copy. Builds on `docs/plans/site-plan.md`
(principles still hold: every claim gets a mechanism or a number, the page
itself is evidence).

## The short version

The page is already lean: one HTML file (11.8 K), one stylesheet
(10.9 K), no fonts fetched, no scripts, about 175 K of WebP above the
fold, the rest lazy. Keep that. What it lacks is life: nothing on the
page says the thing is being built *right now*, the download button
was cut because it was a dead end, and every section past the hero has
the same shape (heading, paragraph, screenshot). M34 adds a build pulse
that follows real pushes, a coming-soon CTA that reads as momentum, one
different-shaped section per screen, and motion that runs on CSS.

Budget, enforced by a check in the build script:

| what | limit |
|---|---|
| requests to third parties | 0 |
| JS | ≤ 2 KB inline, page works with it off |
| above-the-fold bytes | ≤ 250 KB |
| total page weight | ≤ 900 KB |
| Lighthouse (mobile) | 100 / 100 / 100 / 100 |
| motion | every animation off under `prefers-reduced-motion` |

## What is there today

`site/index.html`: top bar, hero (14 s CSS loop over two desktops), the
hover day band, two wide showcases (timeline, reports), a six-stage How
it works with terminal panels and three shots, footer. Deployed by Render
as a static site from `./site` (`render.yaml`), rebuilt only when
`site/**` or `render.yaml` changes (`buildFilter`). `site/img/src/`
(3.4 M of PNG/JPG sources, a 1.2 M `mac_background.jpg`) sits inside the
publish dir; not loaded by the page but shipped every deploy.

## Chunk 0 — the pulse and the button (no code dependency)

**Build pipeline.** `site/build.sh`, run by Render (`buildCommand: sh
site/build.sh`), rewrites `index.html` in place from the git checkout.
No client-side API call: the repo is private, a token in the page is out,
and a fetch would be the page's first third-party request. The committed
`index.html` keeps placeholder text so a local `open site/index.html`
still renders.

- `buildFilter` goes away so every push to `main` rebuilds (a static
  build is seconds; this is what makes the pulse honest).
- The script reads `git log` and writes, between `<!-- pulse -->`
  markers: last push relative time and absolute date, short hash, the
  subject of the last `feat|fix|perf` commit (truncated at 80 chars,
  scope kept, the milestone tag `mNN` pulled out as a badge), commits
  today / this week, and a 30-day commit-per-day strip as inline SVG
  (one `<rect>` per day, 30 rects, under 1 KB).
- Numbers strip, same script: `#[test]` count across `crates/`
  (`grep -rc`), crate count, migration number (highest
  `crates/core/migrations/NNN*`), Rust edition. No `cargo build` on
  Render; anything that needs a build is hard-coded and dated.
- Risk to verify on the first deploy: whether Render's clone is shallow
  (`git rev-parse --is-shallow-repository`, then `git fetch --unshallow`;
  `RENDER_GIT_COMMIT` is there either way). Fallback if git history is
  unavailable: a GitHub Action on push writes `site/pulse.json` as an
  artifact and hits the Render deploy hook; the script reads the JSON.
- `site/img/src/` moves to `docs/site-src/`. Render's build filter no
  longer matters, but the publish dir should hold only what ships.

**Hero pulse strip.** Under the "no" list, one monospace line plus the
30-day strip:

```
● pushed 2 h ago · m32 chunk 0 · capture ledger and lock signal
  4 commits today · 41 this week · ▁▂▅▃▇▂▁▁▃▆▄▂▁▅▇▆▂▃▁▄▅▂▁▁▃▇▅▂▄▆
```

The dot breathes (CSS, 3 s, opacity only). Hover the strip for the last
five subjects. This is the "sync and update" section: it tracks pushes
to the repo and shows the last update without anyone maintaining it.

**Coming-soon button.** A CTA row in the hero: primary `Download ·
coming soon` (solid, not greyed; a small amber dot; hover reveals
"Linux first, macOS and Windows after · built in the open above"),
secondary `Get in touch` (the mailto that is in the top bar today). The
button is not a dead end because the pulse under it says what is
happening. When a build exists the same button becomes the download
with the version from the script.

**Sharing.** `og:title`, `og:description`, `og:image` (a 1200×630 WebP
of the hero art, ~60 K), `twitter:card`. Hype spreads as links; today a
pasted link shows nothing.

Gate: push a code-only commit to `main`; the live page shows it within
one Render build, the strip's day count matches `git log --since`, the
page still makes zero third-party requests, Lighthouse stays 100.

## Chunk 1 — hero and band motion

- **Hero art** keeps the 14 s loop. Add a slow gradient mesh behind it
  (two radial gradients on `::before`/`::after`, 40 s drift, `opacity`
  and `transform` only) and a 2 % film grain via an inline SVG
  `feTurbulence` data URI at 8 % opacity. Cheap, and the flat panel
  stops looking like a slide.
- **Day band fills in as it scrolls into view.** Blocks grow from
  `scaleX(0)` left to right, staggered by their `left`, using
  `animation-timeline: view()` where supported and a 12-line
  `IntersectionObserver` that adds `.in` as the fallback. A thin "now"
  playhead ticks across the track over 60 s and stops at 17:40, so the
  band reads as a day being sorted while it happens (the caption already
  says so).
- **Legend isolates.** Hover a legend item and the other tasks' blocks
  dim: `.band:has(.legend .a:hover) .blk:not(.a) { opacity: .25 }`. No
  JS.
- Gate: reduced-motion shows the finished band at once; the band is
  identical with JS disabled.

## Chunk 2 — one section that shows the trick

Replace the first wide showcase ("The day, as it happened") with a
**before/after wipe**: left is the raw stream (a column of window titles
with timestamps, the same rows as the Sees panel, mono, dim), right is
the placed timeline shot. One `<input type=range>` drives a CSS custom
property that sets `clip-path: inset(0 calc(100% - var(--x)) 0 0)` on
the top layer. No JS beyond `oninput` writing the property (one line;
without it the range still moves nothing and the page shows the after
state). Autoplays one sweep on scroll-in, then waits for the hand.

This is the product in one gesture: titles in, tasks out. The reports
showcase stays a still, but its caption gets the real weekly numbers
from the pulse script's date (the "31 aug – 6 sep" line is hard-coded
today and will go stale).

## Chunk 3 — How it works, animated, plus proof

- **Terminal panels reveal line by line** as each stage scrolls in:
  every `<pre>` line wrapped in a `<span>` (build script does the
  wrapping so the source stays plain text), `animation-delay` by index,
  40 ms apart. The `stored 3 blocks · replaces 1 provisional row` line
  lands last with a green flash.
- **A flow line** down the left rail (the stages already have a rail):
  one SVG path, `stroke-dashoffset` driven by `animation-timeline:
  scroll()` so it draws as the reader moves. Fallback: static line.
- **"What leaves your machine"** — the section site-plan §4 specified
  and the page never got. A two-column table (with a key / without a
  key) and, beside it, the egress panel from Storage & server rendered
  as a terminal: `today · nothing else`. Under a key, the same panel
  shows `anthropic · 3 requests · standup, chat`. This is the trust
  section and it is the only table on the page, which is why it will
  stand out.
- **Presence and supervision** (M32 chunks 1 and 3) get a stage or a
  line in Places once shipped: "ten minutes reading is reading, not
  away", "two agents at once, split by who you were watching". Not
  before they ship.

## Chunk 4 — the build, in the open

A short section after How it works, fed by the pulse script:

- **Changelog strip.** The last ten `feat|fix|perf` subjects, grouped by
  milestone tag, each with its date. The milestone that is in progress
  is marked. Reads as a public changelog without a changelog to write.
- **Numbers.** `237 tests · 6 crates · migration 024 · 1 binary`, the
  binary size hard-coded with its date ("52 MB, 2026-09-04") until a
  release build exists.
- **Footer proof.** "This page: 0 third-party requests, 1.4 KB of
  script, no cookies, no analytics" with the byte count written by the
  build script, so the claim can never drift from the file.

## Chunk 5 — copy sync and the perf pass (after M32/M33 ship)

Copy on the page that the code has moved past or will:

| page | today | after |
|---|---|---|
| Sees, para 2 | "Five minutes away closes the batch." | quiet vs away (M32 chunk 1): "Five quiet minutes and it keeps reading with you; thirty away closes the batch." |
| Places | rules then model | add the segmenter as the default path, evidence scoring, the `not captured` line (M32 chunk 0) |
| band caption | "40m away" | add "12m not captured" so the honesty line is on the page |
| reports caption | hard-coded week | date from the build |
| Learns | rename, move, throw out | rename includes project; the Chronicle window itself is never counted (M33) |
| hero lede | unchanged | unchanged |

Perf pass: `<link rel=preload>` for the three hero images with
`fetchpriority=high` on the app shot, `Cache-Control: max-age=31536000,
immutable` for `img/*` in `render.yaml` headers (filenames are already
content-distinct per shoot; add a hash suffix in the build script if
they stop being), `decoding=async` on lazy shots, `content-visibility:
auto` on the two showcases and the stages. Re-run Lighthouse; keep the
four 100s.

## Order

0, then 1 and 2 together (both hero-adjacent, one visual review), then
3, 4, 5. Chunk 0 is the one to do first regardless of M32/M33 timing:
it turns every code push into a visible update.

## Out of scope

A framework, a font, a blog, analytics of any kind, a mailing list
service (the mailto stays), video.

## Shipped (2026-09-08, chunk 0)

- **`site/build.sh`**, Render's `buildCommand`, `buildFilter` gone so every
  push to `main` rebuilds. Three marker blocks in `index.html`: `pulse`
  (line, subject, counts and strip, the hover list of the last five
  commits), `numbers` (footer), `og` (absolute `og:url` / `og:image` from
  `RENDER_EXTERNAL_URL`, a placeholder host otherwise). POSIX sh + awk +
  GNU date, nothing installed. `sh site/build.sh DIR` bakes a copy for a
  local preview; the committed file keeps placeholders. Unshallows the
  clone when it is shallow (`--unshallow`, then `--deepen=500`), and keeps
  the placeholders when there is no history at all.
- **Pulse strip** under the CTA row: a breathing green dot (3 s opacity,
  off under reduced motion), `pushed 12 min ago · 14ae66c` with the
  absolute UTC time on hover, the last `feat|fix|perf` subject with its
  `type(scope)` dim and the `mNN` tag as a blue badge, cut at 80 chars,
  then `24 commits today · 170 this week` and the 30-day strip. The strip
  is one SVG `<path>` (four units per day, 13 tall at the busiest day,
  1 for an empty one), ~600 bytes. Hover or focus the box for the last
  five subjects; no JS anywhere, the page is at 0 B of script.
- **CTA row**: `Download · coming soon` solid blue with an amber dot,
  hover/focus reveals "Linux first, macOS and Windows after. Built in the
  open, right below."; `Get in touch` beside it (the top-bar mailto stays
  too). The download is a focusable span, not a link, until a build
  exists.
- **Sharing**: `og:type/title/description/url/image` with width, height
  and alt, `twitter:card=summary_large_image`. `img/og.webp` is
  1200×630, 36 K: the mac desktop darkened to the page ground, the wide
  home shot at right, the h1 and lede at left (ImageMagick + DejaVu; the
  recipe is in this session's log, not scripted — reshoot by hand if the
  hero changes).
- **Budget check** at the end of the build, exit 1 on a breach. Today:
  third-party 0, inline JS 0 B, above the fold 184 KB, page 405 KB.
- **Numbers** live in the footer until chunk 4 gives them a section:
  `271 tests · 6 crates · migration 027 · rust 2024 · 1 binary`.
- Also fixed on the way: `no cloud model by default` overflowed its chip
  between 420 and 860 px wide; the chips wrap under 860 now.

Deviations from the plan above:

- `site/img/src/` was never shipped: it is gitignored, so Render's clone
  never has it. Not moved.
- "pushed N ago" is the last commit's author time, not the push; a push
  lands within minutes of its commit here and the build runs at push, so
  the line is honest to the minute it claims.
- Lighthouse not run: no `lighthouse` on the box and the page has no
  script or third-party request, so the budget check stands in for it
  until the first live build.

Gate still open: needs a push (James). On the first Render build check the
log for the `pulse:` line (a shallow clone that cannot unshallow prints
`today`/`week` counts that are too low; then the GitHub Action fallback in
chunk 0 above), that the page shows the head commit, and that the strip's
day count matches `git log --since`. If `RENDER_EXTERNAL_URL` is unset
for static sites the `og` block keeps the `onrender.com` placeholder;
set the host by hand in the script then.

## Shipped (2026-09-08, chunks 1 and 2)

- **Hero mesh and grain.** `.hero::before` is now two radial gradients
  (blue at 40/40, green at 70/70) drifting 40 s, `transform` and
  `opacity` only, `alternate`; `.hero::after` is a 160 px `feTurbulence`
  data URI at 8 % opacity behind a radial mask so its edges never show
  (`isolation: isolate` on the hero, both pseudos at `z-index: -1`).
- **Band fills in.** `.band.in .blk` grows from `scaleX(0)` at its left
  edge, 0.5 s, staggered by `--l` (the block's `left` as a number, on the
  inline style) × 6 ms; the playhead is `.track::after`, resting at
  96.7 % (17:40 on a 08:00–18:00 track) and sweeping from 0 over 60 s
  once `.in` lands. Legend hover dims every other task's blocks through
  `:has()`. No `animation-timeline: view()`: the track is 28 px tall, so
  a view-progress range would be one scroll notch; `.in` from a
  seven-line `IntersectionObserver` (threshold 0.3) does both the band
  and the wipe. Without the script nothing gets `.in` and the page shows
  the finished band and the placed timeline — checked with the script
  stripped.
- **Before/after wipe** replaces the first wide showcase. `.wipe-box`
  keeps the shot's 1478 / 1080 ratio; under it a mono column of window
  titles written in the vocabulary the shot actually shows (Terminator,
  Google-chrome, ACME-11381/11382, PR #9480/#9481, the two away gaps);
  over it the timeline shot with `clip-path: inset(0 0 0 var(--x))` and
  a 2 px divider at `left: var(--x)`. One `<input type=range>` under the
  box writes `--x` in a one-line `oninput`. On scroll-in one 2.4 s sweep
  from 100 % to 0 % (raw in, placed out), no fill mode, so the base
  `--x: 0%` then holds until the hand moves it. Caption "what it saw ·
  what it wrote down". Mobile keeps the 760 px horizontal scroll the
  wide shots had.
- **Reduced motion** block moved to the end of the stylesheet — it sat
  before the band rules and lost on order; now every animation on the
  page (hero loop, mesh, dot, band, playhead, sweep) is off under it,
  checked by forcing the media query in a preview copy.
- Budget after: inline JS 340 B, above the fold 189 KB, page 410 KB,
  third-party 0.

Deviations:

- The reports caption keeps "31 aug – 6 sep": the shot is of that week,
  so a build-time date would say the wrong thing under the right
  numbers. Chunk 4's changelog is where the build date belongs.
- Chunk 2 said the wipe autoplays "then waits for the hand"; it does,
  but the handle starts at the left (divider at 0 = placed timeline
  shown) and dragging right reveals the raw column, since the divider's
  position is the value.
