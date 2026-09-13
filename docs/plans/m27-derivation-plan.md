# M27 — Derivation: accurate, fast, visible

Status: **chunks 1–7 shipped** (2026-09-03; a899606, 81f1853, ed3f387, b7d87ab, 799af4d, 4043e14, bbb249d — see Shipped). Open: the post-chunk-5 replay scored 52/171 against the 76/180 baseline (title-key rule over-fires; see chunk 5 Shipped) and a quiet-box soak day for the chunk 3 numbers. Research: four passes (daemon scheduling + logs, live DB statistics, feed/timeline surfaces, eval history) plus live benches of both model presets on batches 66–68. Every "today" claim below is verified in code or data with a `file:line`; every number comes from the live DB, the daemon log, or a bench run on this machine (Ryzen 5 5625U, 6 cores, 14 GB, Vega iGPU, no discrete GPU).

Numbering: m26 (daily driver) is in flight in another session. The teams doc reserved m27 for "publish outward" and framed derivation quality as the gate for it (`teams-direction.md:289`); this milestone takes m27 and publishing slides to m28. None of the chunks below touch m26's files (Home layout, calendar, Wakapi, atuin, intent, Jira write-back). m26 chunks 3–4 will add cwd/file evidence that chunk 5 here should consume once both land.

## Context

Chronicle's whole value is the derived layer: watch what the user does, decide what they were working on, group it into tasks, show it in a feed that makes sense. Today that layer is a 30-minute batch that a 4B model labels two minutes at a time, a rule-based pre-pass that fills the gap, and a feed that shows the result without showing the work. It is right often enough to be useful and wrong often enough to need 77 hand corrections in seven days. This plan fixes the failure modes the data actually shows, makes each derive materially faster on the same hardware, adds a live tier so the feed reacts in minutes instead of half an hour, and gives the user a window into the brain while it thinks.

## Today (verified in code, 2026-09-03)

Pipeline:

- Events → spans → batches. A batch closes after `batch_minutes` (30) of non-AFK time, or at an AFK gap of 30 min or more (`crates/core/src/sessionizer.rs:229-263`, `BATCH_BREAK_MINS` at `:16`). Spans after the last closed batch are the tail; they are never derived until a batch closes.
- The daemon ticks every 15 s (`SESSIONIZE_EVERY`, `crates/app/src/main.rs:1926`): sessionize, pre-pass every `prepass_secs` (60, `main.rs:1541`), proposals (`main.rs:1548`), then `Scheduler::tick` (`main.rs:1718`).
- Derive gate (`derive_gates_open`, `main.rs:1887-1892`): AFK for `derive_idle_secs` (300) **or** 1-minute load under `LOW_LOAD` 1.0 (`main.rs:1676`). So derive runs while the user is active whenever the box is quiet. Battery under 30% and discharging defers.
- One worker slot. Derive and AI jobs are subprocesses of the daemon's own binary (`main.rs:1868-1883`), 300 s timeout (`DERIVE_TIMEOUT`, `main.rs:1674`). `infer_intervals` loads the model from disk on every call (`crates/derive/src/runner.rs:26-34`, mmap). The Scheduler comment at `main.rs:1686-1688` says the design assumes a single resident llama.cpp process; it is not resident. The log shows 248 "loading model tensors" lines for ~226 spawns.
- The derive worker (`main.rs:550`) builds the digest from batch spans, the 8 most recent open tasks (`main.rs:564`; declared first), pre-pass hints (`storage::prepass_hints`, `crates/core/src/storage.rs:1935`), 4 similar corrections (`storage.rs:2171`), activity events in the window (`storage.rs:165`), and MCP context gathered synchronously (a hung Jira MCP adds to derive wall time; "connect timed out" is in the log).
- Digest (`crates/core/src/digest.rs:36`): apps by time, sites by time, last 10 activity lines, a per-minute run-length timeline with a runner-up per minute, open tasks, hints, corrections, ejects, MCP context. Budget 1900 approx tokens (`digest.rs:18`) of a 3132-token prompt limit (`N_CTX` 4096 − `MAX_GEN` 900 − 64, `runner.rs:19-21`).
- Output is grammar-constrained JSON, up to 12 intervals (`grammars/task_output_v3.gbnf:8`), keys `ref,label,project,start_offset_min,end_offset_min,confidence`, greedy sampling.
- Post-processing: `sanitize_intervals` + `link_intervals` (`crates/core/src/merge.rs`) resolve refs and near-duplicate labels to task slots; `clamp_intervals` (`main.rs:1084`) clips to the window and splits on AFK ≥ 5 min, drops pieces under 1 min. Nothing coalesces adjacent same-task intervals and nothing repairs overlaps.
- Pre-pass (`crates/core/src/prepass.rs`): three rules in order over unassigned runs in the tail — branch names a ticket key and an open task has that `external_ref`; a repo active in the run matches an open task's project (`prepass.rs:75`, picks the most recently active such task, no recency limit); a past correction's FTS match names an open task. Hits are `source='prepass'` intervals at confidence 0.5 that derive later replaces.
- Feed (`storage::feed_blocks`, `storage.rs:2016`): one row per interval, newest 12 of the day, hover shows source and confidence, 5 s reload (`crates/app/src/ui/mod.rs:37`). Chips: "to confirm" for pre-pass rows. No surface shows the digest, the model output, the queue, the last derive time, or the model name after download.
- Bench (`chronicle bench`, `main.rs:396-546`) builds digests **without** hints, activity lines, or MCP context (`main.rs:464` vs the worker at `:564-580`), so it does not reproduce production prompts. Eval (`crates/core/src/eval.rs`) scores fixtures against hand-written `expect.json`; two fixtures carry expectations (`day3_sms`, `day4_heroku`). User corrections are never replayed.

