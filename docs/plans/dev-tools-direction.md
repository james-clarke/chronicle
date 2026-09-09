# Developers only — direction research (2026-09-09, no code)

James's framing: shift focus solely to developers, put Chronicle in front of
the mass of them, and make accuracy the product: use API (frontier) models
wherever they buy derivation accuracy, and cover the whole tool surface a
working developer touches, including the connections those tools use to
talk to each other. Ten sub-agents surveyed editors and AI coding agents,
terminals and infra, VCS hosts / trackers / chat / calendar / browsers,
observability / networking / databases / cloud / CI / git tooling,
connection kinds and MCP, the competition and what developers say they
want, how shipping products and the literature use LLMs for attribution,
platform reach and distribution, and the codebase twice (anchors and
collectors; model call sites and eval). This is the synthesis, with the M35
plan (`m35-project-first-plan.md`) reviewed against it. Decisions are
proposals until James edits them. Sources at the end.

## The short version

1. **Accuracy is the product.** The claim is "the most accurate account of
   a developer's day": more of what their tools already write than any
   other tracker reads, a deterministic pipeline that places time where the
   evidence says, and a frontier model reasoning over the ambiguous rest.
   Privacy is table stakes and stays true (nothing leaves without a chosen
   backend, the redaction pass, an exact egress line), but it is no longer
   the headline.
2. **M35 is the right shape.** Rule-matched projects, a declared sink,
   tasks confined to a project, no model choosing a project. Keep the
   chunks and the order. Six changes to chunk 0 make it hold on other
   people's machines (below).
3. **The mass-market case is zero-config: a repo is a project.** Chronicle
   discovers repos from what it already sees and proposes them. TOML groups
   repos and adds non-repo matchers. Project identity is the git remote,
   not the path.
4. **API models raise accuracy in five places, and never in one.** Naming,
   descriptions, merge and duplicate judgement, the standup and chat go to
   a frontier model. The segmenter and the scorer stay deterministic, and
   the model is an advisor on low-margin verdicts only. Corrections come
   back as retrieved few-shot examples. Every model output that makes a
   claim carries evidence ids the pipeline checks. A nightly reconciliation
   of the whole day runs on the Batches API. The eval loop measures all of
   it; the first gate (`bench --replay --backend anthropic`) has never run
   because this box has no key.
5. **The differentiator nobody else has is AI-agent sessions as a source.**
   Only Claude Code is read today. Copilot, Cursor, Codex CLI, Gemini CLI,
   Aider, Cline, Amp and OpenCode all leave transcripts with a cwd on disk.
6. **The rest of the tool surface splits in two.** Repo-local link files
   (`.vercel/project.json`, `fly.toml`, `supabase/config.toml`, Bruno
   collections, build manifests) and git hooks give hard project identity
   for free. Observability, cloud consoles, incident and API tools give the
   kind of work (deploying, on-call, debugging prod), which is a new anchor
   the standup should name.
7. **Connections, ranked by tools unlocked per unit of user effort:** files
   on this machine, local servers and sockets, the user's existing CLI
   logins, OAuth accounts, a generic MCP client, pasted tokens. No webhook
   receivers.
8. **Reach is platform, then sources.** Windows ~50 % of professional
   developers, macOS ~33 %, Linux ~28 % and mostly Wayland. Chronicle
   captures Linux X11 only. Order: macOS, Wayland (wlroots and KDE),
   Windows, GNOME. cargo-dist into Homebrew, winget/Scoop, AUR, .deb. No
   Flatpak.

## M35 reviewed

What the plan gets right, against the field: rules not ML for the project
(Rize's ML needs constant correction; ActivityWatch's manual rules are its
top complaint, but rules seeded from git are what Timing and WakaTime users
accept); the declared task as the sink (Everhour and Tempo win Jira shops on
"the ticket I said I'm on gets the time"); sessions attributed by cwd (the
Claude Code transcript's `cwd` is the most reliable place anchor Chronicle
has); a general task per project so grouping works with no declared task.

Six changes, in plan order:

### Chunk 0: identity, discovery, matchers

- **Project identity is the remote.** Today a place is a path basename
  (`extract::place_from_path`, crates/core/src/extract.rs:1023), so a
  worktree or second clone is a different place (crates/capture/src/git.rs:47).
  Parse `git remote get-url origin` to `host/org/repo` (one URL parser
  covers GitHub, GitLab, Bitbucket, Azure DevOps, Gitea). `repos` entries
  may be paths or remotes; a path resolves to its remote when it has one.
  `git worktree list` adds every worktree as an instance. This is also the
  key two machines share when teams (`teams-direction.md`) arrive.
