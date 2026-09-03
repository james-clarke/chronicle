# M22 — Activity events + local collectors


## Context

Timeline evidence today is git-only: `vcs_events` (migration 007) holds checkout/commit point markers, consumed by anchoring (`crates/core/src/anchor.rs:20-25`), the digest "## Git activity" section (`crates/core/src/digest.rs:129-155`), the timeline Activity rows (`crates/app/src/ui/timeline.rs:1085-1092`, `activity_row` `:1400-1416`) and Settings › Connections repo rows (`crates/app/src/ui/connections.rs:400-405`). MCP context can only land as digest text (`MAX_CONTEXT_CHARS`), never as rows. The post-m21 research (progress.md "Post-m21 integration research") picked three no-account local sources as the highest-signal next step: Claude Code sessions, GitHub PR events via `gh`, mic-in-use as "in a call". This milestone generalizes the table into `activity_events` with a `kind` column and adds those three collectors, reusing the existing row/digest/prune plumbing.

Verified on this box (2026-09-02): `~/.claude/projects/` has 21 project dirs, main-session `<uuid>.jsonl` files (37 in the chronicle dir) plus `<uuid>/subagents/*.jsonl`; user/assistant lines carry `timestamp` (ISO 8601 Z), `cwd`, `gitBranch`, `sessionId`, `isSidechain`; other line types (`last-prompt`, `mode`, `permission-mode`, `attachment`…) lack `timestamp`. `gh` authed (keyring, scopes gist/read:org/repo); `gh search prs --author=@me --updated='>=…' --json number,title,url,state,updatedAt,repository` returns `repository.nameWithOwner`; titles carry ACME keys. `pw-dump`, `pw-cli`, `pactl` all present; 0 `Stream/Input/Audio` nodes while idle (expected). `~/.codex/sessions` absent.

## Design decisions

### Schema: rename, don't rebuild (migration `010_activity_events.sql`)

```sql
ALTER TABLE vcs_events RENAME TO activity_events;
ALTER TABLE activity_events RENAME COLUMN commit_id TO ext_id;
ALTER TABLE activity_events ADD COLUMN end_ts INTEGER;   -- span-like kinds only
-- (idx_vcs_events_ts follows the rename; keep the name, it's internal)
CREATE UNIQUE INDEX idx_activity_ext ON activity_events (kind, ext_id, ts) WHERE ext_id IS NOT NULL;
```

Column meanings by kind (`repo` stays "short scope name", `branch` stays NOT NULL, `''` when unknown):

| kind | ts / end_ts | repo | branch | ext_id | summary |
|---|---|---|---|---|---|
| `checkout` / `commit` | as today | repo basename | branch | commit hash | subject |
| `ai_session` | first / latest user-or-assistant line ts | basename(`cwd`) | `gitBranch` | `sessionId` | first user prompt, clipped 120 chars |
| `pr_authored` / `pr_reviewed` | `updatedAt` / NULL | repo name (after `/`) | `''` | PR url | `#N title · state` |
| `call` | mic stream appeared / disappeared | `''` | `''` | `call:<start ts>` | `application.name` |

Dedupe is per kind (`ActivityKind::dedupe()`): checkout = existing "same branch as last checkout" rule; commit = none; `ai_session`/`call` = upsert on `(kind, ext_id)` updating `end_ts` (and `summary` when the stored one is empty); `pr_*` = `INSERT OR IGNORE` on `(kind, ext_id, ts)` so each `updatedAt` bump is one marker and re-polls are free. Unique index gives the `pr_*` and upsert paths their conflict target.

The existing unique-checkout dedupe and the `git.rs` poller are untouched.

### Types (`crates/core/src/types.rs:22-57`)

`VcsEvent` → `ActivityEvent { ts, end_ts: Option<Timestamp>, repo, branch, kind: ActivityKind, ext_id: Option<String>, summary: Option<String> }`; `VcsKind` → `ActivityKind { Checkout, Commit, AiSession, PrAuthored, PrReviewed, Call }` with `as_str`/`parse` (stored as the strings above) and `is_vcs()`. `CaptureEvent::Vcs` → `CaptureEvent::Activity`. Mechanical rename across core/capture/app/tests (sed-able: `VcsEvent`, `VcsKind`, `CaptureEvent::Vcs`, `commit_id`).

