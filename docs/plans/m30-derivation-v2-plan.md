# M30 — Derivation v2: evidence first, the model last

Status: **chunk 1 built on branch `m30` (worktree `../chronicle-m30`), not
deployed** (2026-09-03; bff636b, 6149b1e — see Shipped). Research session
while m29 runs in another checkout: four sub-agents (capture inventory, pipeline map, live
DB statistics, prior art) plus a check that `llama-cpp-2` 0.1.154 already
exposes embeddings with pooling. Every "today" claim carries a `file:line`;
every number is from the live DB (last 7 days, 2026-08-27 → 09-03) or the
m27 plan. James's ask: the most accurate, hands-off way to derive what a
person is doing all day and group it meaningfully — for anyone's tools,
not this machine's. This doc answers that and proposes an order. James to
edit and pick.

## The short version

Accuracy is capped by evidence shape and task identity, not by the model.
Today a 4B model reads a text digest of window titles and picks a list
index; task identity is that index plus a Levenshtein 0.9 label match;
nothing remembers what a task looked like. Three structural changes fix
most of it, and each one shrinks the model's job. None of them depends on
which editor, browser, chat app, tracker or AI tool the person uses; those
are adapters at the edge.

1. **Anchor every span at capture.** Every focus span gets a set of typed,
   tool-neutral anchors: work item, document, place (repo/folder/URL path),
   branch, tool session, people, event, terms. Adapters know how a given
   app family writes its titles and URLs, and how a given tool logs its
   sessions; the pipeline never sees app names.
2. **Tasks become evidence profiles; assignment becomes scoring.** A task
   accumulates weighted anchors from its own history and from the user's
   corrections. A segment goes to the task whose profile matches best,
   with "new task" as an explicit competitor. Magnets disappear because
   every task competes on the same evidence.
3. **Segment deterministically on anchors, then name with the model.** A
   segment runs while its dominant anchors are stable, with hysteresis so
   a chat glance folds into the surrounding work. The model only names
   new clusters, breaks near-ties, and writes the day narrative.

Corrections update profiles and retro-apply across the day. Confidence is
the scoring margin, calibrated on the replay eval, so "to confirm" appears
only where the data is genuinely ambiguous. Everything stays local.

## Who this has to work for

The design is checked against four days that are not this machine's:

| person | most of the day is in | work item lives in | "project" is |
|---|---|---|---|
| developer | editor, terminal, AI coding tool, PR pages | Jira / Linear / GitHub issues | repo |
| product manager | docs, tracker, chat, meetings, browser | Linear / Asana / Notion | initiative or customer |
| designer | Figma, browser, chat, meetings | Figma file, Notion page | client or feature |
| agency / consultant | mail, docs, spreadsheets, calls, invoicing | none, or a client folder | client |

The developer row is the only one with hard identifiers on most of the
day. The other three are held together by document titles, people and
events. Both must work with zero configuration, and the soft tier (terms,
people, domains) is the primary signal for three of the four, not the
fallback.

## What the data says (one developer's week)

Live DB, last 7 days (agent 3). Dev-heavy; used to size the problem, not
to design for.

| fact | number |
|---|---|
| focus time | 2709 min over 11 distinct apps |
| terminal share | 74.4% (2014 min) |
| of which a bare AI-tool title (`✳ Claude Code`) | 53% (1071 min) |
| browser minutes with a work-item key in the title | 48.2% |
| focus minutes overlapping any `activity_events` row | 21.1% (3 days of data) |
| focus minutes with **no** interval at all | 33% (2709 vs 1819) |
| tasks with ≤2 intervals | 39 of 58 (67%) |
| tasks with `external_ref` | 6 of 80 |
| duplicate `external_ref` | ACME-11381 ×3, ACME-11382 ×2 |
| top correction kind | `merge` 27 all-time (then reassign 17, rename 16, assign 16) |
| median derive | 89 s, 3051 prompt tokens, 192 gen tokens |

Reading: the model is asked to label the day from titles, and for half the
day the title says nothing. The general lesson is that **the richest
signal is usually not the window title**: it is the tool's own log
(an AI assistant's session file, an editor's heartbeat, a shell's
history), the URL behind the tab, or the calendar entry behind the call.
Those all exist for every tool family; the current pipeline discards or
never joins them. The correction mix (merge ≫ reassign) says the pipeline
over-mints tasks more than it misfiles time; that is an identity problem,
not a labelling problem, and it is tool-independent.