- **Discovery, not configuration.** Every place source already produces a
  path: AI session cwd (ai_sessions.rs:154), listening-port cwd
  (crates/capture/src/ports.rs:32), editor heartbeat project
  (crates/core/src/heartbeats.rs:99, never matched to `git_repos` today),
  editor and VCS window titles (extract.rs:1107, :348), and from M37 the
  repo-local link files below. A path under a git repo that is not yet a
  project shows in Home as "new project seen: ~/dev/foo, 2 h this week"
  with one click to add; `chronicle project test` lists them. First run
  pre-fills from `git_repos`, the parents of those dirs (`~/dev`, `~/src`,
  `~/code`, `~/projects`) and VS Code's recent-workspace list.
- **The matcher takes a path, not a basename** (monorepo subpaths, two
  projects under one parent).
- **Drop `localhost:8000` from `domains`.** The ports collector maps a
  listening port to the cwd of its process; the browser's `localhost:<port>`
  anchor (extract.rs:1242) should resolve through it. A domain list is for
  the hosted app and the tracker.
- **Ticket keys:** keep `ticket_regex` (config.rs:194) for the Jira /
  Linear / YouTrack / Shortcut form, add repo-scoped `#123` from branch
  names and commit messages (today only `owner/repo#123` in GitHub URLs,
  extract.rs:1281), and match branch names case-insensitively (Linear
  branches are `user/eng-123-title`). Two regexes cover ~90 % of trackers.

### Chunk 1: sinks

- As planned, plus: task 152 (`chronicle@main · 94020a60…`) was minted
  this morning beside declared task 145 after the sink fix shipped. Before
  chunk 1 is called done, replay that window and say why the sink lost.

### Chunk 2: derivation per project

- As planned. The trivial split is the cheapest fix for interleaving;
  nothing in the field does better.

### Chunk 3: Home and the task manager

- Add a **Sources** row per project (which of git, sessions, editor, shell,
  ports, browser, calendar, link files fed it this week). It shows a new
  user what Chronicle sees and what it cannot yet, and drives collector
  setup from the place the gap shows.

### Chunk 5: cleanup

- As planned. The self-score's "unfiled minutes" and "cross-project
  placements: 0" are the two numbers for the site's proof block once the
  sources land.

### Answers to the three open questions

- **Silos.** Two, by ticket prefix: ACME (contoso, mailer, admin-api) and
  ACAI (acme-ai-agent-backend). Tickets cross repos and the standup
  reads per ticket; a silo is the thing a standup line is about. The
  `repos` list is exactly that grouping; every span keeps its place anchor
  so the per-repo view survives inside it.
- **`derive` default.** On. The 09-09 damage was cross-project bleed, which
  chunk 1 removes; inside one project a derived task can only subdivide
  time the declared task did not claim. Off makes every undeclared project
  one bar, which is WakaTime. Keep the per-project off switch.
- **General task name.** Not shown as a task. The project line carries it:
  "chronicle 3 h 12 m, 40 m not on a task". Standup and reports say
  "<project>: other work (40 m)". Internally `source = 'project'`.

## Maximum accuracy: API models in the pipeline

### What the code does today

Eleven model call sites, all routed per job through `models.toml`
(`ModelsConfig::route_for`, crates/core/src/models_config.rs:162; the
decision in `run_routed`, crates/app/src/ai_job.rs:84, with a $2/day cap
and local fallback): derive batch, live label, consolidate,
task_description, journal, checkpoint, narrative, standup, suggest_task,
name_task, chat. The Anthropic backend (crates/derive/src/cloud/anthropic.rs)
streams, uses `output_config.format` json_schema for JSON jobs, sets
`effort` per job, and prices cache reads. Three deterministic decisions
never see a model: the segmenter's `decide` with
`Verdict { ranked, margin, confident }` (crates/core/src/segmenter.rs:774),
the scorer (crates/core/src/profile.rs:824), and the place veto
(segmenter.rs:711). Corrections (migrations/001_schema.sql:45) feed the
bench replay only; nothing injects them into a prompt or a profile.
Embeddings (crates/derive/src/embed.rs) are bench-only and unstored. The
only redaction on a cloud-bound prompt is stripping `/no_think`
(crates/derive/src/prompts.rs:25). Prompt caching is declared on an empty
system message (anthropic.rs:78); the derive prompt's ~1100-token stable
instruction and ~2200-token digest are not split for it. The M31 gate,
`chronicle bench --replay --backend anthropic --since 7` against the local
baseline (37/134 scorer, 44/134 qwen3-4b), has never run: no key on this
box.

What the field says (Rize, Timely, Dayflow, Screenpipe, Copilot, Cursor,
Linear): nobody publishes an accuracy number; the products that survive
keep boundary and assignment deterministic and use the model for naming
and narrative, where an error is cosmetic and a human edits the draft.
The literature adds: retrieved corrections as few-shot beat fixed
examples; structured outputs remove a failure class; self-consistency pays
only on the ambiguous middle; an abstention path ("unsure, pick one")
beats a calibrated confidence nobody trusts; an LLM judge must itself be
checked against a human-labelled sample.