## Findings (data, 2026-08-27 → 2026-09-03)

Speed, from the daemon log (68 completed derives):

| metric | value |
|---|---|
| derive wall time, median | 120 s |
| p90 / max | 200 s / 904 s |
| prompt tokens | 1492–3120 of 3132 |
| failures | 4 "prompt still too long" (batches 57/58, before the 09-02 budget fix), 3 worker timeouts |
| user-visible lag from starting new work to a model label | 35–65 min (batch close + idle gate + derive) |

Bench on this box (load was 4–9 from a parallel build, so absolute numbers are inflated; the log median above is the reliable figure):

| batch | 4B | 1.7B |
|---|---|---|
| 66 (61 min window, 469 digest tokens) | 7 intervals, 1 task, 103 s | 5 intervals, 1 task, 59 s |
| 67 (44 min, 802 tokens) | 12 intervals, 3 tasks, 205–231 s | 12 intervals, 1 task, 79–95 s |
| 68 (33 min, 1249 tokens) | 3 intervals, 3 tasks, 115 s | 12 intervals, 1 task, 72 s |

Accuracy, from the DB (361 intervals, 55 tasks with intervals, 77 corrections):

1. **Fragmentation.** 72% of intervals are under 5 min (median 2.0 min); 53% of tasks total under 10 min. Cause is visible in every bench run: both models emit one interval per Timeline line, copying the digest's run-length rows instead of merging same-goal lines. Batch 67 with 4B: eight consecutive one-minute intervals with the identical label. Output JSON costs about 45 tokens per interval at roughly 8 tok/s, so 12 intervals is about 60 s of the 120 s median. Fragmentation is both the top visible defect and half the latency.
2. **Declared-task magnet.** With one declared task per project, whole windows collapse onto it at 0.95–0.98 confidence regardless of evidence. Batch 66 in production: 49 min → "start dev on ACME-11382" while the Claude sessions in the window were chronicle direction talk; the same batch in bench (no hints) → all 61 min on "fix derivation/task accuracy in app" at 0.95. Same confidence, opposite answer. The pre-pass repo rule (`prepass.rs:75`) maps any run with chronicle git or Claude activity to the most recent open chronicle task, the prompt calls hints strong priors, and the model follows. 1.7B is worse: every bench window went 100% to one declared task, and on batch 68 its confidence decayed 0.90 → 0.00 across twelve one-minute pieces.
3. **Duplicates across batches.** `merge` is the most common correction (26 of 77). ACME-11381 work got five near-identical derived labels in one day. Batch-local derivation cannot see the day.
4. **Distractions become tasks.** Wordle, YouTube, F1 stats, a GitHub repo each minted a one-interval task with no project; several were later reassigned by hand.
5. **Corrections section poisons.** `assign` corrections are stored with `old_label = '(unassigned)'` (`storage.rs:1763`, `:2111`) and render in the digest as renames `"(unassigned)" → "✳ Complete m25"`, glyphs included. One retrieved rename ("exploring app monetization strategy" → "investigating automatic time tracking… [memtime.com]") directly mislabels a real monetization session.
6. **Overlap.** Batch 68 with 4B produced 0–28 and 13–15 as separate intervals; nothing rejects or repairs overlap.
7. **No instrumentation.** `batches` has no derived timestamp or duration, `intervals` no created timestamp, `ai_jobs` no duration. Latency and pre-pass hit rate cannot be measured from data (only one `prepass` row survives at any time). `checkpoint` jobs fail 48% of the time (11 of 23), mostly "batch N holds no intervals for task M" races after re-derivation.

## Design decisions

### 1. Three tiers, one model process

| tier | when | window | output | purpose |
|---|---|---|---|---|
| live | every `live_secs` (300) while active and the tail has new focus | last ~15 min | one interval: current task or new label | feed reacts in minutes; "the brain deriving right away" |
| batch | batch close, existing gate | 10–30 min, closed at natural boundaries | 1–8 intervals | authoritative labels for the window |
| day | first long AFK after 14:00, or 18:00, or on demand | the day's derived tasks | merges + renames | fix cross-batch duplicates, fold orphans |

All three run in one resident worker process with the instruction prefix cached in the KV cache. Batch and day tiers keep the existing idle/load/battery gates; the live tier adds its own CPU budget gate.

### 2. Shorter output, fewer intervals, no overlaps