## What the code does today (agents 1 and 2, the parts that matter)

- Capture keeps `app`, `title`, `pid` per focus event (`x11.rs:149-154`);
  `pid` is stored (`storage.rs:352-354`) and never read. Editor heartbeats
  carry the full `entity` path and `language`, only the basename survives
  (`heartbeats.rs:103,121-127`). Calendar keeps id/summary/start/end and
  drops attendees/description (`gcal.rs:406-411`). AI-tool transcripts
  yield `cwd`, `gitBranch`, `sessionId` and the first prompt clipped to
  120 chars (`ai_sessions.rs:301-337`); tool calls and file paths are
  never read. Browser URLs are captured when the extension posts
  (`server/lib.rs:311-367`, `spans.url`); nothing parses URL structure.
- The sessionizer clusters on app + title similarity + URL domain only
  (`sessionizer.rs:100-139`); anchors never enter it. `anchor.rs` and
  `evidence.rs` re-derive repo/key from titles at pre-pass and digest time
  with two hard-coded patterns: one ticket regex and `~/dev/<repo>`
  (`evidence.rs:29,60-114`, `prepass.rs:81-186`).
- The model sees a numbered Open-tasks list (cap 16) and emits
  `ref | label, project, start, end, confidence`
  (`grammars/task_output_v4.gbnf`, `derive_v5.txt`). A `ref=null` label
  joins an existing task only at Levenshtein ≥ 0.9 (`merge.rs:16-19,75-127`).
  `OpenTask` is `id, label, project, declared` (`types.rs:244-250`); there
  is no per-task evidence store. `open_task_by_ref` is exact-match on
  `external_ref`, `ORDER BY id LIMIT 1` (`storage.rs:2547-2563`).
- Pre-pass rules fire first-hit-wins with fixed confidence 0.5
  (`prepass.rs:47-204`). The prompt tells the model hints are "a firm
  prior" (`derive_v5.txt:31`). Replay: 76/180 baseline, 52/171 after the
  title-key rule (`m27-derivation-plan.md:245,267`) — each strong rule in
  turn became the magnet.
- Confidence is whatever the model prints, ×0.8 for live, 0.5 for
  pre-pass; nothing calibrates it and nothing but the amber dot reads it
  (`derive.rs:279`, `home.rs:876`).
- Corrections feed back as FTS-retrieved few-shot text and an eject gate
  (`digest.rs:431-460`, `prepass.rs:107-119`); `merge`/`reassign` change
  nothing about future derivation.

## Prior art (agent 4), what carries over

- **Rize** is the closest shipped shape: rules first, LLM fallback after
  2 min uncategorised, accepted suggestions become rules. Timing.app is
  rules only; WakaTime is "project = git repo"; Timely drafts entries from
  history and makes the user approve each; RescueTime never infers
  projects; ActivityWatch is regex on app/title; Memtime rejects AI
  inference outright, citing ~80% accuracy. Nobody derives a task from
  accumulated evidence and re-scores it — that is the gap.
- **TaskPredictor** (Shen et al., IUI 2006): bag-of-words over titles and
  paths, Naive Bayes for confidence + SVM for the decision, >80%
  precision by abstaining below a threshold. **Real-time switch detection**
  (IJCAI 2007): Viterbi over the sequence lifted coverage from 40% to 65%
  at the same precision. **SWISH** (Oliver et al., 2006): unsupervised
  clustering of titles + switching patterns, ~70% on titles alone.
  Lessons that map directly: abstain-when-unsure is the right UX for
  "to confirm"; sequence smoothing beats per-minute decisions; titles
  alone plateau around 70–80%, structured anchors are what lifts it.
- Small embedding models run on llama.cpp today: bge-small-en-v1.5 (33M,
  384-d, MIT), EmbeddingGemma-300M (768-d, multilingual), nomic-embed
  v1.5 (137M, Apache). CPU latency per short text is **unmeasured** on
  this box; treat as a benchmark step.
- Getting the URL and the open document without an extension is a
  per-platform question: macOS exposes the frontmost browser's URL over
  AppleScript/JXA for Safari and Chromium browsers, and the open file for
  most apps through the Accessibility `AXDocument` attribute; Windows
  exposes the address bar and document through UI Automation; Linux/X11
  exposes neither, so the browser extension and editor plugins carry it
  there. The pipeline must not care which path filled the anchor.

## Design

### 1. Anchored spans

A span carries a set of typed anchors, written at sessionize time and
immutable with the span. The types are the design; the sources are
examples.

