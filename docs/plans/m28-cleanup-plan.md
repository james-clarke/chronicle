# m28 — clean up and optimize

Status: **phases A–C landed 2026-09-03** (ba4ba22, 2bc2652, 83138ad, f990c64, cfcda93, d1f6166, 3e9d0d9); deploy + review notes under Shipped. Asked for as one milestone: close every loose end
from m21–m27, put the repo in order (docs, organization, sync), then review the whole
codebase and apply the optimizations that survive review. Run to completion in parallel
under one integrator; each phase lands as its own commit(s) with tests + clippy green.

## Phase A — loose ends and repo order

1. Record the m27 replay numbers (4B baseline `76/180`; post-chunk-5 run) in
   `m27-derivation-plan.md` Shipped, flip its status line (chunks 1–7 shipped, not
   "chunk 1 shipped"), and mark m26's "merge into main pending" as done (81b5c23).
2. Remove the `m26` worktree (`../chronicle-m26`) and branch — merged into main.
3. Docs sync: README milestones table gains m26/m27/m28 rows and drops the stale "M0–M14
   complete" header; plan docs move under `docs/plans/`, direction docs under `docs/`,
   `workflow.md` stays at the root next to README (it is the dev entry point). Every
   reference in README/plan docs follows the move.
4. `progress.md` gets the m27 chunks 2–7 + merge entry it is missing.
5. Hand-test items only James can do stay listed under "Open for James".

## Phase B — codebase review (read-only)

Five lenses, one pass each, findings as `file:line · severity · confidence · what · fix`:

- `crates/core` (storage.rs 4056 lines, digest, replay, prepass, evidence, sessionizer).
- `crates/app` main.rs (4510 lines: daemon, scheduler, CLI) — structure, duplicated
  spawn/gate code, blocking calls on the async runtime.
- `crates/app/src/ui` (mod.rs 2286, timeline 2150, connections 1362, home 1297) — per-frame
  allocations, repeated DB reads inside the paint loop, dead state.
- `crates/derive`, `crates/mcp` — model session lifecycle, prompt fitting, retries.
- `crates/server`, `crates/capture` + cross-cutting: unused deps, feature flags, build
  profile, CI, clippy pedantic candidates worth turning on.

Cut line: only findings with a concrete failure or a measurable win are applied. Pure
style/rename findings are dropped unless they ride along with a functional change.

## Phase C — apply

Group accepted findings by crate, one commit per group, `cargo test --workspace` +
`cargo clippy --workspace --all-targets -- -D warnings` + `cargo fmt --check` before each.
Large move-only splits (main.rs, storage.rs) are separate `refactor:` commits with no
behaviour change so `git blame -w -M` still follows.

## Shipped

### Phase A — loose ends (2026-09-03, ba4ba22, 2bc2652)

m26 worktree and branch removed; plan/direction docs moved to `docs/plans/` with README references following; README milestones table gained rows 26–28 and lost the "M0–M14 complete" header; m26/m27 status lines flipped to shipped; the 4B replay baseline (76/180) recorded; CI skips doc-only pushes. Post-chunk-5 replay finished later at 52/171 — recorded in the m27 plan as an open regression (title-key rule magnet), not fixed here.

### Phase B — review (2026-09-03, no code)

Five read-only review passes, ~110 findings. Dropped at the cut line: MCP server reuse across derives (2 s per 35-min batch, not worth a process-global runtime), describe-session prefix reuse (backfill only), `Config::validate`, moving `proposals` SQL into `storage.rs`, `prepare(&format!)` constants, card/frame chrome consolidation, grammar re-parse per generate. Correction to the brief: the daemon is a crossbeam `select!` loop on OS threads; tokio only hosts the tray, so the "blocking on the async runtime" class did not exist.

### Phase C — applied (2026-09-03)

