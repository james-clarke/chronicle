# Data and privacy

Chronicle keeps everything it records in one SQLite file on your machine. This page lists what each table holds, what leaves the machine and when, and how to prune, export or delete what is stored.

## Where it lives

Everything sits under one data directory, resolved per platform:

| Platform | Path |
|---|---|
| Linux | `$XDG_DATA_HOME/chronicle`, usually `~/.local/share/chronicle` |
| macOS | `~/Library/Application Support/chronicle` |
| Windows | `%APPDATA%\chronicle` |

Inside that directory:

| File | Contents |
|---|---|
| `chronicle.db` (+ `-wal`, `-shm`) | the single SQLite database, WAL mode |
| `chronicle.log` (+ `chronicle.log.1`) | a 5 MB rotating log, one backup kept |
| `config.toml` | the settings, see [Configuration](configuration.md) |
| `mcp.toml` | MCP servers and allowlisted calls, file mode 0600; absent means MCP is off |
| `models.toml` | cloud model backends and API keys, file mode 0600; absent means no cloud backend is configured |
| `google.toml` | the Google Calendar OAuth client and refresh token, file mode 0600; only present after `chronicle gcal-login` |

## Tables

All timestamps are UTC unix milliseconds. The tables fall into five groups, with the smaller ones listed at the end.

### Capture

Raw and near-raw material from what you look at and do, written before any model sees it.

- **events**: every raw focus change, title change, AFK edge and browser tab URL, one row per event. Holds window titles and URLs verbatim (after exclusions run), the app name and the process id.
- **spans**: events merged into stretches of one app and title (or context switching, or AFK), with idle time folded into the stretch and the project it was filed into. Same personal content as events: titles and URLs.
- **activity_events**: everything the evidence collectors see that is not focus time: git checkouts and commits, AI coding sessions, browser history visits, calendar meetings, shell working directories, pull request activity, notes and calls. One row per marker, with kind-specific JSON detail (a commit subject, a meeting's attendees, a session's touched file paths, a note's body). This is the table with the widest range of personal content. [Sources](sources.md) says what each collector keeps out of it.
- **presence**: per-minute counts of keys, mouse buttons, motion and scroll events. Counts only, never which keys. Not on Wayland, which has no protocol for it.
- **daemon_runs**: one row per stretch the daemon was capturing, with the reason a stretch ended (shutdown, provider exit, lock, sleep, crash). No personal content. Reports use it to explain capture gaps.

### Derivation

The tables that turn raw spans into what you were doing.

- **batches**: 30-minute windows of non-AFK activity, the unit sent to the model, with timing and token counts per run.
- **tasks**: what work gets filed under, each with a label, an optional project, open or closed, declared by you or derived by the model, a ticket reference, a description, and whether it is the current task in its project. Labels and descriptions usually come from window titles but can be anything you or the model wrote.
- **intervals**: the time slices assigned to a task, with start and end, confidence, who placed it (model, you, pre-pass, segmenter), a one-line reason for a rule placement, and a share when two tasks split one stretch, such as two AI sessions writing at once.
- **corrections**: every rename, reassignment or merge you make by hand. Kept forever, because they teach the next derivation. Each carries a searchable snapshot of the titles and apps around it.
- **proposals**: unassigned runs of activity that share distinctive title words or a repo, clustered into a card the feed shows until you accept or dismiss it, with the time ranges behind the cluster.

### Evidence

Typed evidence attached to spans and tasks, used to place time without asking a model and to back anything written in prose.

- **span_anchors**: one row per span, evidence kind and value: a ticket key, a git branch, a tool session id, a calendar event, a document, a place, a person, a domain. Ticket keys, branch names and domains live here outside the raw title text.
- **task_evidence**: the same evidence keys rolled up per task, with decayed minutes summed across sources, so placement can score a task against its own history.
- **claims**: the evidence behind each piece of model-written prose (a task description, a journal entry, a narrative, a standup draft), so a claim in that text can be traced to the interval or anchor it rests on.

### Model-written

Text a local or cloud model produced, plus the job queue and caches behind it.

- **ai_jobs**: the queue for every model job (task descriptions, task naming, narratives, journal entries, checkpoints, chat, advice), with kind, status, priority, which backend ran it, token counts and cost.
- **narratives**: cached prose summaries of a date range, keyed by the range and a hash of the underlying numbers, so reopening a report does not re-run inference.
- **journal_entries**: one append-only entry per task per batch, written as work happens, with the interval ids behind it.
- **embeddings** (`span_embeddings`, `task_embeddings`, `task_label_embeddings`, `correction_embeddings`): vectors computed from a span's title, a task's label or a correction's context, used to find similar past work. Only populated when `embed_model` is set. A vector is derived from the text but is not a copy of it.

### Measurement

How well derivation is doing, computed from the tables above rather than captured directly.

- **self_score**: one row per local day: active, uncaptured, underived and placed milliseconds, tasks minted and merged, placement and correction counts, and the segmenter's confident and wrong counts. Counts and milliseconds only, no titles.
- **backend_score**: the same daily rollup per model backend, local or cloud: jobs run, claims made and resolved, advisor questions asked, changed placements later confirmed or undone, tokens and cost.

### Other tables

| Table | Holds |
|---|---|
| `conversations`, `chat_messages` | the chat feature's threads and messages, general or scoped to one task |
| `checkpoints` | the latest where-I-am and next-steps text per task, overwritten each time |
| `task_context` | the last MCP fetch for a task's external ref, replaced on re-fetch |
| `standup_drafts` | one drafted standup per civil day, built from that day's journal entries |
| `verdict_log` | the segmenter's placement verdicts (winner, runner-up, margin, outcome, and any cloud advisor's answer) |
| `repo_links` | names and ids read from a repo's own deploy files (`.vercel/project.json`, `fly.toml`, `.sentryclirc` and similar), never file contents |
| `editor_workspaces` | recently opened folder paths and remote URIs read from editors' own state, for resolving a window title to a repo |
| `meta` | daemon status flags and small cached values (an API key for the heartbeat endpoint, the last prune timestamp, bench scores) |
| `spans_fts`, `tasks_fts`, `corrections_fts` | full-text search indexes mirroring `spans.title`, `tasks.label` and `corrections.ctx` |