The model output shape is the lever on both accuracy and speed. Compact keys, a grammar cap of 8, a prompt example that shows merged lines, and a deterministic coalesce + overlap repair after linking. Time intervals still split on AFK ≥ 5 min (the M5 time-honesty rule stands); they merge only across gaps with no such AFK.

### 3. Rules carry strength, and recency gates the repo rule

A branch or ticket-key match is strong evidence. "Some file in this repo was touched and there is an open task with that project" is weak, and today it is the magnet. Hints in the digest say which they are; the repo rule only fires for a task active in the last two hours or declared today.

### 4. Measure on real corrections

Every correction the user makes is a labelled example of a batch the pipeline got wrong. A replay eval re-derives those batches with the production digest builder and scores whether the corrected outcome comes out. Prompt and rule changes get a number before they ship; the two hand fixtures stay as regression tests.

### 5. Show the work

Stream the worker's tokens into a "deriving…" row on the feed, and give Settings › Derivation an inspector: model, queue, last derive, the tail digest as the model will see it, the last raw output, pre-pass placements with reasons.

## Chunk 1 — Interval hygiene (fragmentation, overlap, output shape)

Smallest change with the largest visible effect. Ships alone.

- `crates/core/src/merge.rs`: new `coalesce(linked: Vec<LinkedInterval>, afk_gaps: &[(i64, i64)]) -> Vec<LinkedInterval>` in digest-minute units. Sort by start; merge an interval into the previous one when both share a slot and no AFK gap ≥ 5 min lies between them (a gap of unlabelled minutes under 2 min does not block the merge). Confidence = duration-weighted mean. Overlap repair runs first: when two intervals overlap, the later start moves to the earlier end; an interval fully inside another is dropped, its slot's confidence unchanged. `clamp_intervals` (`main.rs:1084`) already computes the AFK gap list; pass it through and call `coalesce` between `link_intervals` and clamp.
- `grammars/task_output_v4.gbnf`: keys `r,l,p,s,e,c`; cap `{0,7}` (8 intervals); `int` stays `[0-9]{1,4}`. `IntervalDraft` (`crates/core/src/types.rs:192`) gains the short serde names; the v3 names stay accepted for the fixtures via `alias`.
- `prompts/derive_v4.txt`: same rules as v3 plus one worked example that shows six Timeline lines becoming one interval, the sentence "consecutive Timeline lines serving one goal are ONE interval; an interval under 3 minutes needs a goal change on both sides", and the key legend. Drop the `/no_think` line (the 2507 Instruct preset has no thinking mode; harmless but dead).
- `MAX_GEN` 900 → 600 (`runner.rs:21`); the freed 300 tokens go to the digest budget (`digest::MAX_TOKENS` 1900 → 2200) so titles clip less.
- Hidden one-off `chronicle backfill-coalesce --since <date>`: applies the same coalesce to stored `derived` intervals per batch (same task, contiguous, no AFK span ≥ 5 min between). Run once after deploy on the live DB, after a `.bak` copy.
- Tests: `merge` unit tests (adjacent merge, AFK blocks merge, overlap trims, nested drop); `eval.rs` gains `max_intervals` next to `max_tasks` and `day3_sms.expect.json` sets it; goldens re-blessed for the prompt version only where the digest text changed (it should not).
- Acceptance: `chronicle bench --batch 67 --model 4b` yields ≤ 4 intervals over the same 3 tasks; after a day of soak, median interval length in the DB is over 5 min and the feed shows no adjacent same-task rows.

## Chunk 2 — Instrumentation and the corrections replay eval

Do this before any prompt or rule change, so the change has a number.

- Migration `014_derive_metrics.sql`: `batches.derived_ts`, `batches.derive_ms`, `batches.prompt_tokens`, `batches.gen_tokens`; `intervals.created_ts` (default now); `ai_jobs.started_ts`, `ai_jobs.finished_ts`. The worker writes them in `store_derivation` (`storage.rs:1411`) and the AI-job completion path. `runner.rs` returns `(intervals, prompt_tokens, gen_tokens, prompt_eval_ms, gen_ms)` instead of bare intervals; the log line at `runner.rs:43-48` grows the same fields.
- `chronicle status` and `status --json` gain: model file, last derive (batch id, ended at, duration, tokens), pending batch count, worker state.
- Bench parity: extract `build_batch_digest(conn, config, batch) -> (digest, open_tasks)` from `derive_worker` and use it from `bench` (`main.rs:464`), so bench prompts equal production prompts. Add `--no-mcp` for offline runs.
- `chronicle bench --replay [--since DAYS] [--model NAME]`: for every batch whose window overlaps a `rename`, `reassign`, `merge`, `eject`, or `assign` correction, rebuild the digest with the open-task list as it stood at the batch's end (tasks created before `end_ts`, filtered to those with an interval before then; declared tasks by `created_ts`), derive, link, and score three checks per correction: `placed` (the corrected minute range resolves to the task the user ended on, following `merge` chains), `label` (for renames: the new label's distinctive tokens appear in the model label), `not_ejected` (an ejected range does not land on the ejected task). Print per-batch PASS/FAIL and an aggregate, write JSON to `--out` for diffing. Record the first baseline in this doc under Shipped.
- Fix the checkpoint race while here: `ai_job_worker` (`main.rs:658`) re-reads the task's current intervals instead of the batch's, and a task with no intervals in the window marks the job `skipped`, not `failed`.
- Tests: replay scoring unit tests on a synthetic corrections set; migration test on a copy of the live DB (the m22 pattern).
- Acceptance: `chronicle bench --replay --since 7` runs over the 77 corrections and prints a score; `chronicle status` shows the last derive's duration.

