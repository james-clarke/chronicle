# M43 — The site and the docs, caught up with what shipped

Status: written 2026-09-10 on James's "revamp the web page with new
features and updates, and line that task up also for docs". Depends on M42,
which owns the synthetic corpus every screenshot here is rendered from.
Every "today" claim carries a `file:line`.

## The short version

The site was written at M34 and describes a product from before M35. Six
milestones have shipped since and none of them appear: projects, the
accuracy work, the source expansion, macOS, Wayland, and the download
section added today. Every screenshot also shows real captured work, so
all sixteen have to be regenerated regardless. That makes this the right
moment to redo the page rather than patch it.

The docs have the opposite problem. `docs/plans/` is unusually good and is
about to become the public exhibit for how this project is built, but there
is no entry point: nothing tells a reader what the plans are, which are
current, or where to start. The README is a stack reference, not an
introduction.

## What the site says today

Sections, in order (`site/index.html`):

| Line | Section | State |
|---|---|---|
| 33 | hero | fine; the CTA now anchors to the download section |
| 65 | band | fine |
| 95 | "The day, as it happened." | real captured work, must be replaced |
| 135 | "The week, added up." | screenshot predates projects (m35) |
| 145 | "How it works" (Sees, Places, Learns, Writes, Shows its work, Your own key) | the six-step spine is still right; the content under it predates m36 and m37 |
| 251 | "Set a task. Go and do it." | fine |
| 289 | "Get it." | added today |
| 311 | "The build, in the open." | numbers are baked at build time and stay correct |

Eight images are referenced and sixteen exist. `site/img/og.webp` is the
one that renders in link previews and is the highest-exposure of them all.

What shipped and is nowhere on the page:

- **Projects (m35).** Home is organised by project, reports and the
  standup are project-major, and tasks carry a project. The current
  "week, added up" screenshot predates all of it.
- **Accuracy (m36).** Daily self-scoring, the deterministic scorer with a
  frontier advisor over it, corrections as few-shot examples, and the
  bench. This is the direction document's headline claim and the page never
  makes it.
- **Sources (m37).** The shell hook, git hooks, repository discovery,
  browser history, link files, eight AI session formats, and remote MCP
  servers. The page's source story is much smaller than the product's.
- **macOS (m38) and Wayland (m39).** The page implies Linux X11. The
  platform matrix in the README is now three platforms wide.
- **Open source and how it is built (m42).** Not on the page at all.

## Decisions

- **Redo, do not restyle.** The visual language works and the byte budgets
  are healthy (204 KB above the fold against a 250 KB limit at the last
  build). The work is content, structure and new screenshots, not a new
  design. A restyle would burn the budget headroom for nothing.
- **Every screenshot is regenerated from the synthetic corpus**, never
  retouched. Blurring or cropping real data leaves the shapes of it behind
  and has to be re-judged image by image. Rendering a designed day is one
  setup and is reproducible when the UI changes again.
- **The corpus is the demo.** M42's invented scenario is what the app runs
  for the screenshots, so the fixtures, the site and any future demo all
  tell one story. A second, site-only fake dataset would drift.
- **Say it is agentically built, as evidence.** A short section, below what
  the product does rather than in the hero. Someone downloading a time
  tracker wants the time tracker first. The strong version is specific and
  checkable: the milestone plans, their gates, what changed while building
  and what is still owed. `docs/plans/` is the exhibit, so it needs a way
  in, which is the docs half of this milestone.
- **Dovetail with m41, do not duplicate it.** M41 generates
  `site/tools.html` from the connector registry. That page owns "what
  Chronicle can read"; this milestone links to it and does not restate the
  list, or the two will disagree the first time a connector lands.
- **Docs get an index, not a rewrite.** The plans are good as they are.
  What is missing is `docs/README.md`: what a plan is, the milestone list
  with one line each and its state, and where to start reading.

## Chunks and gates

### 0. The rendering setup

- A repeatable way to run Chronicle against the M42 corpus and capture the
  eight referenced views at the current window size.
- Gate: two runs of the same view produce the same image but for the clock.