- 83138ad capture/server: `poll_loop` shared by ai_sessions/github/gcal/mic (git and shell keep their own loops: they sleep before the first poll and carry per-item state); mic survives a failed `pw-dump` and keeps its open call span; git keeps last-good state on a failed read (no spurious checkout); `buckets` lock recovers from poisoning like `edits`; hostname read once into `AppState`; dead `with_endpoints`; orphaned `derive_v1–v4.txt`, `task_output.gbnf`, `task_output_v3.gbnf` removed.
- f990c64 core: pre-pass calls `anchor_tasks` once per tick and reads task recency from one `task_recency` map (updated in place as placements land, so later runs still see earlier ones); `repos_active_in_many` for proposals; `distinctive_fts_query` is one `UNION ALL` instead of up to 32 round-trips; `standup_digest` left-joins checkpoints; `build_digest` aggregates once and only re-renders per ladder rung (goldens byte-identical); `hint_lines` index map; migration 015 adds `chat_messages(conversation_id, id)`, `corrections(task_id)`, `intervals(start_ts, end_ts)`; dead `distraction_ms`; `truncate_chars`/`clip` shared. Not merged: `correction_hints` cannot be lazy (the fallback rule always reads it) and `chat::fmt_dur` differs from `digest::fmt_dur` (drops seconds). Root `[profile.release]`: thin LTO, one codegen unit, stripped symbols, no `panic = "abort"` (pollers rely on a panicking thread dying alone); `tracing` dropped from core.
- cfcda93 derive/mcp: `backend.rs` holds the model load, thread count, `common_prefix`, and chunked `decode_prompt` that runner/chat/describe each had; `ChatSession` keeps one context and its KV cache across turns (only the suffix after the common prefix is decoded; falls back to a fresh decode past `N_CTX`) — `ChatModel::answer` stays as a wrapper; MCP `gather` gives each call an even share of `max_chars` with carry-over so a verbose first server no longer starves the rest; dead `infer_intervals`; `fit_prompt` arithmetic in `u128`.
- d1f6166 app: `main.rs` 4510 → 542 lines; `daemon.rs` (scheduler, resident worker, tray, ctrl socket), `capture.rs` (providers, OAuth, AFK), `derive.rs` (worker protocol, batch/live/day derives, digest builders), `ai_job.rs`, `chat_worker.rs`, `bench.rs`, `status.rs`; `pub(crate)` re-exports in `main.rs` keep `ui/` paths unchanged. Dedups: `Scheduler::drop_resident`, `spawn_provider_thread`, `task_label_project`, `dispatch`'s progress ctor, one `CtrlCmd → CtrlMsg` map. Chat worker holds one `ChatSession` for its stdin loop.
- 3e9d0d9 ui: `list_conversations` no longer runs on every streamed-token repaint (history cached, refreshed on the 5 s reload and on delete); `merge_candidates` cached per reload; `Connections` probes (`resolve_git_dir`, `gh`/`pw-dump` on PATH, atuin db, session dir) computed in `load` and after add/remove; Home filter passes skipped when the query is empty, lowercase query cached; `day_totals` once per paint; day header and week range strings cached behind `set_day`/`set_week_anchor`; `import_ui` clone moved into the click; `pipeline.as_ref()`; standup task count stored on the row; `config.toml` parsed once per reload (`TimelineApp.config`) instead of three to four times.

Verification before deploy: 167 tests (166 + the MCP budget test), `cargo clippy --workspace --all-targets -- -D warnings` clean, `cargo fmt --check` clean; a sixth agent reviewed the whole diff against the pre-m28 tree (move-only check passed, no behaviour change found beyond the two intended ones: pollers surviving transient failures, MCP per-call budget).

Deployed 2026-09-03 16:35: release build 5 min 11 s with the new profile, binary 57 MB → 38.8 MB (stripped), `cargo install --locked --target-dir target`, migration 015 applied on the live DB, daemon healthy, UI re-opened. Open for James: the m27 replay regression (title-key rule), the chunk 3 quiet-box soak, and the m21 click-tests.