## Chunk 3 — Resident derive worker with a cached prefix

- New hidden subcommand `chronicle derive-worker`, modelled on `chat_worker` (`main.rs:209`) and `chatproto` (`main.rs:175`): JSON lines over stdio, `Ready` on load, then requests `{t:"derive", batch_id}`, `{t:"live", ...}` (chunk 4), `{t:"consolidate", day}` (chunk 6); replies `Tok{text}` while generating, `Done{...}` or `Err{...}`. `crates/derive/src/runner.rs` becomes a `DeriveModel` struct that owns backend, model, and one context, like `ChatModel` (`crates/derive/src/chat.rs:25`).
- Prefix cache: evaluate the rendered prompt up to the `{digest}` marker once per process; before each request, remove everything after the prefix from sequence 0 (`LlamaContext::clear_kv_cache_seq(Some(0), Some(prefix_len), None)`; `kv_cache_seq_rm` is the same call with a raw sequence id — both exist in the pinned llama-cpp-2 0.1.154) and decode only the digest and template tail. Keep the logits of the last prefix token out of the batch; the first digest token's decode produces the first usable logits. The Qwen chat template puts the user turn after the system tokens, so the static prefix is the instruction text; the digest is the only variable part.
- Scheduler (`main.rs:1685-1813`): `worker` becomes an enum of `Idle | Loading | Busy(request)`; the process stays up, is killed and respawned on `DERIVE_TIMEOUT` or a broken pipe, and exits itself after 20 min idle (`worker_idle_secs`, default 1200) so a laptop asleep does not hold a context. The model stays in page cache either way. AI jobs keep their own subprocess for now (they use `Describer` and `ChatModel`); moving them onto the resident worker is a follow-up.
- Battery and load gates unchanged. Threads unchanged (physical − 1, capped 8).
- Tests: worker protocol round-trip with a stub model behind a feature flag; scheduler state machine tests for timeout and idle exit.
- Acceptance: in the log, `cached_prefix_tokens` ≈ 1078 on every request after the first; derive wall time on a 2500-token prompt drops from the 120 s median to under 70 s with the chunk 1 output cap (measure over a day: `batches.derive_ms`).

## Chunk 4 — Timing: natural batch boundaries, live tier, streaming row

Batch boundaries (`sessionizer::assign_batches`, `sessionizer.rs:231`):

- A batch also closes when at least `batch_min_minutes` (10) of non-AFK time has accumulated and an AFK gap ≥ 5 min begins. The 30-min active cap and the 30-min AFK break stay. Windows now end at breaks instead of cutting through a stretch, so the model labels more often than it segments. Config: `batch_min_minutes` (10). Goldens for `day1` batches re-blessed; the change is deliberate and reviewed in the diff.

Live tier:

- Config `live_secs` (300; 0 = off). On the pre-pass timer, when not AFK, the tail has at least 5 min of focus newer than the last live pass, the load gate is open, and battery is above 30%: build a live digest with the shared renderer over `[max(last boundary, now − 15 min), now)` where a boundary is the last derived or live interval end or an AFK ≥ 5 min; sections: timeline, activity lines, the 16 open tasks, the previous interval as "Previously: <label>", any pre-pass hint over the window with strength. `prompts/live_v1.txt` asks for exactly one object `{r,l,p,c}` naming the task of the current stretch; `grammars/live_output_v1.gbnf`. Store as `source='live'`, `batch_id NULL`, `reason='live'`, confidence = model × 0.8. `clear_prepass` (`prepass.rs:49`) generalizes to clear both `prepass` and `live` rows in the tail on each pass; the batch derive replaces them as it does pre-pass rows, and user `keep` promotes them the same way.
- Budget: about 1200 prompt tokens with the prefix cached and about 40 output tokens; expected 15–25 s every 5 min, under 10% of five threads. The gate skips a pass when the previous live pass took over 60 s (self-throttle, logged).
- Feed: `live` rows get a "live" chip beside "to confirm"; hover detail says "live pass, not confirmed by the batch yet".

Streaming row:

- The daemon accumulates `Tok` text from the worker into meta `derive_progress` = `{kind: batch|live|day, id, started_ts, partial}` and clears it on `Done`/`Err`. Home's feed section (`crates/app/src/ui/home.rs:852`) shows, above the newest block, "deriving 09:18–09:51 · <label being typed>…" with the existing spinner; the label is the `l` field parsed from the partial JSON, or "linking to <open task>" when `r` is set. The 5 s UI reload already carries it.
- Acceptance: after starting a new stretch, the feed shows a model row within `live_secs` + one pass; daemon CPU over a working hour under 10% (`systemd-cgtop` on the user unit); the "deriving…" row appears within a second of the worker starting and never outlives the job.