### Storage (`crates/core/src/storage.rs`)

- `insert_vcs_event` → `insert_activity_event` with the per-kind dedupe above; `VCS_COLS`/`vcs_from_row` gain `end_ts`, `ext_id`.
- Keep `vcs_in_range` and `branch_state_before` (anchoring) and `latest_vcs_event_per_repo` (Connections) but add `AND kind IN ('checkout','commit')` so non-git rows never reach `anchor_tasks` or the repo rows. `commits_for_task` unchanged apart from the table name.
- `commits_in_range` (`storage.rs:182-212`) → `activity_in_range_by_task(lo, hi) -> Vec<(i64, ActivityEvent)>`: overlap join uses `COALESCE(end_ts, ts)` so a session/call overlaps every interval it spans; keep commit-only behaviour for `checkout` (excluded, as today).
- New `activity_in_range(lo, hi)` for the digest (replaces `vcs_in_range` there) so journals see sessions/PRs/calls.
- `prune` STMTS (`storage.rs:577`): table rename.
- Tests: update `latest_vcs_event_per_repo_picks_newest_of_each` (`storage.rs:1954-1979`) + golden `vcs_events_store_dedupe_and_anchor_guard` (`crates/core/tests/golden.rs:684`), add one test each for the upsert path (`ai_session` end_ts grows) and the `pr_*` ignore path.

### Digest (`crates/core/src/digest.rs:129-155`)

Section becomes `## Activity`, last 10 events in window, one line per kind:
`- 14:02 claude chronicle@main 23m "first prompt…"`, `- 15:30 PR authored acme-ai-agent-backend #40 "ACME-11342: …" (open)`, `- 11:00 call 32m (Firefox)`, git lines unchanged. Golden `digest_git_activity_section` (`golden.rs:635-664`) updated, one new golden for the mixed section.

### Collectors (`crates/capture/src/`, one file each, same `FocusProvider::run(self, tx)` shape as `git.rs:62-95`, own thread, never load-bearing)

1. **`ai_sessions.rs`** — poll 20 s over `config.ai_session_dirs` (default `["~/.claude/projects"]`, empty = off). Only top-level `<dir>/<project>/*.jsonl` (subagent files live under `<uuid>/subagents/`, skipped by not recursing). Per-file state `{len, first_ts, last_ts, sent}`; on first sight read the head until the first line with `timestamp` + `cwd` (start, repo, branch, sessionId, prompt) and the tail (last 64 KB) for the latest timestamp; afterwards read only bytes past the stored `len` and take the max `timestamp` seen, skipping lines with `isSidechain: true` or without `timestamp`. Boot scan limited to files with mtime in the last 24 h. Emits `ai_session` (upsert path) whenever `last_ts` moves. Timestamps parsed with `jiff` (already a dep) from RFC 3339.
2. **`github.rs`** — poll 300 s when `config.github_prs = true` (default false; opt-in because it needs `gh auth`). Two calls per poll: `gh search prs --author=@me` and `--reviewed-by=@me`, both `--updated='>=<yesterday>' --json number,title,url,state,updatedAt,repository --limit 30`. `gh` resolved to an absolute path once via the same PATH walk as `resolve_command` (`crates/app/src/ui/connections.rs:84`) — move that helper to `chronicle_core::config` next to `expand_home` so daemon and UI share it (systemd PATH is minimal). Non-zero exit → `tracing::warn!` once per distinct stderr line, keep polling. Emits `pr_authored`/`pr_reviewed` rows (ignore path). ≤ 2 req / 5 min against a 30/min search limit.
3. **`mic.rs`** (Linux only, cfg-gated like the X11 threads in `spawn_capture` `main.rs:1894-1917`) — poll 20 s when `config.mic_capture = true` (default true): run `pw-dump`, parse with `serde_json::Value`, collect nodes whose `info.props["media.class"] == "Stream/Input/Audio"` and their `application.name`. none→some: emit `call` with `ts = now`, `end_ts = None`; some→none: re-emit the same `ext_id` with `end_ts = now` (upsert fills it). If `pw-dump` is missing, log once and exit the thread.