| anchor | strength | what it is | filled from (examples) |
|---|---|---|---|
| `item` | strong | a work item id in any tracker | key in title/URL (`ABC-123`, `#123` with repo, Linear/Asana/Notion/Trello URL ids), branch name, PR title, AI-tool prompt |
| `change` | strong | a PR / MR / review | GitHub/GitLab/Bitbucket URL path, VCS events |
| `branch` | strong | VCS branch | git poller, editor heartbeat, AI-tool log |
| `session` | strong | one run of an AI/agent tool | session log on disk (see below) |
| `event` | strong | a calendar entry | calendar collector; attendees kept |
| `doc` | medium | a named document | title with app/site suffix stripped: editor file, Figma file, Google/Notion/Confluence/Office page, PDF, spreadsheet, mail subject thread |
| `place` | medium | where the work lives | repo or project folder from any path, URL path prefix (`/org/repo`, `/workspace/board`), cwd from shell or `/proc` |
| `people` | weak | who it is with | chat DM/channel name, mail sender, calendar attendees, PR reviewers |
| `domain` | weak | site | URL |
| `terms` | soft | tokens of the humanised title | everything, for FTS/embeddings |

Storage: `span_anchors(span_id, kind, value)` indexed on `(kind, value)`.
Sessionizer boundaries stay as they are; anchors are computed per span
from the event row plus time-overlapping collector events.

**Adapters, not rules.** Extraction lives in a small table of app-family
grammars shipped with the app, each a few lines: how this family's titles
are shaped (`<doc> - <workspace> - <App>`, `<file> (<folder>) - <App>`,
`<user>@<host>:<cwd>`), which suffix to strip, which URL path segments are
ids. Families, not products: chat (`<name> (DM|Channel) - <ws> - <App>` is
Slack, Teams and Discord with different separators), trackers (key in
title or URL), code hosts (`/owner/repo/pull/N`), docs (title then site
name), editors (path in title, plus the heartbeat), terminals (cwd in
title or via the shell), meetings (calendar + mic), design tools (file
name then app). An unknown app falls back to the generic grammar: strip a
trailing ` - <App name>` / ` — <site>`, take the remainder as `doc`, the
domain as `domain`, tokens as `terms`. Users never have to write a
grammar; adding one for a new app is a table row.

**Tool session sources.** Every AI/agent coding tool logs to disk, and
those logs are the richest signal that exists for the time spent in
them: working directory, branch, the user's own words, files touched.
One collector family reads them (Claude Code JSONL, Codex CLI sessions,
Aider chat history, Gemini CLI, Cursor/Windsurf/Copilot where local logs
exist) into the same `session` anchor plus `place`, `branch`, `doc` (file
paths) and `terms` (prompts, clipped). Attach rule: the session whose log
wrote most recently before or during the terminal/editor span; concurrent
sessions split the span at their writes. Privacy: local only, prompts
clipped, no assistant text, same as today.

**Terminals and editors without a plugin.** Most terminals will put the
cwd in the title if the shell emits it (OSC 7 / `precmd`); ship a
one-line snippet for bash/zsh/fish in onboarding and read it back. On
Linux, `events.pid` → child shells in `/proc` → `cwd` covers the rest
(pick the child whose `stat` moved most recently); on macOS `lsof -p`,
on Windows the process tree. Editors mostly put the file path or name in
the title; the heartbeat plugin (WakaTime protocol, already served) adds
branch and full path for those that do not.

Acceptance metric for this layer, from the DB: share of focus minutes
with at least one strong or medium anchor. Baseline is the 21% overlap
figure; target above 80% for the developer day and above 60% for the
document-heavy days, where `doc` and `people` do the work.

### 2. Task evidence profiles

```
task_evidence(task_id, kind, value, weight REAL, first_ts, last_ts, source)
```

Rows accrue from three places: intervals (anchors of the spans under the
task, weight = minutes × strength, decayed by age), corrections
(`assign`/`merge`/`reassign` copy the moved segment's anchors onto the
target at a bonus weight and, for the source of a reassign, a negative
row; `eject` writes negative rows), and declarations (item id, project,
folder; weight equal to one ordinary strong match, no more).