### Design: where the model goes, and where it never goes

| Step | Today | Proposed | Backend |
|---|---|---|---|
| Segment boundaries | deterministic | unchanged | none |
| Segment → task score | deterministic, `margin` | unchanged; the number people bill against never comes from a model | none |
| Low-margin verdicts (`confident = false`) | placed on the winner with "to confirm" | a **pairwise advisor**: "given this evidence, task A or task B or new?", structured output with the evidence ids it relied on; only re-ranks when the model's cited ids exist in the segment; abstains to the UI below a similarity floor | Sonnet 5 online, Opus 5 in the nightly pass |
| Naming a new cluster | qwen 4B, window-title placeholder on failure | frontier, with the 3–5 most similar past corrections retrieved by embedding as examples | Sonnet 5 |
| Description, journal, checkpoint | 52–68 % success locally | frontier; failures were length and grammar limits | Sonnet 5 |
| Merge and duplicate detection | `consolidate` (MAX_MERGES 6), deterministic folds | frontier judge that must quote overlapping evidence, not similarity; proposals, never auto-applied above a size | Opus 5 nightly |
| Standup, narrative | prose with a source tag per claim | unchanged rule, enforced: every claim carries an evidence id the pipeline verifies against the DB; unverifiable claims are dropped and the job re-runs once | Opus 5 |
| Chat | 24 k context on cloud | unchanged, plus the Messages API MCP connector for on-demand reads of the user's Jira / Linear / GitHub during a question | Opus 5 |
| Project rules | none | "suggest a matcher" from the top unfiled titles, shown in `project test` and the Settings card | Sonnet 5 |
| Nightly reconciliation | consolidate per day | the whole day (segments, verdicts, names, merges, standup) as one Batches API job at half price; the online pass stays fast and cheap | Opus 5, batches |

Model ids: `claude-opus-5`, `claude-sonnet-5`, `claude-haiku-4-5` for the
live label if latency needs it. Effort per job as M31 set it (low for
naming, medium for journal and derive, high for chat, narrative, standup,
merges).

### Corrections as memory

- Store an embedding per task (label + top evidence) and per correction
  (the evidence that was moved, the wrong and right task). `embed.rs`
  already produces vectors; add a `task_embeddings` table and a cosine
  scan (a few thousand rows, no index needed).
- Every naming, merge and advisor prompt gets the k nearest corrections as
  examples. This is the loop the deterministic profile update started; it
  reaches the model.
- Corrections also update the scorer's profile weights, as M30 intended, so
  the deterministic path learns too and the model is not the only memory.

### Prompt hygiene

- Split every template into a frozen prefix (instructions, schema, project
  rules, the open-task list sorted by id) and the volatile digest, and put
  the `cache_control` breakpoint after the prefix. Target cache reads on
  ≥ 85 % of calls; log `cache_read_input_tokens` per job.
- Structured outputs with `strict` schemas everywhere a field is parsed;
  an `evidence_ids` field on every claim-making job, checked post-hoc.
- **Redaction pass before any cloud call**: drop URL query strings, path
  segments that look like tokens, anything matching the secrets regexes
  (AWS keys, `sk-`, JWTs, connection strings), and the command line of any
  shell span. The M31 "what leaves" line becomes exact per job.
- Determinism: the nightly pass regenerates, the online pass never
  rewrites history, so drift between runs cannot rewrite yesterday.

### Measuring it

- **Placement:** the bench replay against corrections, per backend.
  Precision and recall of placements versus what the person moved, not raw
  agreement. This is the gate that decides the `derive` route default.
- **Faithfulness:** share of standup and description claims whose evidence
  id resolves. Target 100 %; the pipeline enforces it, the metric catches
  regressions.
- **Names:** pairwise judge (Opus 5) between the previous and the new
  naming prompt on a fixed fixture set, itself spot-checked by James on a
  sample each release.
- **Abstentions:** how often the advisor punts to the UI per day. Rising
  means evidence shape got worse, not the model.
- **Drift:** same-day re-run diff on names and standup.
- **Cost:** `ai_jobs.cost_usd` per kind per day (migration 023 already
  stores it), with the cap in `models.toml`.
- All six on the self-score row and in `chronicle status`, per backend, so
  the local and cloud paths are compared on the same days.

### Cost envelope

About 50–100 calls a day at 2–4 k tokens. With the prefix cached and the
nightly pass in batches: roughly $0.05–0.30 per user per day on a Sonnet
and Opus mix, $1.50–9 a month. Reclaim and Motion charge $10–29 a seat for
their AI tiers; M31's $5–8 hosted tier holds.

### Sign in instead of a key

James's ask: sign in with an account rather than set up a card and paste
an API key. What exists, checked 2026-09-09 against the official docs:

| Door | What it is | Billing | Verdict |
|---|---|---|---|
| **Your Claude Code login** | Chronicle spawns `claude -p` (headless Claude Code) as a subprocess with the prompt on stdin: `--bare --output-format json --json-schema <schema> --model <id> --max-turns 1 --no-session-persistence`, `stream-json --include-partial-messages` for chat. Verified on this box: it answered under the Claude Code login with no API key and returned `result`, `session_id`, `usage` and `total_cost_usd`. | the user's Claude subscription quota; `total_cost_usd` is a list-price estimate for the cost line | **Build it (M36 chunk 0).** Headless mode is documented for automation. The docs do not address a local program spawning it on the user's own machine, and the Agent SDK requires an API key, so ship it labelled "for people who already have Claude Code" and ask Anthropic before advertising it on the site. Overhead: Claude Code's own system prompt rides along (about 25 k cached tokens per call in the test), so each call is roughly $0.02–0.05 at list; try `--exclude-dynamic-system-prompt-sections` |
| `ant auth login` profile | Anthropic's CLI stores an OAuth profile under `~/.config/anthropic/credentials/`; Chronicle could read it (the "Your CLIs" tier) or send the same Bearer token | the Console workspace, which needs a payment method | no gain over a key for James; worth supporting as credential reuse later |
| Chronicle Cloud sign-in | M31 chunk 4: loopback OAuth to our gateway, prepaid credits, the same wire shape | us, metered | the real "sign in with account" for everyone who has neither Claude Code nor a key; unchanged |
| "Sign in with Anthropic" for third-party apps | does not exist | | |

`claude_code` is a third `BackendKind` next to `Anthropic` and
`OpenAiCompat` (crates/core/src/models_config.rs:72), one file under
`crates/derive/src/cloud/`, a label and form branch in
`crates/app/src/ui/cloud.rs:52`, no key field. The bench takes it as any
other backend (`--backend <name>`, crates/app/src/bench.rs:1263), so the
M31 gate runs on this box with no card.

### First actions

1. Build the `claude_code` backend and run the M31 gate through it:
   `chronicle bench --replay --backend claude --since 7`. The number
   decides the default `derive` route and whether the advisor is worth
   building.
2. Redaction pass and the prefix split (small, and both are prerequisites
   for turning the cloud route on for other people).
3. Corrections embeddings and the advisor, gated by the bench.

The chunked plan is `m36-accuracy-plan.md`.

## What developers use, and what Chronicle sees

Share figures are Stack Overflow 2025 (professional developers, multi-select)
unless noted. Effort is S / M / L for a Rust collector. "Today" is what
Chronicle reads now.

### Editors and IDEs

| Tool | Share | Local signal | Today | Gap and effort |
|---|---|---|---|---|
| VS Code | 76 % | WakaTime plugin; `workspaceStorage/*/state.vscdb` recent workspaces; title `file — folder` | heartbeats (plugin + api_url edit), title path | read `state.vscdb` for recent workspaces and remote URIs: S |
| Cursor, Windsurf | 18 %, growing | same layout under `~/.config/Cursor`, `~/.config/Windsurf` | title only if app class is known | same reader, three brands: S |
| JetBrains family | 72 % use one | WakaTime plugin; `recentProjects.xml`; title `project [path] — file` | heartbeats, title | `recentProjects.xml` + the repo's `.git/HEAD` for branch: M |
| Vim / Neovim | 24 % / 14 % | WakaTime plugin; shada; title is the terminal's | heartbeats | nothing beyond the shell hook below |
| Visual Studio | 29 % | WakaTime plugin; MRU in `%APPDATA%` | none (Windows) | with the Windows port: M |
| Zed | growing | WakaTime plugin; `zed/db/*/db.sqlite` `workspace_location` | heartbeats | S |
| Sublime, Emacs, Helix, Xcode, Android Studio | tail | plugin or session files | heartbeats where a plugin exists | leave to heartbeats |

The WakaTime protocol already covers every GUI editor with one setup step.
The `state.vscdb` and `recentProjects.xml` readers remove that step for the
two families that are ~90 % of editor time. Window titles stay a fallback:
Electron titles are user-overridable per workspace.

### AI coding agents

| Tool | Share | Local transcript | cwd? | Effort |
|---|---|---|---|---|
| GitHub Copilot (IDE, CLI, agent) | ~42 % of paid seats | CLI: `~/.copilot/session-state/<id>/{workspace.yaml,events.jsonl}`; IDE chat inside `state.vscdb` | yes (workspace.yaml) | M |
| Cursor agent | 5 M+ users | `~/.cursor/chats/<id>/*/store.db`, `cursorDiskKV` in `state.vscdb` | sometimes (meta.json) | M, schema churns |
| Claude Code | 18 % of work use, growing fastest | `~/.claude/projects/<proj>/<session>.jsonl`; hooks | yes | done |
| OpenAI Codex CLI | growing | `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` | yes | S |
| Gemini CLI | growing | `~/.gemini/tmp/<project_hash>/chats/` | project hash → root | S–M |
| Aider | power users | `.aider.chat.history.md` in the repo | repo root | S |
| Cline / Roo Code | popular extension | `globalStorage/saoudrizwan.claude-dev/tasks/<id>/*.json` | yes | S–M |
| Amp | dev-tool crowd | `~/.local/share/amp/threads/T-*.json` | yes | S |
| OpenCode | growing OSS | `~/.local/share/opencode/opencode.db` | yes | S–M |
| Devin, Kiro, Augment, Junie | cloud / ACP | none documented | no | skip |