## Chunk 5 — Evidence rules and prompt hygiene

- Ticket keys in titles and URLs. New pre-pass rule between branch and repo: over a run, count focus minutes whose title or URL matches `ticket_regex`; when one key holds at least 2 min and at least half of the ticketed minutes and an open task has that `external_ref`, place with reason `title ACME-…` (strong). Digest gains a `## Keys seen` line under Activity: `ACME-11382 9m (Jira), ACME-11374 1m`. Batch 68 has 9 min of a Jira page titled with the key and no rule used it.
- Terminal cwd from titles. Regex over focus titles for `~/dev/<name>`, `<user>@<host>:<path>`, and `<name> — <editor>` forms → repo basename, treated like `repos_active_in` for the repo rule and rendered in the digest as `cwd chronicle 12m`. m26 chunk 4 (atuin) will supply real cwd; this is the title-only fallback and stays.
- Repo rule recency gate: candidate task must have an interval ending in the last 2 h or be declared today; else no hint. Hints render with strength: `- 0–12m → 2 (strong: branch ACME-11382)` / `(weak: repo chronicle)`, and the prompt says weak hints are tie-breakers only.
- Distractions. Prompt rule: "brief chat, social, or video stretches under 3 min inside a longer goal belong to that goal's interval; do not create a task for them". Deterministic: focus spans matching `distraction_patterns` (`insights.rs:131` already compiles them) never seed a pre-pass run or a proposal cluster.
- Corrections hygiene in `similar_corrections` rendering (`digest.rs:584-599`): `assign` rows render like ejects, `"<first ctx line>" → "<task>"`, never as a rename of "(unassigned)"; labels pass through a core `strip_glyphs` (move the glyph table out of `theme::display_title` so digest and UI share it); a rename whose new label is a declared task's label is preferred over free-text renames when both match.
- Open-task cap in the worker 8 → 16 (`main.rs:564`), paid for by chunk 1's shorter output.
- Tests: pre-pass unit tests for the key rule, the cwd regex, and the recency gate; digest golden for the new sections; replay eval before/after.
- Acceptance: replay score improves on the chunk 2 baseline; on batch 68's window the pre-pass places ACME-11382 by title before the model runs; batch 66's window no longer lands on a mailer task.

## Chunk 6 — Day-tier consolidation

- Trigger: once per local day (meta `consolidated:<date>`), at the first AFK ≥ `checkpoint_afk_secs` after 14:00, at 18:00 if none, or from the Home `…` menu ("tidy today"). Runs on the resident worker under the batch gates.
- Deterministic first: derived tasks with total under 3 min, no project, no ticket, no journal or checkpoint, merge into the task that surrounds them in time (both neighbours the same task) — no model. This alone removes most of the "Wordle became a task" rows.
- Then the model: input is the day's derived tasks (declared and user-created tasks are listed for reference but locked) with id, label, project, `external_ref`, total minutes, interval count, three top evidence lines. `prompts/consolidate_v1.txt` returns `{merges: [{into, from: [ids]}], renames: [{id, label}]}` under `grammars/consolidate_v1.gbnf`. Guards in code: never merge across two different non-null projects or two different ticket keys; never merge into or out of a locked task; at most 6 merges per run; renames only for tasks with one interval batch (fresh labels), never for tasks the user renamed.
- Applied via `merge_task` (`storage.rs:2122`) and a new correction kind `consolidate` with the before state in `ctx`, so "undo tidy" on the Home card reverts the day's run in one transaction.
- Model: 4B by default. A `qwen3-8b` preset (`Qwen3-8B-Q4_K_M`, about 5 GB) is added to `PRESETS` and an optional `model_path_heavy` config selects it for this tier only; measure before making it default.
- Tests: guard tests on synthetic task sets; replay eval gains `merge` corrections as cases.
- Acceptance: `merge` corrections per day drop over a week of soak; no user-created task changes in any run.

## Chunk 7 — Inspector

- Settings › Derivation gains a "Pipeline" card: model file and preset, worker state, pending batches, last derive (batch, ended at, duration, prompt/gen tokens), last live pass, pre-pass placements in the tail with reasons. Two buttons: "show digest" opens the current tail digest as the worker would build it (read-only scroll area, monospace), "last output" shows meta `derive_last_output` (raw JSON) beside the linked result. Both are debugging surfaces and say so.
- Feed rows with confidence under 0.7 render the timeline's amber/orange confidence dot (`theme.rs:444-470`) instead of hiding it in hover text.
- `chronicle status --json` carries the same data (chunk 2) so a terminal can watch it.
- Acceptance: the card answers "what is it doing right now and what did it last see" without opening a log.

## Experiments (no chunk, run when idle)

- Vulkan on the iGPU: `cargo build --release --features vulkan -p chronicle` (`crates/app/Cargo.toml:33`; needs the Vulkan SDK). Bench batch 67 with and without; expect prompt eval to improve, generation not. Adopt only if the packaged build can carry it.
- 8B for the day tier only, after chunk 6 measures 4B.
- Speculative decoding with 1.7B as draft is not exposed by llama-cpp-2; revisit when it is.

