# M36 — Accuracy: API models where they buy it, measured

Status: draft for James's edit, 2026-09-09. Research and rationale in
`dev-tools-direction.md` ("Maximum accuracy"); this is the build order.
Runs after M35 chunks 0–1 (project identity and sinks), and can interleave
with M37 (sources). Every "today" claim carries a `file:line`.

## The short version

The deterministic pipeline keeps every decision people bill against:
segment boundaries and the segment → task score. A frontier model does the
judgement work around it (naming, descriptions, merges, the standup, chat)
and advises on the verdicts the scorer is not sure about. Corrections
become examples the model sees. Every claim the model makes carries an
evidence id the pipeline verifies. The whole day is reconciled once a
night on the Batches API. Six metrics say whether any of it helped, per
backend, on the same days.

Chunk 0 exists because the M31 gate has never run: this box has no API key
and James does not want to set up a card to find out. Chronicle spawning
the user's own Claude Code (`claude -p`) is a backend like any other and
runs the gate today.

## What the code does today

- Routing: `ModelsConfig::route_for` (crates/core/src/models_config.rs:162);
  the decision and the $2/day cap in `run_routed`
  (crates/app/src/ai_job.rs:84); local fallback on any `CloudError`.
- Backends: `BackendKind { Anthropic, OpenAiCompat }`
  (crates/core/src/models_config.rs:72); `cloud::build`
  (crates/derive/src/cloud/mod.rs); the Anthropic backend streams SSE, sends
  `output_config.format` for JSON jobs and `effort` per job
  (crates/derive/src/cloud/anthropic.rs); `TextBackend` / `Request` /
  `Completion` (crates/derive/src/text.rs).
- Deterministic: `Verdict { ranked, margin, confident }`
  (crates/core/src/segmenter.rs:774), the scorer
  (crates/core/src/profile.rs:824), the place veto (segmenter.rs:711).
- Corrections: `corrections` table (crates/core/migrations/001_schema.sql:45,
  extended by 027); read by the bench replay only.
- Embeddings: `crates/derive/src/embed.rs`, bench-only, unstored.
- Redaction: only `/no_think` is stripped (crates/derive/src/prompts.rs:25).
- Caching: `cache_control` on an empty system message
  (crates/derive/src/cloud/anthropic.rs:78); templates are one user message
  (~1100-token instruction + ≤2200-token digest for derive), not split.
- Eval: `chronicle bench --replay --backend <name> --since N`
  (crates/app/src/bench.rs:1263); nightly `self_score` (migration 027:
  `placed_ms`, `minted`, `merged`, `verdicts`, `wrong`, `confident`,
  `confident_wrong`, …); `ai_jobs.backend/prompt_tokens/gen_tokens/cost_usd`
  (migration 023).
- Local baseline on the replay: 37/134 scorer, 44/134 qwen3-4b
  (`m30-derivation-v2-plan.md`).

## Chunks and gates

### 0. `claude_code` backend and the gate

- `BackendKind::ClaudeCode`; `BackendCfg` gains an optional `command`
  (default `claude`) and `api_key` is unused for it (the UI hides the
  field; `masked_key` returns "your Claude Code login").
- `crates/derive/src/cloud/claude_code.rs`: spawn
  `claude -p --bare --output-format json --model <model> --max-turns 1
  --no-session-persistence [--json-schema <schema>] [--append-system-prompt
  <system>]`, prompt on stdin, cwd a scratch dir (never a repo: Claude Code
  reads CLAUDE.md from cwd). Parse `result`, `usage.input_tokens`,
  `usage.cache_read_input_tokens`, `usage.output_tokens`, `total_cost_usd`
  into `Completion`. Chat: `--output-format stream-json
  --include-partial-messages`, text deltas to `on_token`; history folded
  into the prompt as the local engine does. Non-zero exit, a missing
  binary or a `is_error` result map to `CloudError` (Auth when the output
  says not logged in; Transport when the binary is missing).
- `cost_usd` for this kind reads `total_cost_usd` from the reply (list
  basis) rather than the price table; the Settings egress line says
  "quota on your Claude plan, ≈ $x at list".
- Try `--exclude-dynamic-system-prompt-sections` and measure the token
  overhead per call (about 25 k cached tokens in the 2026-09-09 test).
- Settings › Model: "Use your Claude Code login" as a third backend kind
  with a detect button (`claude --version`) and the note "for people who
  already have Claude Code".