Scoring a segment against a task: sum over the segment's anchors of
`strength × min(1, task weight for that value)`, normalised by segment
minutes, plus a soft term from FTS or embedding cosine between the
segment's `terms` and the task's term profile, plus a small recency prior
(last interval within 2 h). "New task" scores a flat calibrated constant.
The winner takes the segment when its margin over the runner-up exceeds
δ; below δ the segment is `to confirm` with the top two as one-click
options, and the model tie-breaks only if its answer would be
auto-accepted (its pick counts as evidence, not authority).

For the document-heavy days the profile is mostly `doc`, `people`,
`event` and `terms`, and that is enough: a task is "the thing whose
documents, people and words these are". A PM's initiative spans a
Notion page, two Linear items, a Slack channel and a weekly call; all
four are rows in one profile after the first day.

What this fixes, by construction: a place match is medium strength
against every task in that place, not a jump to the most recent one; a
declared task with no evidence has one weak row; an item id names the
task that has done that item's work, not the lowest id (m29 chunk 7 is
subsumed); two batches that look alike land on the same profile, so
`merge` traffic drops.

### 3. Segmentation on anchors, hysteresis, then naming

Replace "the model emits intervals from Timeline lines" with a
deterministic segmenter over the anchored span stream:

- A segment's signature is its dominant strong/medium anchor set over a
  sliding window. The segment continues while the signature holds.
- A switch requires the new signature to hold for `switch_min` (3 min) or
  a strong anchor change (new item/branch/session/event). Shorter
  excursions (chat, a lookup, a tracker page for the same item) fold into
  the enclosing segment and are recorded as its `kind` mix, not as
  separate tasks.
- AFK ≥ 5 min still ends a segment (the M5 time-honesty rule stands).
- Distractions (config patterns, plus domains with no task profile and no
  item) become `kind = break` inside a segment, never a task.

Scoring (section 2) assigns each closed segment as it closes, in the
daemon tick, with no model in the loop. Coverage becomes immediate: the
33% of focus time with no interval goes away because assignment no
longer waits for a batch and a 89 s derive.

The model's remaining jobs, all small and cached-prefix friendly:

- **Name a new cluster** from its evidence (item title from the tracker
  or MCP, branch, PR title, document titles, prompt words, people) — one
  short JSON with `label, project, kind`. Once per new task, not per
  batch. Works the same for "ACME-11382" and "Q4 pricing page draft".
- **Tie-break** a low-margin segment between two named tasks, given their
  profiles. Rare by design.
- **Day narrative and standup** — unchanged in role, better input.

The batch tier becomes a reconciliation pass (re-score the day's segments
with the day's final profiles, propose merges through the existing
`consolidate` guard) and can be removed once the replay says it adds
nothing. The live tier's 5-min LLM call goes away; the resident worker
stays for naming.

### 4. Learning that needs no attention

- A correction updates the profile and immediately re-scores every
  not-yet-confirmed segment of the same day; segments that flip show up as
  one "also moved N blocks" line with undo. One click, whole day.
- `to confirm` rows left alone for 24 h are accepted at their best guess
  and the margin is recorded as a passive positive example; rows the user
  changes are strong examples. Both feed calibration.
- Calibration: `replay.rs` already turns corrections into probes. Add a
  second score, "confidence honesty": bucket segments by margin and check
  what fraction the user corrected. Set δ so `to confirm` covers roughly
  the worst 10% of segments. The stored confidence becomes that
  calibrated value, and the amber dot means something.
- Lifecycle: recency decay does what auto-close does today but gradually;
  a task with no evidence for `task_autoclose_days` stops competing and
  reopens itself if its item, branch or document comes back.
- Projects are learned too: tasks whose profiles share a strong `place`
  or item prefix, or a `people`+`doc` cluster, roll up into one project;
  the project label is the folder, the tracker prefix, or the client name
  the model reads off the documents. A declared project is a hint with
  the same weight as anything else.

### 5. Grouping people can read

Profiles give the hierarchy for free: project → task → segments, and
each segment carries a `kind` from app family plus anchors, general
across roles:

| kind | signal |
|---|---|
| `author` | editor / doc / design tool with a `doc` or `path`, writes happening |
| `agent` | AI/agent tool session |
| `review` | change pages, diff tools, comments on a `doc` |
| `communicate` | chat, mail |
| `meet` | `event` overlapping mic or a meeting app |
| `plan` | tracker pages, boards |
| `read` | docs and sites with no `item` and no writes |
| `admin` | billing, HR, expense, settings families |
| `break` | distraction patterns, no profile match |

