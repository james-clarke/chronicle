# M31 — Cloud models: BYOK, sign-in, and a hosted tier

Status: **chunks 0 + 1 shipped 2026-09-04** (branch `m31`; see "Shipped" at the end). Originally research + plan (2026-09-04). Runs after m30 completes;
m30 chunk 2 (scorer gate) is in progress in another session and this plan
does not touch its files. Four sub-agents (codebase seams, live DB volumes,
Rust client crates + provider APIs, prior art + hosted-tier economics)
plus the Claude API reference. Every "today" claim carries a `file:line`;
every number is from the live DB (2026-08-21 → 09-04) or a cited source.
James's ask: frontier cloud models as a user choice (paste a key, or sign
in), nothing configured by default, cloud used only where it buys accuracy
or speed, and a cheap hosted tier later. James to edit and pick.

## The short version

The local 4B model is not the accuracy bottleneck (m30's thesis holds:
evidence shape and task identity are), but it is the **latency** and
**prompt budget** bottleneck, and it is the wrong tool for three jobs:
free-text writing (journal, narrative, standup — it invents), chat (it
truncates context to ~1900 digest chars and answers slowly), and naming
new clusters well. m30 shrinks the model's job to exactly those. This
milestone lets that small slot be filled by a cloud model when the user
chooses, and keeps everything else local.

