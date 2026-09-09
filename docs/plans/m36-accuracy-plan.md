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

### 2. Corrections as memory

- Migration 030 (029 is M35's `spans.project`): `task_embeddings(task_id,
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