### 1. Screenshots

- Regenerate all sixteen, `og.webp` first since it has the widest reach.
- Gate: no identifier from the M42 table survives in `site/img/`, checked
  by eye against the rendered images rather than by grep, which cannot read
  a `.webp`.

### 2. The day, the week, and how it works

- Replace `site/index.html:100-125` with the designed day.
- Refresh "The week, added up." for projects, and the six "How it works"
  steps for the accuracy and sources work.
- Gate: every claim on the page maps to something that shipped, named by
  its milestone.

### 3. New sections

- Sources and connections, linking to `tools.html` rather than listing.
- Platforms, matching the README matrix: Linux X11 and Wayland, macOS,
  Windows when M40 lands.
- Open source and how it is built.
- Gate: the four byte budgets still pass.

### 4. Docs

- `docs/README.md`: what a plan is, the milestone list with state, where to
  start.
- README: the introduction a public reader needs before the stack table,
  the AGPL section, and the agentic authorship section.
- Gate: a reader who has never seen the repository can get from the README
  to a plan and understand what it is for.

## Owed

- M40 Windows is not shipped, so the platforms section ships with Windows
  marked as not yet, and gets a second edit later.
- M41's `tools.html` may land after this; the link is written either way
  and the section reads correctly with or without it.
- The tray mockups the M34 plan deferred are now unblocked, since M38
  shipped a real tray to screenshot.

## Shipped (2026-09-11)

All five chunks, in one pass, with these deviations from the plan above.

- **Chunk 0.** The rendering setup is `scripts/site-shots.sh` over a hidden
  `chronicle replay --case <fixture>=<day> --reconcile` (`crates/app/src/replay.rs`),
  which writes a fixture's events through `storage::insert_event` and
  `sessionizer::refresh` exactly as the daemon does, then places every closed
  batch through the segmenter tier the shipped config uses and drains the
  `name_task` jobs through the local model. Nothing loaded a fixture into a
  database before this. Beside it, a public `chronicle task add` (Home's
  declare, step for step) and three env hooks for the UI child:
  `CHRONICLE_UI_DAY`, `CHRONICLE_UI_TASK`, `CHRONICLE_UI_SETTINGS`, so no
  screenshot needs a click. Gate met: two runs differ only by the clock and
  by what the local model named.
- **The corpus is designed, not real.** `fixtures/site/*.jsonl` are five
  generated days (`scripts/site_week.py`, seeded) for Sam, the developer the
  cast document `fixtures/README.md` introduces; the M42 plan's chunk 0 owed
  that document and it lives here. `day3_sms`/`day4_heroku` were not used:
  they are real days with the identifiers swapped, and they still carry the
  author's searches and the employer's product name. The `sam@` shell
  prompt was renamed to `sam@` tree-wide in the same pass.
- **Chunk 1.** Seven images replaced (`og`, `home-wide`, `timeline-wide`,
  `reports-wide`, `triage`, `model`) and eight deleted rather than
  regenerated, since the page never referenced them. `pipeline.webp` is
  gone: "Shows its work" now quotes the sandbox's own `chronicle status`
  self-score block, which says more than the pipeline card did. No
  connections screenshot: the registry probes this machine, so the panel
  would have shown the author's repositories however the data dir was
  sandboxed. Checked by eye against every rendered image.
- **Chunks 2 and 3.** The band, the day and every "How it works" panel are
  Sam's Thursday, read from the sandbox database, not typed. New sections:
  "What it can read" (three cards and a pointer to the README, not to
  `tools.html`, which M41 has not generated) and "Open source, built in the
  open", which absorbed the build-numbers block. Platforms stayed in the
  download section, with the macOS line now saying it has never run on a
  Mac. Budgets: 218 KB above the fold, 398 KB page weight.
- **Chunk 4.** `docs/README.md` and the README introduction.

Owed: the naming job files a minted task under a project it invents
(`platform-eng`, `django`); the seed script unfiles those, but the prompt
should be constrained to the configured projects. `chronicle replay` does
not reproduce `activity_events`, presence rows or the consolidation tier.