1. **One backend trait, three implementations.** `Local` (llama.cpp, as
   today), `Anthropic` (Messages API), `OpenAiCompatible` (OpenRouter,
   Ollama, Gemini's compat endpoint, anything with a base URL). A fourth,
   `ChronicleCloud`, is the same Anthropic/OpenAI wire shape pointed at
   our gateway with a session token instead of a key.
2. **Routing is per job, not global.** A small table says which backend
   each job kind uses. Defaults when a cloud backend is configured: chat,
   narrative, standup, journal, description, naming → cloud; scorer and
   segmenter never call a model; batch reconciliation stays local unless
   the replay says otherwise. The user can flip any row.
3. **Nothing leaves by default.** Fresh install offers three doors —
   download the local model, paste a key, sign in — and the app works
   (capture, timeline, reports) before any is chosen. The Storage
   "what leaves this machine" line becomes exact and per job, and a
   redaction pass runs on every cloud-bound prompt.
4. **Hosted tier last.** Sign-in is loopback OAuth (the `gcal-login`
   precedent), the gateway is a metered proxy in front of the same
   provider APIs, billing is prepaid credit packs. BYOK must work
   without it.

## What the data says (live DB, 14 days)

- Job volume on a heavy day (2026-09-03): 19 derive batches, 33 journal,
  19 task_description, 17 checkpoint, 4 suggest_task, 4 fetch_context,
  1 standup — about 80 model calls. Typical day is half that.
- Derive prompts are 2,969 tokens mean, ~3,300 p90, 147 generated tokens
  mean, 100 s wall per batch (`batches.prompt_tokens/gen_tokens`, 11
  batches with metrics). Other job kinds carry no token metrics yet.
- Chat: 3 messages in 14 days. Chat is unused because it is slow and
  shallow, not because it is unwanted — this is the job a cloud model
  changes most visibly.
- Failure rates: checkpoint 52 % success, task_description 68 %, journal
  90 % (`ai_jobs` status). Free-text jobs fail on grammar/length limits
  the 4B hits; a cloud model removes that class.
- Cost projection for a heavy day, assuming ~2k prompt tokens per
  non-derive job (unmeasured, conservative): ~220k input + ~15k output
  tokens. At Anthropic list prices (input/output per MTok): Haiku 4.5
  $1/$5 → ~$0.30/day; Sonnet 5 $2/$10 → ~$0.60/day; Opus 5 $5/$25 →
  ~$1.50/day. Prompt caching (512-token minimum prefix on Opus 5, 1024
  on Sonnet 5; the instruction block + open-task list is a stable
  prefix) cuts input by roughly half. Post-m30 the derive tier calls the
  model only for new clusters, so the real number is lower. **A hosted
  tier at $5–8/month covers a heavy Sonnet-class user with margin.**

## What the code does today

- Three model entry points, all synchronous, all take a prompt string and
  a token callback:
  `DeriveModel::load` / `DeriveSession::infer` / `infer_live` /
  `infer_consolidate` (`crates/derive/src/runner.rs:125-211`), the
  resident worker holding one context with a KV prefix cache
  (`runner.rs:155-164`, daemon side `crates/app/src/daemon.rs:576-643`);
  `Describer::describe_task` / `journal_entry` / `narrative` / `standup` /
  `checkpoint` / `suggest_task` (`crates/derive/src/describe.rs:33-146`),
  one-shot, run in an `ai_job` subprocess per job
  (`crates/app/src/ai_job.rs:96-275`); `ChatModel::answer` /
  `ChatSession::answer(history, context, question, on_token)`
  (`crates/derive/src/chat.rs:61-92`), warm subprocess
  (`crates/app/src/chat_worker.rs:171-194`).
- JSON shape is enforced by GBNF grammars compiled into the sampler
  (`runner.rs:26-31`: `task_output_v4.gbnf`, `live_output_v1.gbnf`,
  `consolidate_v1.gbnf`). Free-text jobs have no grammar.
- Prompt budgets are model-context constants: `N_CTX=4096`,
  `LIVE_N_CTX=3072` (`runner.rs:36-37`), chat `N_CTX=4096`, `MAX_GEN=1024`,
  three history turns at 800 chars (`chat.rs:23-27`), describer
  `MAX_GEN=256` (`describe.rs:29-31`). `fit_prompt`
  (`crates/derive/src/lib.rs:22-46`) truncates the digest to fit. These
  budgets are the reason chat is shallow; they must become a backend
  property.
- Templates: `derive_v5.txt`, `live_v1.txt`, `consolidate_v1.txt`,
  `task_description_v1.txt`, `journal_v1.txt`, `checkpoint_v1.txt`,
  `narrative_v1.txt`, `standup_v1.txt`, `suggest_task_v1.txt`,
  `chat_v1.txt` (paths per `ai_job.rs` / `derive.rs` call sites above).
  They are written for a 4B and carry rules ("quote the totals table
  verbatim", "if no checkpoint is shown write nothing") that a frontier
  model follows without prodding; they stay as-is for chunk 1 and get a
  cloud variant only where a replay or a read shows a difference.
- Config: `Config.model_path` / `model_path_heavy`
  (`crates/core/src/config.rs:97-101`), preset table and `resolve`
  (`crates/derive/src/model.rs:20-60`), Settings › Model panel = path
  field + download button (`crates/app/src/ui/settings.rs:482-508`),
  onboarding = model download + service install
  (`crates/app/src/ui/onboarding.rs`).
- Queue: `ai_jobs` with kind, priority, payload; `AI_JOB_INTERACTIVE=5`
  (`crates/core/src/storage.rs:663-757`); scheduler picks interactive →
  batch → background and spawns a subprocess per job
  (`daemon.rs:799-835`). A cloud job needs no subprocess and no idle
  gate; the queue stays, the executor changes.
- Precedents: `google.toml` written by temp file + rename at mode 0600
  (`crates/capture/src/gcal.rs:45-86`); `chronicle gcal-login` loopback
  OAuth on a random port (`crates/app/src/capture.rs:202-278`);
  `config::expand_home`; the Storage egress line
  (`settings.rs:614`: model download · MCP fetches today · posts today).
- HTTP: `ureq = "3"` (sync) for model downloads and the Google token
  call; **no reqwest, no keyring, no oauth2 crate**. `tokio` is in the
  workspace (`rt, net, time`). The model path is synchronous end to end.
- Bench: `bench --replay` (`crates/app/src/bench.rs:13-92`, scoring at
  `:250+`) runs fixtures through the local model and scores against
  `.expect.json` (`crates/core/src/eval.rs:10-56`); baseline 76/180.
  It takes a model path, not a backend — chunk 0 changes that.

## Prior art (what carries over)

- Every local-first peer ships local-default + BYOK as table stakes and
  sells the hosted tier as convenience credits at a visible markup:
  Zed bills provider list +10 % after $5 of included credits; Cursor
  adds $0.25/MTok even on BYOK for teams; Screenpipe runs credit packs
  next to BYOK with "nothing leaves by default, only the query text
  leaves when you enable a cloud provider". Obsidian Copilot keeps BYOK
  on the free tier and stores keys in the OS keychain.
- Roo Code's per-mode model assignment (cheap model for routine, strong
  model for hard) is the routing-table idea; Chronicle's version is per
  job kind.
- Cautionary: Cursor's privacy mode stops applying the moment BYOK is on
  — the data follows the provider's policy. Our egress line must say
  which provider, not just "cloud".
- Provider retention to state honestly: Anthropic commercial terms do not
  train on API content; retention is 30 days for covered models even
  under zero-data-retention orgs (privacy.claude.com, checked
  2026-09-04). OpenAI API: no training by default, ~30-day abuse
  retention.
- Login without a webview: loopback OAuth (RFC 8252) is the better UX,
  device flow (RFC 8628) the headless fallback. WorkOS AuthKit (1M MAU
  free, device-flow native) or Clerk (50k MRU free, loopback+PKCE
  reference for CLIs). Billing: Stripe Meters for true metering; prepaid
  packs need no metering; Lemon Squeezy / Paddle as merchant of record
  handle VAT at a larger fee. Details and URLs in the research notes at
  the end.

## Design

### 1. Backend trait

```rust
// crates/derive/src/backend/mod.rs
pub trait ModelBackend: Send + Sync {
    fn id(&self) -> BackendId;                       // Local | Anthropic | OpenAiCompat | ChronicleCloud
    fn caps(&self) -> Caps;                          // context_tokens, supports_json_schema, streams, cost_per_mtok
    fn complete(&self, req: Request, on_token: &mut dyn FnMut(&str)) -> anyhow::Result<Response>;
}
pub struct Request<'a> {
    pub job: JobKind,                                // Derive | Live | Consolidate | Describe | Journal | ... | Chat
    pub system: &'a str,                             // template head, stable → cache breakpoint
    pub user: &'a str,                               // digest / question
    pub history: &'a [(String, String)],             // chat only
    pub schema: Option<&'a serde_json::Value>,       // JSON jobs; Local maps to GBNF, cloud to json_schema
    pub max_output: u32,
}
```

- `Local` wraps the three existing structs; `Request.schema` selects the
  existing GBNF file (the JSON schema and the grammar are checked
  against each other in a test so they cannot drift).
- `Anthropic` posts to `/v1/messages` with `x-api-key`, streams SSE,
  passes `output_config.format = {type: "json_schema", schema}` for JSON
  jobs, `output_config.effort = "low"` for naming/description and
  `"high"` for chat/narrative, `thinking` omitted (adaptive default),
  `cache_control` on the system block. Default model is user's choice
  at setup; the picker lists Opus 5, Sonnet 5, Haiku 4.5 with list price
  per MTok next to each so the choice is informed, no silent default.
- `OpenAiCompat` posts to `<base>/chat/completions` with
  `response_format = {type: "json_schema", json_schema: {strict: true}}`
  where the endpoint supports it (OpenAI, OpenRouter passthrough, Ollama
  via `format`); otherwise falls back to "return only JSON" + a
  `serde_json` parse-and-retry once. Ollama is the way to run a bigger
  local model without touching llama.cpp integration.
- `ChronicleCloud` is `Anthropic` or `OpenAiCompat` with `base_url` =
  our gateway and `Authorization: Bearer <session>`; no separate code
  path beyond auth refresh.
- HTTP client: **hand-rolled on `ureq` 3** (already a dependency) with a
  ~40-line SSE line parser shared by both wire shapes. The unofficial
  Anthropic crates are 11–35 stars, one self-describes as "quickly
  drafted", and none document the 2026 request shapes
  (`output_config`, adaptive thinking, `effort`); `async-openai` is
  solid but pulls tokio+reqwest into a synchronous path for one
  endpoint. Synchronous is fine: every caller today is synchronous and
  runs in a worker thread or subprocess. Request/response structs are
  ours, versioned with the API date header.
- `fit_prompt` takes the limit from `caps().context_tokens` minus
  `max_output` instead of `N_CTX`. Cloud backends get the whole day's
  digest, all open tasks, all corrections, full MCP context; the 1900
  char digest budget applies to `Local` only.

### 2. Routing table

`models.toml` (0600, next to `google.toml`; **keys never enter
`config.toml`**):

```toml
[backends.anthropic]
kind = "anthropic"
model = "claude-opus-5"
api_key = "sk-ant-…"

[backends.router]
kind = "openai_compat"
base_url = "https://openrouter.ai/api/v1"
model = "…"
api_key = "…"

[routes]                 # job kind → backend name; missing = local
chat = "anthropic"
narrative = "anthropic"
standup = "anthropic"
journal = "anthropic"
task_description = "anthropic"
naming = "anthropic"     # m30 chunk 3's new-cluster namer
checkpoint = "anthropic"
suggest_task = "anthropic"
consolidate = "local"    # day tier; candidate for the Batches API later
derive = "local"         # batch reconciliation; flip only if replay says so
live = "local"
```

- When a cloud backend is added, the UI proposes the defaults above in
  one click ("use for writing and chat") with derive/live/consolidate
  left local; a second click ("use for everything") exists but is not
  the default. Per-row override in Settings › Model.
- If a route's backend is unreachable (offline, 401, 429 after
  retries), the job falls back to `Local` when a model is downloaded,
  otherwise stays queued with a visible reason on the feed row, never
  silently dropped. Interactive jobs (chat) show the error inline.
