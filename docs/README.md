# docs/plans

This directory is the design record for Chronicle. It is also, from here on,
the public exhibit for how the project is built — see the README's "How this
was built" for the framing this page keeps consistent with.

## What a plan is

Every milestone starts as a plan document, written before the code, not
after it. Each one opens with a Status line (what has actually shipped, as
of the date it was last touched) and a short version — a few paragraphs
naming the problem and the shape of the fix. Below that are the decisions
taken, and numbered chunks, each with a gate: a measurable condition the
chunk has to pass, not a description of intent. Most plans end with an
"Owed" section that names, in plain terms, what was knowingly left undone.

The plans are written by an AI agent working in this repository under
James's direction and review — the same authorship the rest of the project
has. Every claim a plan makes about what the code does "today" carries a
`file:line`, so the claim can be checked against the tree rather than taken
on trust.

They are not tidied up afterwards. A plan's Status line is what its author
believed when the file was last edited, and building often outran that:
several plans here have a Status line that says a chunk is still open while
a "Shipped" section further down the same file shows it landed later that
day. That is left as it happened rather than smoothed over — what changed
while building is part of the record, not noise to clean out of it.

## Where to start reading

- **[m13-ui-plan.md](plans/m13-ui-plan.md)** — the earliest plan in the
  directory and the shortest. It shows the anatomy in miniature: root-cause
  findings with a `file:line` each, the design decisions they justify, and
  an execution order, all in about forty lines.
- **[m35-project-first-plan.md](plans/m35-project-first-plan.md)** — a
  decision reversed in the open. Chronicle had attributed time to tasks
  first and projects second since M9; this plan traces a single
  mis-projected task hijacking a day's work back to that ordering, by
  `file:line`, and flips it.
- **[m36-accuracy-plan.md](plans/m36-accuracy-plan.md)** — the accuracy
  work, and the plan most honest about a negative result: three separate
  attempts to make corrections-as-few-shot-examples clear its own gate are
  logged in order, with the scores each one produced, and the gate stays
  unmet.
- **[m42-open-source-plan.md](plans/m42-open-source-plan.md)** — the
  going-public one. The shortest plan here after m13, and the one that
  explains why the repository looks the way it does to a stranger opening
  it for the first time.

## Milestones before m13

M0 through M12 have no plan document — they predate the practice this
directory records. What they were and what each shipped is in the
README's ["Milestones"](../README.md#milestones) section, in the first
table.

## The milestones