## Build order

1. Chunk 1 — hygiene. One day. Fixes the most visible defect and cuts derive time by about half on its own.
2. Chunk 2 — instrumentation + replay eval + bench parity. Needed before any prompt or rule change is judged.
3. Chunk 3 — resident worker with cached prefix.
4. Chunk 4 — batch boundaries, live tier, streaming row. Depends on 3 for the CPU budget.
5. Chunk 5 — rules and prompt hygiene, scored with 2.
6. Chunk 6 — day tier.
7. Chunk 7 — inspector (parts can ride along with 2 and 4).

Each chunk is its own deploy: stop the unit, `cargo install --locked`, restart, soak a day, record numbers under Shipped below.

## Out of scope

- Sync, teams, publishing outward (m28 now).
- New collectors (m26 covers calendar, editor heartbeats, shell history).
- Moving journal/checkpoint/standup/describe jobs onto the resident worker (follow-up after chunk 3).
- Changing the sessionizer's span rules or title similarity.

## Open for James (not blocking)

- Live tier cadence: 5 min default, or tie it to the pre-pass timer (60 s) with the CPU self-throttle deciding? Recommendation: 5 min; the pre-pass already covers the first minute with rules.
- Day-tier trigger time: 14:00 first-long-AFK plus 18:00 fallback, or only on demand for the first week?
- Whether "tidy today" may rename model-labelled tasks at all, or only merge. Recommendation: merge plus rename of single-batch tasks only, as written.
- 8B: buy the RAM cost for the day tier or stay on 4B everywhere until the replay eval says otherwise.

## Verification commands

```sh
# digests the worker would build (after chunk 2 parity; today omits hints/activity/MCP)
chronicle bench --fixtures /nonexistent --batch 67 --batch 68 --digest
# model comparison on live batches
chronicle bench --fixtures /nonexistent --batch 67 --model qwen3-4b
chronicle bench --fixtures /nonexistent --batch 67 --model qwen3-1.7b
# fixture regressions
chronicle bench --only day3_sms
# corrections replay (chunk 2)
chronicle bench --replay --since 7 --out /tmp/replay.json
```

Read the live DB read-only only: `python3 -c 'import sqlite3; sqlite3.connect("file:~/.local/share/chronicle/chronicle.db?mode=ro", uri=True)'` (no `sqlite3` CLI on this box), or copy db + wal + shm to the scratchpad first.

## Shipped

### Chunk 1 — interval hygiene (2026-09-03, a899606 + backfill fix)

What landed: `merge::coalesce` (overlap repair, then adjacent same-slot join across unlabelled gaps under 2 min with no AFK ≥ 5 min or user row between; unit-agnostic so the backfill reuses it in ms), called between `link_intervals` and `clamp_intervals` in the worker and before `resolve` in bench; `task_output_v4.gbnf` + `derive_v4.txt` (cap 8, worked merged-lines example, "an interval under 3 minutes needs a goal change on both sides"); `MAX_GEN` 600, `digest::MAX_TOKENS` 2200; hidden `chronicle backfill-coalesce --since YYYY-MM-DD [--dry-run]`; `max_intervals` eval check (day3_sms cap 4). Overlap trimming already existed in `clamp_intervals`; the finding above overstated that.

Two deviations from the plan text, both from bench evidence:

- Keys are `ref,label,project,start,end,confidence`, not `r,l,p,s,e,c`. With one-letter keys 4B reads `r` as an ordinal (second interval → `r: 2` → whichever open task is listed second): day3_sms fell from 8/10 to 5/9 with the 19–26 SMS range linked to the chronicle task. With the words restored it is back to 8/10, identical failures to v3. Only the two offset keys shortened (about 8 tokens per interval).
- `/no_think` stays as line 1: the 1.7B preset is the thinking-mode Qwen3, and the line costs three tokens.
- The worked example uses a neutral shop repo, not chronicle evidence, so the example cannot bleed into labels.

Numbers (4B, this box, load 10–14 from a concurrent release bench in another session, so wall times are inflated ~2× against the 120 s production median):

| case | v3 (same binary, prompt/grammar swapped) | v4 |
|---|---|---|
| day3_sms | 2 intervals, 8/10 | 2 intervals (0–26 SMS, 26–27 chronicle), 8/10 |
| day4_heroku | 3 intervals, 5/6 | 2 intervals, 5/6 |
| batch 67 | 12 intervals / 3 tasks (plan bench) | 3 intervals / 3 tasks, 224 s under load |
| batch 68 | 3 intervals / 3 tasks (plan bench) | 2 intervals / 2 tasks, 141 s under load |

Backfill on the live DB (`.bak-m27c1` taken first, 2026-08-20 onward): 35 batches changed, derived rows 370 → 186 (13 pinned by `corrections.interval_id` stay put), median interval 2.0 → 6.0 min, share under 5 min 71% → 41%. The one overlap left (batch 64, a user row over a derived one) predates the backfill.