## Exclusions

`excluded_apps` and `excluded_titles` in `config.toml` are lists of regexes, compiled once at daemon start and checked against every focus, title change and URL event: the app name, the window title, and the URL for browser tab events. A match drops the event before it reaches storage. It is never written to the database and never reaches the sessionizer or a model.

This filter covers window capture only. It does not run against `activity_events`: a commit subject, an AI session's first prompt, a calendar title or a browser history row is not matched against these patterns. Those collectors have their own, narrower rules about what they keep, listed per source in [Sources](sources.md).

The patterns themselves are stored nowhere but `config.toml`. They are not logged and not sent anywhere.

## Redaction

Separately from exclusions, every request to a cloud model backend passes through a redaction pass first, on every field of the request: the system prompt, the user prompt and both sides of any history. It replaces, in order:

- JWTs, AWS access keys, `sk-` style API keys, GitHub tokens and bearer tokens with a class marker such as `[jwt]` or `[api key]`
- credentials embedded in a URL (`user:pass@host`) with `[credentials]`
- a URL's query string and fragment (everything after `?` or `#`)
- an opaque path segment (20 or more hex or mixed-case-with-digits characters) with `[token]`; ordinary slugs, branch names and ticket keys are left alone

Settings shows which classes fired, never the secret itself. Command lines never need this pass, because only program names ever reach the digest.

## Retention

`retention_days` (default 180, 0 keeps everything) drives a daily prune that runs in the daemon's idle window in batches of 1000 rows. It deletes rows older than the cutoff from `events`, `activity_events`, `spans` and their orphaned anchors, `intervals`, `chat_messages`, `conversations`, finished `ai_jobs` and `narratives`, then derived `tasks` and `batches` once nothing references them.

Never pruned, whatever `retention_days` says: `corrections`, the evidence and measurement tables (`task_evidence`, `claims`, `self_score`, `backend_score`, `verdict_log`), `presence`, `daemon_runs`, the embedding tables, `checkpoints`, `task_context`, `standup_drafts`, `repo_links` and `editor_workspaces`. A declared task, and any task or interval a correction references, is kept at any age.

`task_autoclose_days` (default 3, 0 turns it off) closes derived tasks nothing has touched. It closes them; it deletes nothing.

## What leaves the machine

Nothing leaves by default. Everything below runs only once you turn it on and give it credentials of your own.

- **Cloud model backends** (`models.toml`): a backend entry (an Anthropic API key, or your own Claude Code login) plus a route for a job kind sends that job's prompt, system and user and history, to that provider after the redaction pass above. Any job kind can be routed: chat, narratives, standups, journal entries, task descriptions, checkpoints, task naming, the segmenter's advisor, and, if you route them, the derive, live and consolidate tiers themselves. A daily spend cap (`max_usd_per_day`, default 2 USD) sends jobs back to the local model once reached. Nothing is sent until a route names a backend.
- **MCP servers** (`mcp.toml`): context calls run at derive time with a 10 s timeout each and inject about 500 tokens of the response into the digest. Fetch calls run once per task with its ticket key substituted in, with a budget of about 6 000 characters. Action calls are writes you trigger by hand from the task pane, such as posting a journal entry to a ticket, and never run on a schedule. All three go to servers you list yourself, and everything a server returns is treated as untrusted text to label time with, never as instructions.
- **Google Calendar** (`chronicle gcal-login`): an OAuth desktop flow with the events read-only scope. The primary calendar is polled every 5 minutes, and only the event id, title, start and end are stored. Nothing is sent to Google beyond the read.
- **GitHub and GitLab** (`github_prs`, `gitlab_mrs`): every 5 minutes, `gh search prs` or `glab mr list` under your existing CLI login. Chronicle stores no token of its own and sends nothing beyond the search the CLI makes.
- **ICS calendars** (`calendars`): a GET of each URL you configure every 15 minutes. Nothing goes back the other way.

Everything else (window capture, git polling, shell, tmux and Docker working directories, browser history, AI session transcripts, editor heartbeats, the local HTTP endpoint, notes) reads and writes only the local database and makes no outbound network call.

## Export and delete

- `chronicle report` prints a timesheet for a day or a week as CSV or Markdown: project, task and hours, not raw titles or URLs.
- `chronicle dump` prints row counts and the raw events (timestamps, app, title, URL) for a day or for everything.
- Deleting the data directory removes everything Chronicle has ever stored. There is no wipe command; stop the daemon and remove the directory.
- Exclusions keep something out of the database from now on. They do not remove rows already written.