- A per-day cost cap (`max_usd_per_day`, default $2 for BYOK, the
  credit balance for hosted) computed from `usage` in each response;
  reaching it flips routes to local for the rest of the day and says so.

### 3. Prompts and output shape

- Same templates for chunk 1. Each JSON job gets a `serde_json` schema
  next to its GBNF (`task_output_v4.json`, `live_output_v1.json`,
  `consolidate_v1.json`, `checkpoint`, `suggest_task`); a test asserts the
  grammar accepts every schema-valid sample and vice versa for the
  fixtures.
- Cloud variants of a template (`*_cloud_v1.txt`) only when a replay or
  a side-by-side read shows the 4B-specific rules hurt (expected for
  `chat_v1.txt` — the "read N blocks" footer and totals-table quoting
  were built around the 4B's habits — and `standup_v1.txt`).
- Chat with a cloud backend keeps the full history (not three turns at
  800 chars) and the whole day's digest plus SQL totals; the streaming
  path is unchanged because `on_token` is the same callback.

### 4. Secrets, config, UI

- Keys live in `models.toml` at 0600 written by temp-file + rename,
  exactly `gcal::Tokens::save`. No keyring: the daemon runs as a systemd
  user unit where Secret Service over D-Bus is not reliably available,
  and the repo already has the file precedent. Revisit if a keychain
  shows up in the Mac/Windows ports.
- Settings › Model becomes three cards: **Local model** (today's
  panel), **Cloud backends** (rows on `ListRow`: name · kind · model ·
  status dot; `test` = one tiny request, verdict cached in meta like
  `mcp_probe:<name>`; `edit` masks the key like the MCP env form; `×`
  confirm-click), **Routing** (the table, one row per job kind, backend
  dropdown per row, the two preset buttons).
- Onboarding: first screen offers three doors and a "later" link:
  *download the local model (~2.5 GB, nothing leaves this machine)* ·
  *paste an API key (your data goes to that provider)* · *sign in
  (hosted tier)*. Capture and the timeline start regardless; jobs that
  need a model queue with a "no model configured" row in the feed until
  one is chosen. This changes `onboarding.rs` from a linear download
  flow to a choice.

### 5. What leaves this machine, exactly

- The egress line (`settings.rs:614`) becomes computed from the routing
  table plus today's `ai_jobs`: "today: 14 journal + 3 chat prompts to
  Anthropic (claude-opus-5) · MCP fetches 2 · posts 0 · nothing else".
  Per-row hover shows what a prompt of that kind contains (window
  titles, file paths, AI-session prompts, calendar attendees).
- **Redaction pass** on every cloud-bound prompt, in `core`: secret
  patterns (API keys, bearer tokens, AWS/GitHub/Slack key shapes, long
  hex/base64 runs) replaced with `‹secret›`; opt-in family toggles from
  m30 (mail subjects, DMs, AI-session prompt bodies) honoured before the
  prompt is built, not after. Pattern set ported from gitleaks' public
  rules, kept in one file with a test corpus. The pass is also applied
  to the local path so the two never differ in what the model sees.
- Provider retention stated in the UI when a key is added, one sentence
  each, with the date checked.

### 6. Sign-in and the hosted tier

- Auth: loopback OAuth with PKCE against the provider (WorkOS AuthKit or
  Clerk; decide on free-tier and Rust-friendliness at build time),
  reusing the `gcal-login` listener. Session token + refresh stored in
  `models.toml` under `[account]`. Device flow as the fallback for
  headless boxes.
- Gateway: a metered proxy that fronts Anthropic and one OpenAI-compat
  provider, issues per-user virtual keys with budgets, streams
  passthrough, and logs tokens per request. Candidates: LiteLLM proxy
  (virtual keys + budgets built in) vs. an axum service we own
  (`axum = "0.8"` is already in the workspace for the local server).
  Decision: LiteLLM for chunk 5 — it is the only open, self-hosted
  option verified to front Anthropic and arbitrary OpenAI-compatible
  base URLs with hard per-key budgets, at a ~$5–10/month VPS floor;
  owning rate limiting and budgets on day one is not worth an axum
  service.
- Billing: prepaid credit packs first (Stripe one-time checkout, no
  meters), balance shown in Settings, the daemon reads remaining
  balance from the gateway. Metered monthly (Stripe Meters) only if
  packs prove annoying. Merchant-of-record (Lemon Squeezy / Paddle) if
  VAT becomes real.
- We become the data controller for hosted traffic: DPA with the
  provider, 30-day retention statement, no logging of prompt bodies at
  the gateway (token counts and job kinds only).

### 7. Accuracy: where cloud actually helps, and the number that decides

- Chat, narrative, standup, journal, description: cloud wins on quality
  and the failure rate drops to near zero — no replay needed, the
  ai_jobs success column is the metric.
- Naming (m30 chunk 3): cloud wins on label quality; cheap (short
  prompt, `effort: low`).
- Derive reconciliation and live tier: unknown. Chunk 0 runs the replay
  with a cloud backend on the same fixtures. If cloud alone moves
  76/180 substantially, the routing default for `derive` changes and
  m30 chunk 3's "batch derive demoted behind a flag" gets a second flag
  value.
- Speed: a cloud naming call is 1–3 s vs. 100 s per local batch; the
  live tier could run every minute instead of every five. That is the
  "open it at any moment" win and it is independent of accuracy.

## Chunks

0. **Replay with a cloud backend (gate, half a day).** `bench --replay
   --backend anthropic --model …` through a minimal `Anthropic` impl
   used only by the bench. Output: the replay score and per-batch
   latency/cost next to the 76/180 local baseline. Decides the `derive`
   route default and whether chunk 2 exists. Needs a key on this box.
1. **Backend trait + Anthropic BYOK for the describer family and
   chat.** Trait, `Local` wrapper (no behaviour change, golden tests
   green), `Anthropic` impl with streaming + json_schema + caching,
   `models.toml` load/save 0600, `routes` with the "writing and chat"
   preset, fallback-to-local, per-day cost cap, `usage` logged into
   `ai_jobs` (new `prompt_tokens`/`gen_tokens`/`backend` columns — a
   migration, so this lands after m30's 016 chain). Settings › Model
   cloud card + routing rows. Egress line computed. Chat with full
   history and whole-day context on cloud. Metric: ai_jobs success per
   kind, chat latency, first real week of journals read by James.
2. **Naming and live tier on cloud (after m30 chunk 3).** Route the new
   cluster namer and the one-interval live prompt; live cadence becomes
   a backend property (1 min cloud, 5 min local). Metric: replay,
   median segment length, feed latency.
3. **OpenAI-compatible backend.** Base URL + key + model; OpenRouter and
   Ollama tested; schema fallback path. Adds the second card row type.
4. **Onboarding + redaction.** Three-door first run, "no model
   configured" feed row, redaction pass with its test corpus, family
   toggles wired, provider retention sentences. Site copy: one
   paragraph on "your data, your choice".
5. **Sign-in + gateway + credits.** Auth provider chosen, loopback flow,
   `[account]` in `models.toml`, gateway deployed with virtual keys and
   budgets, prepaid packs, balance in Settings, `ChronicleCloud`
   backend = existing impl + bearer auth + refresh.
6. **Cost and telemetry.** Per-kind token/cost rollup in Reports
   ("model spend this week"), Batches API for the day tier if it stays
   on cloud (50 % off, minutes of latency are fine there).

Order rationale: 0 is a number; 1 is where James feels it (chat,
journals, no more invented next steps) and carries the security work
that everything after depends on; 2 waits for m30; 3 is small; 4 is the
product surface; 5 is a separate product with its own ops; 6 is polish.
Chunks 1, 3, 4 can run as parallel agents on disjoint files (derive
backend / capture-config / ui) once the trait from 1 is on main.

## Risks and open questions

- **Two sources of truth for output shape** (GBNF and JSON schema).
  Mitigated by the cross-check test; long term the schema generates the
  grammar.
- **Prompt drift between backends.** A template tuned for the 4B may
  read as over-constrained to Opus. Keep one template per job until a
  measured difference; version cloud variants separately.
- **Key leakage paths**: logs (never log request bodies or headers),
  crash reports (none today), the `chronicle status` output, the meta
  table. One grep-able rule: the key string only ever lives in
  `models.toml` and the `Authorization`/`x-api-key` header.
- **Rate limits.** Anthropic's start tier is 1,000 RPM / 2M input
  tokens per minute for Opus 5 / Sonnet 5 (docs checked 2026-09-04), far
  above one desktop; OpenRouter/Ollama vary. Still: per-backend
  concurrency of 1, honour `retry-after`, and a burst of 30 queued
  journal jobs after a long offline stretch drains at the queue's pace.
- **Offline.** Laptop on a train: cloud routes fall back to local if a
  model exists, otherwise queue. Never lose a job; never block capture.
- **Hosted tier liabilities**: we hold user text in flight, we owe a
  DPA, a retention statement, and abuse handling. Chunk 5 is the only
  chunk that makes Chronicle a service; everything before it keeps
  Chronicle a program.
- **Anthropic's `refusal` stop reason** on Fable-tier models: not
  relevant at these prompt shapes, but the client must treat
  `stop_reason != end_turn` as a failed job, not empty output.
- **Cost surprises.** The daily cap plus a visible spend line are the
  guard; defaults never route derive/live to cloud without the replay
  number.

## Out of scope

- Embeddings via cloud (m30 chunk 6 benches local embeddings first).
- Teams sync and shared workspaces (`teams-direction.md`); the gateway
  is not a sync server.
- Fine-tuning or training on user data, in any tier, ever.
- Mac/Windows keychain storage (arrives with those ports).

## Research notes (sub-agent findings, 2026-09-04)

### Rust clients and provider APIs

- Anthropic crates, all unofficial: `misanthropic` 1.0.0-alpha.18
  (2026-08-30, ~35 stars) https://github.com/cortesi/misanthropy ;
  `async-anthropic` (11 stars, "quickly drafted")
  https://github.com/bosun-ai/async-anthropic ; `anthropic-sdk-rust`
  (low adoption). None document `output_config` / adaptive thinking /
  `effort`. OpenAI-compatible: `async-openai` 0.41.3 (2026-07-31, 1.8k
  stars, SSE) https://github.com/64bit/async-openai ; `openai-api-rs`
  10.0.1. Multi-provider: `genai` 0.6.5 (842 stars)
  https://github.com/jeremychone/rust-genai ; `rig-core` 0.42.0. SSE:
  `eventsource-stream` 0.2.3 (2022, stable, async only).
- JSON guarantees: Anthropic `output_config.format` json_schema + strict
  tools, GA https://platform.claude.com/docs/en/build-with-claude/structured-outputs ;
  OpenAI `response_format` json_schema strict, works; OpenRouter
  passthrough partial (enforced only when the underlying model has
  strict mode) https://openrouter.ai/docs/guides/features/structured-outputs ;
  Ollama `format: <schema>` works https://docs.ollama.com/capabilities/structured-outputs ;
  Gemini OpenAI-compat works https://ai.google.dev/gemini-api/docs/openai.
- Caching: minimum prefix 512 (Opus 5, Fable 5/5.1), 1,024 (Sonnet 5,
  Opus 4.8), 4,096 (Haiku 4.5); read 0.1× input, write 1.25× (5 min) /
  2× (1 h) https://platform.claude.com/docs/en/build-with-claude/prompt-caching.
  Batches: 50 % off, 24 h SLA, usually under an hour
  https://www.anthropic.com/news/message-batches-api. Start-tier rate
  limits for Opus 5 / Sonnet 5: 1,000 RPM, 2M ITPM, 400k OTPM; cache
  reads do not count against ITPM
  https://platform.claude.com/docs/en/api/rate-limits.
- Key storage: `keyring` 4.2.0 (2026-08-29) split into `keyring-core` +
  backend crates; Secret Service fails under systemd `--user` units
  without a session bus (keyring-rs issues #15, #207); kernel keyutils
  is in-memory only. Confirms the 0600 file decision for the daemon.
- Gateways: LiteLLM proxy (MIT, virtual keys with hard budgets, streams,
  fronts Anthropic + any OpenAI-compatible base URL, needs Postgres,
  ~$5–10/mo VPS) https://docs.litellm.ai/docs/proxy/users ; Portkey
  (gateway core MIT in 2026, self-hostable); Cloudflare AI Gateway (free
  at the edge, BYOK + per-user spend limits, fixed provider list — could
  not verify arbitrary base URLs)
  https://developers.cloudflare.com/ai-gateway/features/spend-limits/ ;
  Helicone (acquired by Mintlify 2026-03, velocity risk — secondary
  source). Recommendation: LiteLLM.

### Prior art and hosted tier

- Screenpipe privacy + credits: https://screenpi.pe/privacy ;
  https://screenpipe.com/onboarding (credit counts via aggregator,
  unverified exact numbers).
- Zed pricing (+10 % over list after credits): costbench aggregator,
  verify on zed.dev. Cursor data use / BYOK disables privacy mode:
  https://cursor.com/data-use.
- Raycast BYOK: https://manual.raycast.com/ai/bring-your-own-keys.
  Obsidian Copilot pricing (BYOK free, keys in keychain):
  https://www.obsidiancopilot.com/en/pricing. Msty: https://msty.ai/pricing/.
- Roo Code per-mode models: https://docs.roocode.com/providers/openrouter
  (project archived May 2026 per secondary source).
- RFC 8252 https://datatracker.ietf.org/doc/html/rfc8252 ; device flow
  https://oauth.net/2/device-flow/. Clerk CLI auth
  https://clerk.com/docs/cli ; WorkOS CLI auth
  https://workos.com/docs/authkit/cli-auth. Supabase PKCE
  https://supabase.com/blog/supabase-auth-sso-pkce.
- Stripe Meters https://docs.stripe.com/api/billing/meter/create ;
  Lemon Squeezy usage billing
  https://www.lemonsqueezy.com/features/usage-based-billing.
- Anthropic retention https://privacy.claude.com/en/articles/15425996-data-retention-practices-for-covered-models ;
  OpenAI API data https://developers.openai.com/api/docs/guides/your-data.
- Redaction: gitleaks rules https://github.com/gitleaks/gitleaks ;
  censgate/redact (Rust, early) https://github.com/censgate/redact.
- Anthropic API shapes used above (structured outputs
  `output_config.format`, adaptive thinking default, `effort`, cache
  minimums 512/1024 tokens, list prices) from the Claude API reference
  cached 2026-06-24.

## Shipped (2026-09-04, chunks 0 + 1)

Seven commits on `m31` (c198692 → e10f790), 230 tests, clippy clean.

- **Engine split** (`c198692`): `derive::prompts` renders every describer
  prompt and parses its output for both engines; `derive::text` holds
  `TextBackend` / `JobKind` / `Request` / `Completion`; `Prompt::render`
  and `runner::finish_derive` expose the batch path; JSON schemas sit next
  to each GBNF in `grammars/*.json` with a test pinning them to the
  structured-output subset. Deviations from the design above: the trait is
  in `text.rs` (`backend.rs` was already llama plumbing), `[routes]` keys
  are the `ai_jobs.kind` strings verbatim (`name_task`, not `naming`), and
  templates stay one user message with only the `/no_think` head stripped
  (the instruction heads are under every cache minimum).
- **Anthropic backend** (`650013d`): `derive::cloud::anthropic` on ureq 3,
  streaming SSE, `output_config.format` json_schema for JSON jobs, `effort`
  per job (low naming/description, medium journal/derive, high chat/
  narrative/standup), retries on 429/529/5xx with `retry-after`, classified
  `CloudError` whose text never carries the key, price table with cache
  reads at 0.1×. Four loopback mock tests.
- **`models.toml`** (`0a8bbfa`): `core::models_config`, 0600 atomic save,
  `route_for` / presets / `remove_backend`; migration 023 adds `backend`,
  `prompt_tokens`, `gen_tokens`, `cost_usd` to `ai_jobs`; `defer_ai_job`
  refunds the attempt; kind-filtered `next_eligible_ai_job_in`; chat writes
  a done row through `insert_done_ai_job`.
- **Chunk 0 hook** (`ebd55ce`): `bench --replay --backend <name>` runs the
  same digest through the cloud backend and scores identically, printing
  tokens and dollars per batch. **The gate itself has not run: no key on
  this box.** To run it: add `[backends.anthropic]` to
  `~/.local/share/chronicle/models.toml`, then
  `chronicle bench --replay --backend anthropic --since 7` against the
  local baseline (37/134 scorer, 44/134 qwen3-4b per the m30 doc). The
  number decides the `derive` route default and whether chunk 2 exists.
- **Routing in the worker** (`36d0952`): cloud first when routed and under
  the cap, fall back to the local model on any `CloudError`, defer with a
  `cloud: …` reason when there is no local model; the daemon runs cloud
  jobs in a second slot with no idle/battery gate, backs off 1→10 min after
  a deferral, and re-reads `models.toml` on mtime change (no restart).
- **Chat** (`7699f96`): `chat::Budget` parameterises the context (24k
  tokens on cloud vs 2.2k local, table and FTS caps scale); the whole
  conversation goes as history; streaming unchanged.
- **Settings › Model** (`e10f790`): cloud backends card, routing grid with
  the two presets, daily cap, computed egress line
  ("1 chat + 1 suggest task + 1 task description to anthropic
  (claude-opus-5) · MCP context fetches today 18 · posts today 0"),
  `can_run(kind)` replaces `model_missing` on Home/Reports/Onboarding/Chat.
  Rename is remove + re-add; presets target the first backend by name.

Verified offline in a sandbox (`XDG_DATA_HOME` copy of the live DB,
`scripts/mock_messages_api.py` on 127.0.0.1:5799 as `base_url`):
task_description on cloud → done with `backend=anthropic`, 1500/40 tokens,
$0.0085; mock returning 500 with no local model → job back to `pending`,
attempts 0, error `cloud: provider error 500: api_error: boom`; same with
the models dir linked → three retries then "running local", done in 20 s;
chat → system prompt + 3-message history, effort high, 28 context blocks,
`ai_jobs` chat row; suggest_task → `json_schema` request, parsed. Not
verified: the daemon's cloud slot end to end (needs a sandbox daemon,
which opens a second UI window), and anything against the real API.

Open for chunk 2+: the OpenAI-compatible backend (`cloud::build` bails on
`openai_compat`), onboarding three-door and the redaction pass, cost rollup
in Reports, the derive/live route once the gate number exists.