Reports then say "Pricing page · 2h 10m — author 1h 30m, meet 25m,
communicate 15m" for a PM as naturally as "ACME-11382 · 2h 10m — agent
1h 30m, review 25m" for a developer, and the day view collapses a task's
excursions under it. Nothing new is typed; the grouping is the evidence.

### 6. Embeddings, soft tier

Titles without hard anchors are where TaskPredictor-style term matching
lives, and for non-developers that is most of the day. Start with what
exists: SQLite FTS over `terms` (the `corrections` FTS pattern,
`prepass.rs:173`) scored BM25-ish, plus a `people` and `doc` exact match.
Add a small embedding model through `llama-cpp-2`'s embedding API
(`context/params.rs:47-57` pooling types are present) after a bench on
this CPU: target under 20 ms per span; store 384-d f32 blobs on the
`terms` row and a centroid per task. Prefer a multilingual model
(EmbeddingGemma) over bge-small if the bench allows, since titles are in
whatever language the user works in. If the bench misses, FTS stays.
Either way it is one term in the score, never the decision.

## Chunks

1. **Anchored spans.** Migration `span_anchors`; anchor extraction in the
   sessionizer through the app-family grammar table with the generic
   fallback; URL structure parsing; keep editor heartbeat path and
   language, calendar attendees; tool-session collector family (Claude
   Code first, the others are table rows) with all prompts and touched
   paths; shell cwd snippet + `/proc` spike. Backfill over existing
   spans. Metric: anchored-minute share. No behaviour change yet; the
   digest gains the anchors as evidence lines, which alone should help
   the current model.
2. **Profiles and scorer, offline.** `task_evidence` built from existing
   intervals and corrections; scorer as a pure function in `core`; replay
   eval scored with the scorer alone and with scorer + model. Add two
   synthetic persona fixtures (PM day, agency day) in the `eval.rs` style
   so the scorer is never tuned on one developer's week. Ship only if the
   scorer alone beats 76/180 on the same probes and holds on the
   fixtures. This is the gate for everything after.
3. **Segmenter + scorer as the live path.** Replaces pre-pass and the
   live-tier LLM call; naming job for new clusters; batch derive demoted
   to reconciliation behind a config flag. Feed shows segments as they
   close. Metric: replay, coverage share, median segment length,
   `merge` corrections per day.
4. **Learning and calibration.** Corrections → profile updates → same-day
   re-score with the "also moved" line; passive acceptance; confidence
   honesty score; δ from data; amber dot re-meaning; learned projects.
   Remove the digest's few-shot corrections section once profiles carry
   the same information.
5. **Kinds and grouping surfaces.** `kind` per segment, per-task kind mix
   in reports and the task detail, excursion folding in the day view.
   Coordinates with m29's row grammar (state word stays; kind becomes
   the meta line's first token).
6. **Embeddings.** Bench, then the model behind the soft term; FTS
   fallback kept.
7. **Retire the batch tier** if chunk 3's reconciliation shows no gain on
   replay for a week. Keep `bench --replay` as the regression gate.

Order rationale: 1 is pure capture and helps the current pipeline; 2 is
the go/no-go with a number; 3 is where the user feels it; 4 is what
makes it hands-off; 5–7 are polish and cost.

## Risks and open questions

- **Concurrent tool sessions in one place** (two agents on one repo, two
  docs in one folder): attach by nearest log write. Wrong attachments
  split a segment, they do not mislabel it, because both share place and
  usually branch.
- **Profile lock-in**: a task that absorbs a whole place or a whole
  channel becomes the new magnet. Mitigation is in the scoring: place and
  people are medium/weak, capped per value, and "new task" competes at a
  calibrated constant; watch the replay's per-task landing histogram (the
  36-probes-on-one-task signal from m27) as a standing check.
- **Cold start**: with no profiles, everything scores "new task" and the
  namer runs a lot on day one. Seed from existing intervals (chunk 2
  backfill) here; a new install has a naming-heavy first day and that is
  fine, and the persona fixtures say how it feels.
- **Grammar drift**: apps change title formats. The generic fallback
  keeps `doc`/`domain`/`terms` flowing when a family grammar stops
  matching; log unmatched families so the table can be updated.
- **Privacy surface grows** with file paths, document titles, people and
  prompts. Local-only as today; the Storage "what leaves this machine"
  line does not change because nothing leaves. Keep the shell precedent
  (no command lines); store prompt text clipped; let a family be turned
  off (mail subjects, DMs) in Settings.
