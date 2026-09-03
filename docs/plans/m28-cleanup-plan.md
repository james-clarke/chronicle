# m28 — clean up and optimize

Status: **in progress** (2026-09-03). Asked for as one milestone: close every loose end
from m21–m27, put the repo in order (docs, organization, sync), then review the whole
codebase and apply the optimizations that survive review. Run to completion by sub-agents
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

## Phase B — codebase review (read-only, sub-agents)

Five lenses, one agent each, findings as `file:line · severity · confidence · what · fix`:

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

(filled as phases land)