| Plan | What it is | State |
|---|---|---|
| [m13-ui-plan.md](plans/m13-ui-plan.md) | Widget-first UI redesign: task cards, evidence bars, one report view | shipped |
| [m15-task-workspace.md](plans/m15-task-workspace.md) | Git evidence anchors (m15) and the task workspace: MCP context, journal, resume card (m16) | shipped |
| [post-m16-direction.md](plans/post-m16-direction.md) | Handoff after m16: where the build stood and the candidate next steps | direction paper (not a milestone) |
| [m17-ux-plan.md](plans/m17-ux-plan.md) | UX fixes: standup reliability, window placement, background noise | shipped |
| [m18-settings-plan.md](plans/m18-settings-plan.md) | Settings redesigned as the setup home, with status and a test per integration | shipped, split into m21 (steps 1–3) and later milestones (steps 4–6) |
| [m19-polish-plan.md](plans/m19-polish-plan.md) | Design-system polish pass: window chrome, scrollbars, type scale, card chrome | shipped |
| [m20-ux-plan.md](plans/m20-ux-plan.md) | UX pass 2: layout rhythm and richer information surfaces | shipped |
| [m21-settings-plan.md](plans/m21-settings-plan.md) | Settings gains a Connections section: MCP servers and git repos, with status | shipped |
| [m22-collectors-plan.md](plans/m22-collectors-plan.md) | `activity_events` generalised; Claude Code, GitHub PR and mic-in-use collectors | shipped |
| [m24-live-feed-plan.md](plans/m24-live-feed-plan.md) | The Unassigned feed becomes the front door: pre-pass, proposals, eject | shipped |
| [m25-polish-plan.md](plans/m25-polish-plan.md) | Polish 3: one duration rule, a resizable window, easier-to-read feed rows | shipped |
| [m26-daily-driver-plan.md](plans/m26-daily-driver-plan.md) | Daily driver: Home task list, calendar, editor/shell heartbeats, intent, Jira write-back | shipped, chunk 7 owed (click-tests and a feed soak, James's to run) |
| [m27-derivation-plan.md](plans/m27-derivation-plan.md) | Derivation speed and accuracy: interval coalesce, resident worker, live tier, an inspector | shipped; a replay regression and a soak day are still open |
| [m28-cleanup-plan.md](plans/m28-cleanup-plan.md) | Repo cleanup: docs synced, loose ends from m21–m27 closed, a five-lens codebase review | shipped |
| [m29-legibility-plan.md](plans/m29-legibility-plan.md) | Legibility pass: list rows, chips, timeline detail, a chat worth demoing | shipped |
| [teams-direction.md](plans/teams-direction.md) | Research: a teams/alignment product built on the derived layer | direction paper (not a milestone) |
| [m30-derivation-v2-plan.md](plans/m30-derivation-v2-plan.md) | Derivation v2: anchored spans, evidence profiles, scorer-first segmentation, embeddings | shipped, chunk 7 owed (a soak week before the pre-pass/live-tier default is decided) |
| [m31-cloud-models-plan.md](plans/m31-cloud-models-plan.md) | Cloud models: a backend trait, an Anthropic backend, `models.toml` routing | chunks 0–1 shipped; chunks 2–6 (routing table, secrets/UI, sign-in, hosted tier) owed |
| [m32-attention-plan.md](plans/m32-attention-plan.md) | Attention, not input: presence, quiet time, capture-gap accounting, the daily self-score | shipped |
| [m33-interleaving-plan.md](plans/m33-interleaving-plan.md) | Interleaved projects: context switching as a first-class timeline thing | shipped, chunk A owed |
| [site-plan.md](plans/site-plan.md) | Research behind the marketing site: mechanism over claims, one plain page | direction paper (not a milestone), folded into m34 |
| [m34-site-plan.md](plans/m34-site-plan.md) | The site: a build pulse, a coming-soon CTA, one different shape per section | shipped |
| [m35-project-first-plan.md](plans/m35-project-first-plan.md) | Project-first: projects as the organising unit, silos, sinks, a task manager view | shipped |
| [dev-tools-direction.md](plans/dev-tools-direction.md) | Research: developers-only focus, accuracy as the product, ten sub-agents | direction paper (not a milestone) |
| [m36-accuracy-plan.md](plans/m36-accuracy-plan.md) | Accuracy: a deterministic scorer with a frontier advisor over it, measured per backend | shipped |
| [m37-sources-plan.md](plans/m37-sources-plan.md) | Sources: session formats, shell/git hooks, repo discovery, remote MCP servers | shipped, small follow-ups owed (remote-URI resolver, OAuth 2.1) |
| [m38-macos-plan.md](plans/m38-macos-plan.md) | macOS: the same daemon ported, written and compiled without a Mac | shipped; real-hardware verification and signing owed |
| [m39-wayland-plan.md](plans/m39-wayland-plan.md) | Wayland: wlroots and KWin focus routes behind the existing traits | shipped; verification on a real Wayland login owed |
| [m41-connections-plan.md](plans/m41-connections-plan.md) | Connections as a surface: the connector registry, per-platform probes and health | chunks 0–1 shipped; chunks 2–5 (setup flow, mined list, requests, the review loop) owed |
| [m42-open-source-plan.md](plans/m42-open-source-plan.md) | Open source: the licence, a synthetic corpus, a clean git history | in progress |
| [m43-site-and-docs-plan.md](plans/m43-site-and-docs-plan.md) | The site and the docs caught up: new screenshots, and this docs index | in progress |

## Generated files

`docs/connectors.json` is generated from the connector registry — it is
what the site's tools page will be built from and what `chronicle status`
reports against. Regenerate it with `chronicle connections --json` rather than
editing it by hand; `crates/core/tests/connectors_json.rs` fails the build
if the committed file drifts from the registry.