- Gate: `chronicle bench --replay --backend claude --since 7` runs to
  completion on this box with no key and prints per-probe results, tokens
  and the list-price estimate. Its placement number against 37/134 and
  44/134 decides the `derive` route default.

**Built and run 2026-09-09** (uncommitted at the time of writing; the
Settings add-form still creates Anthropic backends only, so the entry is
written by hand: `[backends.claude]`, `kind = "claude_code"`,
`model = "claude-sonnet-5"`). Findings from the build: `--bare` skips the
stored login ("Not logged in · Please run /login"), so it is never passed;
`--json-schema` spends a tool turn, so `--max-turns` is 3 and the answer
is read from `structured_output`; `--system-prompt` does not cut the
overhead (about 25–65 k cached tokens of Claude Code's own context ride
along per call); a result object with `is_error: true` can come with exit
code 0. Same sandbox copy of the live DB, same 77 probes from 26
corrections over 39 batches, `--since 7`:

| path | placed | total | lenient | wall | cost |
|---|---|---|---|---|---|
| scorer only (`--scorer`) | 20/75 | 22/77 | 37/77 | seconds | 0 |
| Claude Code, claude-sonnet-5 (`--backend claude`) | 27/75 | 28/77 | 36/77 | 3149 s (one batch hit the 240 s timeout) | $5.09 at list, on the plan's quota |
| local qwen3-4b (`--model qwen3-4b`) | not run: the session's tool harness killed the run twice for memory pressure (6 GB of swap in use beside the daemon's own worker); run it from a terminal with `XDG_DATA_HOME=<sandbox> chronicle bench --replay --since 7 --model qwen3-4b` on the same copy | | | ~65 min | 0 |

Reading: the frontier model places 6 more probes than the scorer on the
strict count and none more on the lenient one, at 80 s a batch. Its
transcripts show the same failure the 4B has: minting a new task
("fixing ACME-11533 Export Selected modal bug") beside an open task the
correction wanted. That is the M30 thesis again: task identity and
evidence shape cap the number, not the model. The `derive` route stays
local by default; the frontier model earns its place as the advisor on
low-margin verdicts (chunk 3), not as the batch deriver. Two `claude_code`
limits to note: no token streaming (chat renders at the end) and a
per-call floor of about a minute, which rules it out for the live tier.

### 1. Redaction and the prefix split

- `derive::redact` runs on every cloud-bound `Request`: drop URL query
  strings and fragments, path segments that look like tokens (≥ 20 chars of
  base64 or hex), matches of the secrets patterns (AWS `AKIA…`, `sk-…`,
  `ghp_…`, JWTs, `postgres://…@`, `Bearer …`), and the command line of any
  shell span (only program names ever reach the digest today; keep it
  that way by construction). Unit tests with fixtures per pattern.
- Split every template into a frozen prefix (instruction, schema, project
  rules, the open-task list sorted by id) and the volatile digest; the
  prefix goes in `system` with the `cache_control` breakpoint, the digest
  in `user`. Log `cache_read_input_tokens` per job (already on
  `Completion`).
- Gate: the redaction tests pass; on a day of real jobs through the
  Anthropic backend the cache-read share is ≥ 85 %; the Settings egress
  line lists the redaction classes applied.

**Shipped 2026-09-09.** `derive::redact` (crates/derive/src/redact.rs) with
eight classes (JWTs, AWS keys, `sk-` API keys, GitHub tokens, bearer
tokens, URL credentials, URL query strings and fragments, opaque path
tokens: 20+ chars all hex, or mixed-case base64 with four digits and no
hyphen so slugs and branch names stay); `cloud::build` returns every
backend behind `cloud::Redacting`, which cleans system, user and both
sides of the history and reports the classes on `Completion.redactions`.
`prompts::split_prefix` cuts each rendered prompt at its template's data
marker and moves the digest's `## Open tasks` section into the prefix
(the list is already in `created_ts, id` order); `ai_job.rs`, the chat
worker and the bench put the prefix in `system` (cache breakpoint on the
Anthropic backend, `--system-prompt` on Claude Code) and the digest in
`user`. Cloud jobs log `cache_read` per job and merge their redaction
classes into meta `redactions:<date>`; the egress line reads "redacted
before sending: …" or "nothing matched the redaction filters" after the
backend counts. Shell command lines need nothing: only program names
reach the digest. Gate: redaction and split tests pass (305 workspace);
`claude -p --system-prompt … --json-schema …` returns `structured_output`
(one call, $0.11 at list); the ≥ 85 % cache-read share on the Anthropic
backend waits for a key, and the derive prefix is the only template long
enough (≥ 1024 tokens) to be cached at all — naming and description
prefixes are ~200 tokens, so their split is for shape, not savings.