Still open after chunk 1, on purpose: labels on batches 66–68 remain wrong the way findings 2 and 5 describe (declared-task magnet, poisoned corrections) — that is chunk 5, scored by chunk 2's replay. Acceptance check pending a day of soak: feed shows no adjacent same-task rows; `batches.derive_ms` does not exist until chunk 2, so the speed claim is bench-only for now.

### Chunk 2 — instrumentation, replay eval, bench parity (2026-09-03, 81f1853)

What landed: migration 014 (`batches.derived_ts/derive_ms/prompt_tokens/gen_tokens`, `intervals.created_ts` stamped by an `AFTER INSERT` trigger — `ADD COLUMN` cannot default to an expression, old rows stay NULL — and `ai_jobs.started_ts/finished_ts`); `infer_intervals` returns a `DeriveRun` with token counts and prompt/gen wall times; `build_batch_digest` extracted from the worker and shared with `bench --batch` (so bench prompts now carry the Activity lines and MCP context the plan bench omitted; `--no-mcp` for offline runs); `chronicle status` shows the model file and preset, the last derive (batch, time, duration, tokens), pending batches, and what the worker slot is running; `chronicle bench --replay [--since DAYS] [--model] [--out]` with the scoring in `core::replay`; journal jobs whose batch was re-derived under other tasks end as `skipped` (the 48% "checkpoint" failure in the findings was this journal race).

Deviations from the plan text:

- Replay probes come from what the corrections table holds. `merge`, `rename`, and `eject` rows carry no `interval_id`, so a merge/rename probe is the target task's current intervals per batch that ended before the correction (one probe per batch, range clipped to the batch window), and an eject probe borrows the range of the `assign` the user made within 120 s after it. The open-task list at a batch's end is rebuilt from `tasks.created_ts`/`closed_ts` with labels rewound through later renames, so a corrected label cannot leak into the prompt. Replay never gathers MCP context.
- `placed` passes on a label-token match when the target task did not exist at the batch's end (declared later), reported as "by label".

Numbers: over the last 7 days, 180 probes from 45 corrections over 50 done batches (1 correction skipped). 4B baseline (pre-chunk-5 rules, prompt v4, run 2026-09-03 under load): `total: 76/180` — `placed 75/178`, `not_ejected 1/2`; the dominant failure is the declared-task magnet (c78 merge probes landing on the neighbouring declared task).

### Chunk 3 — resident derive worker (2026-09-03, ed3f387)