- **Platform**: anchors are the contract; the capture side differs (X11
  today, AT-SPI/Wayland portals, macOS Accessibility + AppleScript,
  Windows UI Automation). Chunk 1 designs the anchor set and the grammar
  table so the Mac/Windows collectors from `site-plan.md` fill the same
  rows.
- **Embedding latency** unmeasured; hence chunk 6 and a bench gate.
- **m29 overlap**: m29 is UI-only apart from chunk 7 (duplicate
  `external_ref`), which this plan replaces; suggest m29 chunk 7 ships
  only the one-off data fix and the declare-path warning.

## Out of scope

- New remote collectors (tracker APIs, chat APIs, mail). Anchors from
  titles and URLs cover the reading side; publishing stays m28-parked.
- Model upgrades. The design makes the 4B adequate; an 8B for naming is a
  config change, not a plan.
- Teams sync. Profiles are local; the derived-rows-only rule from
  `teams-direction.md` still holds for whatever leaves later.

## Shipped

### Chunk 1 — anchored spans (2026-09-03, bff636b + 6149b1e, branch `m30`)

What landed: migration 016 (`span_anchors(span_id, kind, value)` with a
`(kind, value)` index; `activity_events.detail` JSON). `core::extract`:
`AnchorKind` (item, change, branch, session, event, doc, place, people,
domain) with fixed strength; `Family` by window class (terminal, editor,
VCS GUI, browser, chat, mail, meeting, document, other) from a substring
table; `extract(app, title, url, ticket_re)` with per-family title grammars
(path → place, `file - project - App` and `project – file [module]`
editors, `Name (DM) - WS - Slack` / `#chan - Server - Discord` /
`Chat | Name | Teams`, mail subject unless a folder, `Note - Vault -
Obsidian`, agent-tool conversation titles as docs) and site rules by domain
(GitHub/GitLab/Bitbucket pull/issue paths → change/item + place, Jira
`/browse/KEY`, Linear, Asana, Trello, ClickUp, monday ids, Google Meet and
Zoom rooms → event, Slack/Teams web → people, Gmail/Outlook subject, every
other site → page title with the site name stripped, plus the registrable
domain); `from_activity` attaches overlapping collector events by family
with a same-place gate (an AI session, edit fold, shell fold or cwd probe
only lands on a terminal/editor span that names no other place), meeting
→ event + attendees, and the latest checkout per named place → branch (+
item from the branch). `storage::anchor_spans` recomputes the tail on every
sessionizer refresh (anchors go with their spans in `replace_tail`, trail
the span delete in `prune`); `chronicle backfill-anchors [--since]` and
`chronicle anchors [--days] [--top]` (coverage by strength, top values).
Collectors keep what they used to drop: editor heartbeat full path and
language, calendar attendees (display name else email, self and rooms
excluded, 12 max), AI-session prompts (12) and Edit/Write/Read tool-call
paths (40, relative to cwd) per segment; a session started in the home
directory has no place. New `ActivityKind::Cwd`: the X11 provider reads
the focused terminal's newest child shell cwd from `/proc` on focus and
on title change, one upsert row per `(pid, place)`; excluded from the
digest Activity lines and task activity queries, rendered nowhere. Digest
gains `## Documents` (top 5 by focus time) and `## People` (top 4) from
the same extraction over the batch's spans; goldens re-blessed (day1: Rust
docs; day3: the SMS failure page, RDS console, dashboard pages).

Numbers, live DB copy, last 7 days, after `backfill-anchors` (before the
cwd probe existed, so terminals with a bare agent title carry only the
session): focus 2701 min; **strong 70.4%, medium 12.9%, weak 3.5%, none
13.2%** against the 21% activity-overlap baseline and the ≥ 80% target.
The unanchored rest is Chronicle's own window (139 min), agent-tool
terminals from before the session collector existed (09-01), `New Tab`,
and bare shell commands (`git log`, `sudo -u postgres psql`) that the cwd
probe now covers live. Top values: place chronicle 1558 min, branch main
1585, item ACME-11342 1126 (from the backend repo's session branch),
two concurrent long sessions at 1040 and 955 min — the two-agents case;
both attach to a bare `✳ Claude Code` terminal, which is the honest
answer until sessions carry per-write timestamps.

Not done in chunk 1: the shell cwd-in-title snippet for onboarding (m29
owns the onboarding UI; a docs line for now), the `/proc` probe is
untested live (needs the daemon restarted on this build), Mac/Windows
capture. Deploy waits for m29 to land first (migration 016 would strand
an older binary; see the m29 conflict note).