Copilot, Cursor, Claude Code, Codex and Gemini CLI give ~80 % of agent use.
The `ai_session_dirs` provider generalises: one trait `SessionFormat` with
`scan(dir)` and `tail(file)` yielding (cwd, branch?, prompt, paths, ts);
Claude Code is the first implementation. Store the same fields as now
(first prompt clipped to 120 chars, paths, cwd), never the transcript.

### Terminal, shell, infra, remote

| Signal | Reveals | Today | Effort |
|---|---|---|---|
| Shell hook (`chronicle shell-init` prints a precmd hook for zsh / bash / fish / PowerShell that POSTs cwd, program name, duration to 127.0.0.1) | cwd per command, no command line | atuin only (`shell_history`, off; atuin is enthusiast-tier) | S; the mass path, atuin stays as an alternative |
| `tmux list-panes -F '#{pane_current_path}'` | live cwd per pane | none | S |
| Listening ports → `/proc/<pid>/cwd` | repo of every running dev server | done (Linux) | macOS `lsof`, Windows `GetExtendedTcpTable`: M with the ports |
| Docker Compose labels `com.docker.compose.project.working_dir` via the events socket | exact repo path per running stack | none | S |
| VS Code `state.vscdb` `vscode-remote://ssh-remote+host/path`, `codespaces+name` | the only local trace of a Remote-SSH or Codespaces session | none | S, and the fix for the remote-dev blind spot |
| Native shell history (zsh, bash, fish, PSReadLine) | timestamp, no cwd | none | skip; command text is a secrets trap |
| kubectl context, k9s, Tilt, `gh run watch` | cluster, not repo | none | skip; the invoking shell's cwd is the signal |

### Git tooling and hooks

| Signal | Reveals | Today | Effort |
|---|---|---|---|
| **Chronicle-installed git hooks** (`post-checkout`, `post-commit`, `post-rewrite`, appended after existing Husky / pre-commit hooks, never replacing) | exact-timestamp branch switches, commits and rewrites; polling misses short branch visits | 60 s git poll (crates/capture/src/git.rs) | M; opt-in per repo from the Projects card, `chronicle hooks install` |
| `git reflog show --date=iso HEAD` | retroactive branch-switch times (prunable, ~90 days) | none | S; backfill and repair, not live |
| GitHub Desktop, GitKraken, Sourcetree, Tower, Fork | repo + branch in the title | title path (extract.rs:348) | S |
| lazygit, tig, gitui | terminal title unchanged | none | shell hook cwd |
| Graphite `gt` | stack and branch | none | M, later |

### Repo-local link files (free project identity)

Read on the git poll, names and ids only, never contents.

| File | Reveals |
|---|---|
| `.vercel/project.json`, `.netlify/state.json`, `fly.toml`, `render.yaml`, `railway.json`, `wrangler.toml`, `supabase/config.toml` | the deployed service this directory is; its dashboard URL patterns then map back to the project (`vercel.com/<team>/<project>`, `fly.io/apps/<name>`, `supabase.com/dashboard/project/<ref>`) |
| `.doppler.yaml`, `.sentryclirc`, `.circleci/config.yml`, `.buildkite/pipeline.yml` | project and environment names for secrets, errors and CI, each with a URL pattern |
| `bruno.json` + `*.bru`, `.insomnia/` | the API collection is the repo |
| `package.json`, `Cargo.toml`, `pom.xml`, `build.gradle`, `nx.json`, `turbo.json` | build system and monorepo shape, for the standup's vocabulary |
| `.idea/dataSources.xml` | database connection names tied to the project |

### VCS hosts and issue trackers

| Tool | Share | Signal | Today | Effort |
|---|---|---|---|---|
| GitHub | dominant | remote URL; `gh pr list --author @me`; PR page URL/title | `github_prs` (off by default) | remote parse S |
| GitLab | ~29 % | `glab mr list --assignee=@me`; MR URLs | none | S |
| Bitbucket, Azure DevOps | Atlassian and regulated shops | remote URL; REST with a token | none | remote parse S, PR lists M |
| Gitea / Forgejo | self-host niche | `tea pulls` | none | S, later |
| Jira | most orgs, losing share | `PROJ-123`; MCP (`mcp-atlassian`, wired in `mcp.toml`) | ticket regex, MCP context | done |
| Linear | fastest-growing | `ENG-123`, lowercase in branches; official remote MCP | regex hits uppercase only | branch case fix S; MCP via the generic client |
| GitHub / GitLab Issues | bundled | `#123`, `owner/repo#123`; `gh issue list --assignee @me` | GitHub URL form only | repo-scoped `#123` S |
| Azure Boards, Shortcut, YouTrack, Asana, Trello, ClickUp, Notion, Plane | tail | `AB#123`, `sc-123`, `PROJ-123`, or URL ids | regex where Jira-shaped; URL ids for some (extract.rs:1260) | leave to the regex and title matchers |