What landed: `DeriveModel` (backend + mmap'd model) and `DeriveSession` (one context whose KV cache keeps the last prompt and answer; each request decodes only the tokens after the longest common prefix with what is cached — the instruction prefix every time, the whole digest too on a retry); `chronicle derive-worker` speaks JSON lines on stdio (`ready`, `progress`, `done`, `err`), exits after `worker_idle_secs` (1200) without a request or after each request when 0; the daemon's `Scheduler` holds the resident beside the one-shot AI-job slot, restarts the 300 s clock at `ready` so model load is not charged to the request, kills on timeout, reaps the idle exit with a 60 s grace; bench and replay hold one session per model so their cases share the cache.

Deviations: the idle exit is the worker's own `recv_timeout`, with the daemon's kill as a backstop, as written. No stub-model feature flag: the protocol has a serde round-trip test and the timeout/idle rule is a pure function with tests; the end-to-end check was a manual `derive-worker` run against a DB copy (ready → 196 progress pieces → done → idle exit after the configured 5 s).

Numbers (release build, this box loaded by the replay baseline and a concurrent build, so absolute times are 2–4× production): bench batch 67 then 68 through one session — 67: 2126 prompt tokens, 0 cached; 68: 2733 prompt tokens, 1276 cached (the rendered instruction prefix), same intervals as the one-shot runs. First two production derives after the deploy (14:00, load 8–10 from the replay baseline running alongside): batch 72 — 3237 prompt tokens, 0 cached, prompt eval 151 s, 179 gen tokens in 49 s; batch 73 through the same resident worker — 2889 prompt tokens, 1382 cached, prompt eval 76 s, 198 gen tokens in 60 s, 139.9 s wall (`batches.derive_ms`). The cache halves prompt eval; generation is the rest, and the day of soak on a quiet box is still to come.

### Chunk 4 — batch boundaries, live tier, streaming row (2026-09-03)

What landed: `assign_batches` also closes at an AFK gap ≥ 5 min once the batch holds `batch_min_minutes` (10) — day1's fixture now closes at the 17:28 gap instead of the 17:40 cap, goldens re-blessed; live tier every `live_secs` (300) on the pre-pass timer while active, slot free, 1-min load under 1.0, battery fine, and at least 5 min of focus since the last boundary (last interval end of any source, or the last AFK ≥ 5 min, at most 15 min back): `prompts/live_v1.txt` + `grammars/live_output_v1.gbnf` return one `{ref,label,project,confidence}`, linked with the batch linker, stored `source='live'` at confidence × 0.8 over the tail with pre-pass rows in the window replaced; the batch derive replaces live rows like pre-pass rows and `keep` promotes them; the resident worker keeps a second 2048-token session for the live prompt so the two prefixes do not evict each other; a live pass slower than 60 s skips the next one. Streaming: the worker sends `progress` with a human label already resolved ("<label being typed>…" or "linking to <open task>"), the daemon mirrors it into meta `derive_progress` from a `select!` branch on the worker's reply channel, Home shows "deriving HH:MM–HH:MM · <label>" with a spinner above the newest block and a "live" chip on live rows.

Deviations: `progress` carries the resolved label instead of raw tokens (the worker owns the open-task list the ref indexes); the feed row refreshes on the UI's 5 s reload rather than per token.

### Chunk 5 — evidence rules and prompt hygiene (2026-09-03, 799af4d)

What landed: `core::evidence` (ticket keys in titles/URLs with screen time per key, `~/dev/<repo>` cwd from titles, distraction matching, the status-glyph strip moved out of the UI); pre-pass rule order is now branch → title key (≥ 2 min and ≥ half the keyed minutes, reason `title <KEY>`) → repo (repos from git activity plus cwd titles; a candidate task needs an interval ending within 2 h of the run or to be declared today) → past correction; distraction runs never seed a placement or a proposal; the digest gains `## Keys seen` (keys with minutes and app, `cwd <repo> <min>`) and renders hints as `(strong: branch …)` / `(weak: repo …)`; `assign` corrections render as `"<work>" → "<task>"` with glyphs stripped (rows without context are skipped); renames onto a declared task's label rank first among similar corrections; the batch prompt is `derive_v5.txt` (hint strength, keys, the under-3-minute distraction rule); open-task cap 8 → 16.

Deviations: hint strength is rendered from the stored reason (`branch`/`title` strong, else weak) rather than stored, so existing rows and the UI's "placed by a rule · repo chronicle" text are unchanged.

Numbers: fixtures unchanged under v5 — day3_sms 8/10, day4_heroku 5/6, same two failures as v3/v4. Replay: 4B baseline before chunk 5 = 76/180 (placed 75/178). Post-chunk-5 run (prompt v5, title-key + gated repo rules, same 50 batches, run 2026-09-03 under load): **52/171** (placed 51/169, not_ejected 1/2) — a regression, not the improvement the acceptance line asked for. Per-correction diff: 16 corrections worse, 5 better, 2 probes (c33 reassign, c34 merge) no longer generated. 36 of the 119 failing probes land on "ACME-10787 re-work" where the user ended on "user experience review and tweaks": the new title-key rule (strong, fires from a Jira page title) is the new magnet, doing to a ticket what the repo rule did to a declared task. Next: gate the title rule on the key's minutes being the majority of the run (not just ≥ 2 min and ≥ half the *keyed* minutes), and re-run the replay before any prompt change. Runs: `replay-baseline.json` / `replay-after.json` in the m27 session scratchpad.

### Chunk 6 — day-tier consolidation (2026-09-03)

What landed: `core::consolidate` (orphan folds: a derived task under 3 min with no project, ticket, or notes whose every interval sits between two intervals of one other task; `guard`: locked tasks never merged or renamed, no merge across two projects or two ticket keys, no task folded twice, at most 6 merges, renames only for single-batch tasks the user never renamed); `prompts/consolidate_v1.txt` + `grammars/consolidate_v1.gbnf` over the day's derived tasks with totals and top evidence lines, locked tasks listed for reference; applied in one transaction as a single `consolidate` correction whose `ctx` is the before-state (folded task rows, their interval ids, old labels), so Home's "undo tidy" reverses the run; the stamp `consolidated:<date>` (0 = ran, nothing to do) keeps it to one run a day; trigger: first AFK ≥ `checkpoint_afk_secs` after 14:00, 18:00 fallback, or Home's "tidy" button (ctrl command `consolidate`), all through the resident worker under the batch gates; `qwen3-8b` preset and `model_path_heavy` (loaded per run, dropped after) for this tier only — not downloaded, not default.

Deviations: model merges and renames write no `merge`/`rename` correction rows (those mean the user spoke and feed the replay eval and the digest's examples); the one `consolidate` row is excluded from both. Renames go through a direct label update for the same reason.

### Chunk 7 — inspector (2026-09-03)

What landed: Settings › Derivation gains a "Pipeline" card — model file and preset, worker state from the daemon socket (busy job and seconds, or "idle, model resident"), pending batches, last derive (batch, time, duration, tokens), last live pass, pre-pass placements in the tail with their rules — refreshed on the UI's 5 s reload while Settings is open; "show digest" renders the tail digest exactly as the worker would build it now (no MCP fetch, so the UI never blocks on Jira), "last output" shows the raw JSON of the last batch derive; both are read-only monospace views that say they are debugging surfaces. Feed rows placed by the model or the live tier at confidence under 0.7 wear the timeline's amber/orange dot instead of the task colour. `chronicle status --json` already carries the same data since chunk 2.

Not visually verified in this session (the daemon's UI was closed by the deploys); the card follows the existing Settings grid idiom and compiles clippy-clean.