### 2. Corrections as memory

- Migration 032 (029 is M35's `spans.project`, 030 its `tasks.current`, 031 its self-score columns): `task_embeddings(task_id,
  kind, vec BLOB, ts)` and
  `correction_embeddings(correction_id, vec BLOB)`; the vectors from
  `embed.rs` (bge-small, 384 floats, already downloaded). Embed on task
  create, rename, and on each correction; a cosine scan over a few
  thousand rows needs no index.
- `derive::examples::nearest(kind, text, k)` returns the k nearest
  corrections rendered as "evidence → wrong task → right task" lines.
  `name_task`, `suggest_task`, `consolidate` and the chunk-3 advisor get
  k = 3–5 in their prompts.
- Corrections also nudge the scorer's profile weights, as M30 intended, so
  the deterministic path learns too.
- Gate: on the replay, naming quality (chunk 5's pairwise judge) improves
  with examples versus without; the embedding pass adds under 50 ms per
  correction.

**Shipped 2026-09-09.** Migration 032 adds `task_label_embeddings(task_id,
label, vec, ts)` (the m30 `task_embeddings` table is the span centroid and
keeps its name; `label` is the text the row was made from, so a rename
re-embeds without a hook) and `correction_embeddings(correction_id, vec)`
over the example kinds (rename, assign, reassign, merge, eject; journal,
checkpoint, rescore and consolidate rows are bookkeeping). The daemon tick
embeds new labels and corrections beside the span pass (`embed_new_examples`,
50 rows a tick) with `embed_model` or, unset, the downloaded bge-small
preset (`model::resolve_embed_or_default`; span vectors stay opt-in);
`chronicle backfill-embeddings` does the same in one go and prints the
per-row cost. `derive::examples::Examples::nearest(conn, job, text, k)`
embeds the work's "app title" lines, scans `correction_embeddings` by
cosine (kinds per job: naming sees rename/assign/reassign/merge,
consolidate sees rename/merge, the advisor sees all five), keeps matches
above 0.6 and dedupes by outcome; without a model it filters
`storage::correction_hints` (FTS) the same way. `name_task` and
`suggest_task` pass the four nearest into `build_digest` so the digest's
own "Past corrections" section renders them; consolidate appends
`examples::render_section` ("evidence → wrong → right", `✗` for ejects) to
its input, and both templates say what the section is. `chronicle bench
--examples "<app title>"` prints the cosine and FTS answers side by side.
The scorer already learns from corrections: `refresh_task_evidence` writes
`Source::Correction` rows at `correction_min` minutes (profile.rs:480),
M30 chunk 2's design, so nothing new was needed there. Gate on the live
DB's copy: 112 labels at 3.5 ms and 87 corrections at 21.4 ms each
(under the 50 ms bound), lookups 9–11 ms after a 32 ms model load; on
"Firefox Jira ACME-11533 Export Selected modal" cosine returns the
ACME-11533 assign first where FTS returns it first too but fills the rest
with monetization and UI rows; the pairwise naming gate waits for chunk
5's judge.

### 3. The pairwise advisor

- New job kind `advise` (JSON). Trigger: a `Verdict` with `confident =
  false` and at least two ranked candidates. Prompt: the segment's
  evidence rows with ids, the top two candidate task profiles, the
  general task, the nearest corrections, and the question "A, B, new, or
  unsure", with `evidence_ids` for the rows that decided it.
- The pipeline re-ranks only when every cited id is in the segment and the
  answer is A or B; "new" mints as the scorer would have; "unsure" leaves
  the verdict as placed and marks it "to confirm" as today. The reason
  string records "advisor: <answer>".
- Online it runs on the routed backend with `effort` low and a per-day
  count cap; the nightly pass (chunk 4) re-runs the day's advisories on
  the nightly model.
- Gate: on the replay, placements the advisor changed are right more often
  than they were wrong (precision ≥ 0.7 against corrections) and the
  abstention rate is under 20 % of low-margin verdicts; a `bench
  --scorer --advisor` flag replays with and without.

**Shipped 2026-09-09.** `JobKind::Advise` with `prompts/advise_v1.txt`, a
GBNF that requires at least one cited id, and a JSON schema;
`derive::advise` renders the segment's focus rows by `spans.id` ("id 412:
25m Firefox: Stripe API reference [place:shop]"), both candidates with
their eight strongest profile keys, and the nearest corrections; the
answer is A, B, new or unsure, and `decide` re-ranks only when every
cited id is a row of the segment (no citation, or one outside, is
`invalid` and changes nothing). Migration 033 adds `verdict_log.advice`.
The daemon tick queues an `advise` job for each unsure reconciled verdict
with a runner-up, 60 a day, today only; the worker keeps (A), moves the
interval to the runner-up with reason "advisor: B" (not a correction: the
scorer never learns from it as truth), mints under the placeholder label
and queues the naming job (new), or leaves it to confirm (unsure), then
refreshes the touched tasks' evidence. `chronicle bench --replay --scorer
--advisor [--backend NAME]` asks on every unsure probe and prints asked /
changed / unsure / invalid and the unsure-verdict probes passing with and
without. Gate, sandbox copy of the live DB, `--since 7` (48 probes on
2026-09-09 evening): local qwen3-4b asked 32, changed 2, unsure 2 (6 %),
invalid 25 — every invalid was an empty citation under the first grammar,
which is why the grammar now demands one — 11 unsure-verdict probes
passing with the advisor against 10 without. Frontier, `--backend claude`
(claude-sonnet-5 through Claude Code, same 48 probes): asked 32, changed
16, unsure 2 (6 %), invalid 0 — every answer cited rows of the segment —
and 10 unsure-verdict probes passing with the advisor against 10 without:
it moved as many right placements wrong as wrong ones right, so the
precision gate (≥ 0.7 on changed placements) is not met on this
correction set, though abstention (6 %) is. Consequence, shipped the same
evening: the online advisor runs only while `advise` has a cloud route
(the daemon never queues for the local model; a locally-engined advise
job skips with advice 'skipped', which the night pass may re-ask), and
the 4B is asked only by the bench, on purpose. The number to beat before
routing it: the M30 thesis holds here too — most of the 32 unsure verdicts
sit between a task and a near-duplicate of it, where the evidence rows
cannot decide.

### 4. Claims carry evidence, and the night pass

- `standup`, `narrative`, `task_description`, `journal` schemas gain
  `claims: [{text, evidence_ids}]`; the pipeline resolves every id against
  `activity_events`, `notes`, `journal` and `checkpoints`, drops claims
  that do not resolve, and re-runs the job once if more than a third
  dropped. The rendered text keeps a footnote marker per claim that the UI
  turns into the evidence popover Home already has for descriptions.
- Nightly: one `reconcile_day` job builds the whole day (segments, low-
  margin verdicts, unnamed tasks, merge candidates, the standup) into one
  Batches API request set through the Anthropic backend
  (`/v1/messages/batches`, results polled, keyed by `custom_id`), on the
  nightly model at half price. The `claude_code` backend runs the same set
  sequentially. The online pass never rewrites yesterday.
- Gate: faithfulness (share of claims whose ids resolve) is 100 % on the
  rendered output and logged before the drop; a night's batch completes
  under the cap and the morning Home shows the reconciled day.

**Shipped 2026-09-09.** Claims: `derive::claims` numbers every evidence
line (`E7: …`) before a description, journal or narrative prompt renders
(`_v2` templates, `grammars/claims_v1`), the model answers `{claims:
[{text, evidence_ids}]}`, `verify` keeps the ids that name a sent line and
drops a claim left with none, the job runs once more when more than a
third dropped, and the text is stored with a `[^n]` marker per claim; the
claims themselves — each with the evidence lines it rests on — go to the
new `claims` table (migration 034; keyed description → task id, journal →
task:batch, narrative → lo:hi, standup → day), read back into the
timeline's summary line and journal rows and the reports' narrative card
as a hover popover. The standup keeps its `[source]` tags as ids:
`standup_claims` resolves each sourced bullet to the DATA lines carrying
the same tag. Faithfulness before the drop lands on `ai_jobs.claims /
claims_ok`, and `ai_jobs.cache_read_tokens` now persists (chunk 5 reads
both). Resolution is against the lines the prompt carried, which are the
`activity_events`, journal, checkpoint and span rows rendered into it —
the popover shows those lines rather than re-querying the tables. Night
pass: `TextBackend::batch` (sequential by default; `Redacting` cleans
each) and on the Anthropic backend the Message Batches API — one request
set, polled every 30 s, results keyed by `custom_id`, each billed at half
list on `Completion.cost_usd`; the `reconcile_day` job (route key
`reconcile_day`, "night pass" in Settings, flipped by the Everything
preset) gathers the day's advisories (every unsure verdict, no cap), its
tasks still under a placeholder label and the standup, runs each through
a collecting engine that records the prompt and stops, sends the set in
one batch, then re-runs each through a replaying engine that answers from
the batch — a job whose rows changed in between renders a different
prompt, finds no answer and fails, which is the right outcome. The
daemon enqueues it once per day in place of the standup job when the
route exists (`storage::job_seen` dedupes), the worker gets a four-hour
timeout for it, and the online advisor now looks only at today. Gate: the
claims and batch tests pass (316 workspace, the batch against a loopback
mock); faithfulness on real jobs and a night's batch under the cap wait
for a key — the local 4B answers the claims grammar, and its first live
descriptions will show the rate in `chronicle status` once chunk 5 lands.

### 5. Six metrics, per backend

- On the `self_score` row and in `chronicle status`, per backend name:
  placement precision and recall against corrections (replay), claim
  faithfulness before the drop, advisor abstention rate, same-day re-run
  drift on names and the standup, cache-read share, cost per day.
- `bench --judge`: pairwise Opus 5 judge between two naming prompts on the
  fixture set, output a win rate and the pairs for James to spot-check.
- Settings › Model shows the six numbers for the last 7 days next to each
  backend so the local and cloud routes are compared on the same days.
- Gate: the numbers render for both a local and a cloud backend over one
  week; the doc's "First actions" all show green or a number.

**Shipped 2026-09-09.** Migration 035 `backend_score(day, backend, …)`:
beside each day's self-score the daemon folds `ai_jobs` per backend
('local' when none) — jobs, claims and claims_ok (faithfulness before the
drop), advisor questions, unsure and invalid answers, and the changed
placements (Move/Mint) joined to `verdict_log.outcome` for right/wrong —
plus prompt tokens, cache-read tokens and cost. `BackendSummary::line`
prints the six numbers on one line ("anthropic: 4 jobs · faithfulness 95
% (19/20 claims) · advisor unsure 25 % of 8, invalid 2, changed 4: 4
right 0 wrong · cache-read 75 % · $0.50/day · replay 27/75 placed
(2026-09-09) · drift not run"); `chronicle status` prints one per backend
under "per backend, same days" (and `--json` carries the rows), Settings ›
Model shows the same line under each backend row and under a "local
model" heading. Replay placement and re-run drift are bench numbers kept
in meta: `bench --replay` writes `replay_score:<scorer | backend>`
("14/46 placed (date)"), the new `bench --drift [--backend]` re-runs
yesterday's standup and five naming prompts and writes `drift:<backend>`
(mean word-set Jaccard), and the new `bench --judge [--backend]` names
ten recent derived tasks with and without past-correction examples on the
local model, has `--backend` (else the local model) pick the better label
pairwise with positions alternating (`prompts/judge_v1`, `JobKind::Judge`,
never routed), prints every pair and writes `judge:<backend>` ("examples
win a, lose b, tie c of n") — chunk 2's naming gate. Gate: the numbers
render for 'local' as soon as the daemon's next daily self-score runs
(the migration is empty until then); a cloud backend's week waits for a
key. On the sandbox copy, local qwen3-4b: `bench --drift` similarity 1.00
over five naming re-runs (deterministic sampling; no standup row for the
copy's yesterday), `bench --judge` below.

Order 0 → 1 → 2 → 3 → 4 → 5. Chunk 0 is a day and unblocks the decision;
1 is a prerequisite for any user other than James; 2–3 are the accuracy
work proper; 4 is the trust work; 5 keeps it honest.

## Model choice

`claude-sonnet-5` for online jobs (naming, descriptions, journal,
advisor), `claude-opus-5` for the nightly pass, merges, standup and chat,
`claude-haiku-4-5` for the live label if latency demands it. Through
`claude_code`, the same ids or the aliases `sonnet` / `opus` / `haiku`.
Effort as `effort_for` sets it (crates/derive/src/cloud/mod.rs).

## Open questions for James

- Is the `claude_code` door shown on the site, or only in Settings until
  Anthropic confirms a local program may spawn `claude -p` on the user's
  behalf?
- Advisor cap per day (proposal: 60 online, unlimited nightly).
- Whether merge proposals from the nightly pass auto-apply under a size
  (proposal: never; Home shows "3 merges proposed" with one-click accept).

## Out of scope

Fine-tuning, a hosted judge, teams, and any change to capture (M37).