One PR-page URL regex family (`org/repo/(pull|merge_requests|pullrequests)/<n>`)
covers the five hosts. CI run pages
(`github.com/{o}/{r}/actions/runs/{id}`, `gitlab.com/{ns}/{p}/-/pipelines/{id}`,
CircleCI, Buildkite) confirm the project and mark the work kind "deploying".

### Observability, incidents, cloud consoles, API and network tools

These rarely name the project on their own. They name the **kind of
work**, which is a new anchor (`mode`: deploying, on-call, debugging prod,
testing an API, on the prod network) that the standup and the timeline
should carry ("2 h on contoso, of which 40 min on incident #312").

| Tool | Signal | Reveals | Effort |
|---|---|---|---|
| Sentry | `<org>.sentry.io/issues/<id>`, short id `BACKEND-42` in the title; `.sentryclirc` names the project | project (via the link file), issue id | S |
| Datadog, Grafana, New Relic, Honeycomb, Kibana, Splunk | dashboard / monitor ids in URL; query strings must be dropped (they carry search text and entity ids) | mode: debugging prod | S, title-only |
| PagerDuty, Opsgenie, incident.io | `<org>.pagerduty.com/incidents/<id>`; Slack `#inc-<n>-<slug>` channel names | mode: on-call, incident id | S |
| Argo CD, Octopus | app and environment in the URL; `argocd app get` | mode: deploying to <env> | M |
| AWS, GCP, Azure consoles | `?region=`, `?project=<id>`, `#@<tenant>/…/subscriptions/<id>`; `~/.aws/config` profiles, `gcloud config list`, `az account show` | account and project; mode: infra | S for URL, M for CLI identity |
| ngrok | unauthenticated local API `127.0.0.1:4040/api/tunnels` | which local service is exposed, and so its repo via the port | S |
| Tailscale | `tailscale status --json` | which network the person is on (prod VPN) | S |
| Postman, Bruno, Insomnia, Hoppscotch | collection name in the title; Bruno and synced Insomnia live in the repo | project via the repo; mode: testing an API | S |
| Wireshark, Charles, Proxyman, mitmproxy | app running | mode: debugging network | S |
| DBeaver, TablePlus, DataGrip, pgAdmin, psql | connection **names** from config files; never the strings | project via `.idea/dataSources.xml` or a name matcher; mode: database | S |
| LaunchDarkly, Doppler, Vault, 1Password CLI | `.doppler.yaml`, `op whoami` | project and environment | S |
| Figma, Storybook, Confluence, Notion, Gmail, Outlook | title only | mode: design, docs, mail | title matchers |

Privacy traps in this group: Bruno, Postman, Charles and Proxyman files
carry live keys and bodies; DBeaver, TablePlus and pgAdmin configs carry
passwords; Splunk, Kibana and New Relic URLs carry query text. The rule
throughout: read names, paths and ids, never contents or query strings,
and the redaction pass above enforces it before anything reaches a model.

### Chat, meetings, calendar, browser