Daemon wiring (`crates/app/src/main.rs`): `spawn_capture` gains `spawn_ai_sessions_capture`, `spawn_github_capture`, `spawn_mic_capture` next to `spawn_git_capture` (`main.rs:1921-1945`), same thread-name + `tracing::error!` on exit pattern. Config (`crates/core/src/config.rs:17-65`, `deny_unknown_fields` + `serde(default)`): add `ai_session_dirs: Vec<String>`, `github_prs: bool`, `mic_capture: bool` with the defaults above; README config table updated.

### UI (`crates/app/src/ui/`)

- `CommitRow` (`mod.rs:239-244`) → `ActivityRow { time, kind, summary, duration: Option<Duration> }`; loader at `mod.rs:918-937` switches to `activity_in_range_by_task`; `TaskGroup.commits` → `activity`.
- `timeline.rs:1085-1092` and `:1366`: glyph by kind — `GIT_COMMIT` (existing), plus three new Phosphor glyphs (`TERMINAL_WINDOW` for sessions, `GIT_PULL_REQUEST`, `PHONE`). Requires regenerating `crates/app/assets/fonts/Phosphor-subset.ttf` with the `pyftsubset` recipe in the comment at `theme.rs:701` (adds three codepoints to `--unicodes`); constants go in `theme::icon` (`theme.rs:704+`). `activity_row` gets an optional duration column ("23m") after the time.
- Settings › Connections: no new section this milestone; the existing passive line pattern (`connections.rs`, newest `fetch_context` job) is the template for a later "Local sources" block (roadmap).

### Out of scope (roadmap in the plan doc)

PR-title ticket keys as anchor evidence (`anchor_tasks` today keys only on branches); splitting one long Claude session into gap-separated segments; showing calls that overlap no task (AFK gaps) on the activity band; Settings toggles for the three collectors; Codex/other agent dirs; Wakapi heartbeats; atuin; the m21.5 presets.

## Build order (each chunk: tests + clippy, own commit)

1. **core** — migration 010, type rename, storage (dedupe/upsert/range fns, prune), anchor + Connections filters, digest section, golden/unit test updates. Everything compiles with git as the only producer; behaviour identical.
2. **capture: ai_sessions** — collector + config field + daemon spawn + README config row. Unit test for the incremental line parser over a fixture jsonl (three line types, one sidechain line).
3. **capture: github** — collector + `github_prs` + shared `resolve_command` move + spawn. Unit test for the `gh` JSON → rows mapping over a fixture.
4. **capture: mic** — collector + `mic_capture` + spawn. Unit test for the `pw-dump` node filter over a fixture.
5. **app UI + docs** — `ActivityRow`, timeline glyphs + duration, Phosphor subset regen, README milestones (add M22), progress.md Status entry, `m22-collectors-plan.md` committed with chunk 1.

## Verification

- `cargo test --workspace` and `cargo clippy --workspace --all-targets` after every chunk (autonomy per memory: tests/builds allowed in this repo).
- Migration: copy the live DB to the scratchpad, run the debug binary against it once, confirm `activity_events` row count equals the old `vcs_events` count and `tasks.external_ref` survives.
- Sandbox daemon (stop the systemd unit first, per `chronicle-no-rebuild-under-daemon` memory): with `CHRONICLE_UI_VIEW=timeline` visual loop, confirm (a) this Claude session shows as a row under the chronicle task within 20 s with a growing duration, (b) with `github_prs = true` the ACME-11342 PR #40 appears as `pr_authored` under whatever task overlaps 15:30, (c) start a Meet/`arecord -d 5` → `call` row with `end_ts` filled after stop.
- Digest: `chronicle derive-now` (or the DeriveNow ctrl message) over today's batch, inspect the `## Activity` section in the derive log.
- Deploy: stop unit → `cargo install --locked` → start; check `journalctl --user -u chronicle` for the three new thread names starting and no `provider exited` errors.