| Tool | Signal | Today | Effort |
|---|---|---|---|
| Zoom, Meet, Teams, Slack huddles | mic in use is the only cross-app call signal; Zoom omits the topic from its title; Meet's tab title has it | `mic_capture` (PipeWire, Linux) | macOS CoreAudio process taps, Windows `CapabilityAccessManager` registry: M each with the port |
| Meeting name | calendar event overlapping the call | `google_calendar` (needs the user to create a Google Cloud OAuth client: a mass-market blocker) | ICS subscription URL (Google's secret iCal address, Outlook publish, Fastmail, any CalDAV): S, zero OAuth, the default route; EventKit on macOS: S |
| Slack, Discord, Teams, Mattermost | workspace / channel in the title, no stable local API; Slack has an official remote MCP | title only | keep title-only; `apps` matcher; MCP for context on demand |
| Browser URL and domain | `aw-watcher-web` posts URL + title to the AW-compatible server (Chromium; Firefox gives no URL); history DBs (`History`, `places.sqlite`, Safari `History.db`) give URL + time with no install | AW route (crates/server/src/lib.rs:348) | history DB read, copy-then-query, domain + path ids only, private windows absent by design: S; the zero-install default, the extension the precise option |

## Connections: how tools talk, and what Chronicle should offer

Ranked by how many tools each kind unlocks per unit of user effort. This
is the Settings › Connections taxonomy.

| Kind | Examples | Hosted endpoint? | Effort | Verdict |
|---|---|---|---|---|
| **Files on this machine** | git, transcripts, histories, link files, `.ics` | no | S | always on, zero-config, the largest tier |
| **Local servers and sockets** | WakaTime and ActivityWatch protocols, ngrok API, Docker socket, D-Bus lock, tmux | no | S | on when detected |
| **Your CLIs** | `gh auth token`, `glab auth token`, `gcloud auth application-default print-access-token`, `az account get-access-token`, `aws sso` cache, `vercel`, `op`, `tailscale` | no | S | auto-detect and offer "use your existing login"; read scopes only |
| **Accounts (OAuth loopback)** | Google Calendar today; Slack, Notion, Figma if ever needed | no | M | only where no CLI or MCP exists |
| **MCP servers** | official remote servers with OAuth 2.1: GitHub, GitLab, Atlassian, Linear (read-only URL variant), Sentry, Slack, Notion, Grafana, Datadog, PagerDuty, Cloudflare, Vercel, Supabase, Google Workspace, Figma | no | M once | one generic client (URL + OAuth) replaces bespoke REST per vendor; Chronicle already has the client (`mcp.toml`) |
| **Tokens** | PATs and API keys | no | S | the fallback |
| Webhooks, gRPC, OTLP | GitHub, Stripe, collectors | yes | L | skip; needs a public endpoint or a relay, only if a hosted tier exists |

Two notes from the survey. MCP went stateless in the 2026-07-28 spec and
the registry holds ~9.6 k canonical servers, so a generic client is
durable. The Messages API MCP connector (`mcp_toolset`) reaches remote
servers with a caller-supplied token and is billed per call, which suits
chat and the nightly standup ("what changed on my tickets"), not periodic
polling; Chronicle's own client does the polling.

## Platforms and distribution

| Platform | Share of professional devs | Capture route | Effort | Blocker |
|---|---|---|---|---|
| Windows | ~50 % (WSL ~17 % of all respondents) | `SetWinEventHook`, `GetLastInputInfo` | M | elevated windows invisible without a service (UIPI); WSL2 titles carry no cwd unless the shell emits OSC 9;9: the shell hook fixes that |
| macOS | ~33 % | AX titles (no Screen Recording), `CGEventSource` idle | M | Developer ID signing + notarization ($99/yr) is mandatory: Homebrew 5 disables casks that fail Gatekeeper since 2026-09-01, and an ad-hoc signature loses the Accessibility grant on every rebuild |
| Linux Wayland, wlroots (Sway, Hyprland, Niri) | Wayland is 60–80 % of Linux sessions | `wlr-foreign-toplevel-management`, `ext-idle-notify-v1` | S | older distro builds lack ext-idle-notify |
| Linux Wayland, KDE | | KWin script over D-Bus (as `awatcher`, `kdotool`) | S–M | scripting surface, not a protocol |
| Linux Wayland, GNOME | largest Linux DE | a Shell extension the user installs | M | the only route needing user action; `ext-foreign-toplevel-list-v1` not adopted by GNOME or KDE yet |
| Linux X11 | shrinking | done | | |

Order: macOS, then wlroots + KDE, then Windows, then GNOME. Distribution:
cargo-dist builds GitHub Releases and from them a Homebrew formula with a
`service` block, winget and Scoop manifests, an AUR PKGBUILD, .deb and .rpm,
and a checksummed `curl | sh`. Azure Trusted Signing (~$10/mo) is the
Windows minimum. Model download stays on first use. Flatpak is out: its
sandbox blocks window capture.

## Positioning

Accuracy first; trust as the supporting claim.

- **The most accurate account of your day.** From your commits, sessions,
  editor, shell, deploys and tickets: more of what your tools already write
  than anything else reads. A frontier model reasons over the ambiguous
  parts and shows the evidence for every line it writes.
- **A standup you can defend.** Every claim carries the commit, session,
  note or ticket it came from. Rebases do not lose time.
- **Yours.** One SQLite file, open source, no seats, shareable by choice.
  Nothing leaves without a backend you chose, and the Settings line says
  exactly what did. Never a scorecard for someone else (LinearB, Swarmia
  and DX are radioactive to individual developers).

The site hero ("Your day, written down") is generic. A developer hero:
"The most accurate account of your day. From your commits, sessions,
windows and tools." The proof block carries the self-score numbers:
placement precision against your own corrections, unfiled minutes,
cross-project placements.

## Milestones after M35

- **M36 Accuracy** (`m36-accuracy-plan.md`). The `claude_code` backend and
  the M31 gate on this box; redaction pass; prefix split
  and cache metrics; corrections embeddings; the pairwise advisor behind
  the bench; evidence-id enforcement on standup and descriptions; the
  nightly Batches pass; the six accuracy metrics on the self-score and in
  `chronicle status`. Gate: placement precision and recall on the replay
  beat the local baseline; standup faithfulness 100 %; cost per day under
  the cap.
- **M37 Sources and connections.** Session formats (Codex, Gemini, Copilot
  CLI, Aider, Cline, Amp, OpenCode, Cursor last), `chronicle shell-init`,
  tmux, Docker Compose labels, VS Code recent workspaces and remote URIs,
  JetBrains `recentProjects.xml`, git remote and worktrees if not in M35,
  git hooks opt-in, repo-local link files, the `mode` anchor, `glab`,
  browser history DBs, ICS calendars, the Connections taxonomy with CLI
  credential reuse and the generic MCP client. Gate: unfiled minutes below
  10 % on this machine with no config edit beyond the six repos; every
  source visible in the per-project Sources row.
- **M38 macOS.** AX capture, idle, lock, tray, LaunchAgent, `lsof` ports,
  CoreAudio mic, EventKit calendar, signing and notarization, Homebrew
  formula. Gate: a clean Mac reaches Home with projects populated from the
  repos under `~/dev` in under five minutes from `brew install`.
- **M39 Wayland.** wlroots and KDE behind the `FocusProvider` trait,
  `ext-idle-notify-v1`, logind lock as now. GNOME extension guided-install
  later.
- **M40 Windows.** `SetWinEventHook`, `GetLastInputInfo`, WTS lock, tray,
  `Run` key, winget and Scoop, Trusted Signing; disclosed UIPI and WSL2
  limits, with the shell hook as the WSL2 answer.
- **Cross-cutting.** cargo-dist from M38 on. M36 and M37 can interleave:
  the M30 thesis still holds that evidence shape caps accuracy, so each new
  source is an accuracy win too. The tray mockups for the site wait until
  M38 ships a real tray to screenshot.

## Housekeeping from this session

- The eleven untracked dotfile-named entries in the repo root
  (`.bashrc`, `.zshrc`, `.gitconfig`, `.idea`, `.vscode`, …) are read-only
  devtmpfs bind mounts the Claude Code tool sandbox places in the cwd to
  mask home dotfile names. They are not files, exist only inside the
  sandbox's mount namespace, and are now listed in `.git/info/exclude`.
  Nothing to commit.
- Open derived duplicates as of 10:00: 149 and 150 duplicate declared 143,
  141 duplicates 144, and new this morning, 152 (`chronicle@main ·
  <session id>`) beside declared 145. `chronicle task` has only `list` and
  `rename`; merge is Home or the 3-day autoclose. Two `name_task` jobs still
  point at a deleted task.
- M34: main is 3 commits ahead of origin, unpushed; the Render check waits
  on the push.

## Sources

Stack Overflow Developer Survey 2025; JetBrains State of Developer
Ecosystem 2025; WakaTime plugin list; Zed workspace persistence; Cline
storage layout; "Where AI coding CLIs store session logs"; GitHub Copilot
CLI session docs; Gemini CLI session management; OpenCode storage; AI
coding market share 2026 (baeseokjae.github.io); Atuin; WezTerm shell
integration; tmux issue #3064; fish discussion #9514; Warp file locations;
wakatime/vscode-wakatime #244; Docker Compose `working_dir` label;
microsoft/vscode #179882; fosspost git market share; tech-insider Linear
vs Jira 2026; sooperset/mcp-atlassian; ActivityWatch aw-watcher-web;
dongdongbh/mindwtr #203 (EventKit); 2e3s/awatcher; GNOME extension 5592;
Homebrew 5.0 release notes; Azure Trusted Signing; cargo-dist; G2 WakaTime
reviews; muety/wakapi #864; arXiv 2411.07479; git-time-metric/gtm #103;
kamranahmedse/git-standup #119; jazzband/Watson #504; HN threads on Rewind
(37906969), Screenpipe (41695840), DX (49033522); GeekWire on Recall 2026;
Swarmia and GitClear on developer metrics; platform.claude.com MCP
connector docs; blog.modelcontextprotocol.io 2026-07-28; The Register on
stateless MCP; digitalapplied MCP adoption 2026; cli.github.com `gh auth
token`; hookdeck on tunnels; tembo.io MCP servers; rize.io; memtime.com;
dayflow.so; timely.com/memory; screenpipe.com; linear.app/changelog;
reclaim.ai and usemotion.com pricing; claude.com prompt caching and
Message Batches posts; eugeneyan.com LLM patterns; hamel.dev evals;
huyenchip.com AI engineering pitfalls; OpenAI cookbook classification via
embeddings; Sentry, Datadog, Grafana, PagerDuty, incident.io URL docs;
ngrok local API; Tailscale CLI; Bruno, Insomnia, Postman docs; Vercel,
Netlify, Fly, Railway, Supabase, Doppler project link files; JetBrains
2025 CI adoption; Synergy Research cloud share.
