# Teams — direction research (2026-09-02, no code)

James's framing: the money is in teams. People work on shared projects, their
Chronicles sync, and settings decide which projects the team sees at all. The
point is not a manager looking at hours. It is **keeping teammates, the whole
team and the manager in line with what needs to be done**: who is on what
today, what is next, what drifted from the plan, what is stuck, without anyone
typing a status. This doc is the research behind that: is the gap real, who
pays, how to sync, what it costs in the codebase, and the order to build it
in. Four sub-agents surveyed sync engines, competitors, business models and
the codebase; this is the synthesis. Sources at the end. Decisions are
proposals until James edits them.

## The frame: alignment, not hours

Every existing product in this space sells one of two things: hours (time
trackers, bossware) or typed status (standup bots, project updates). Both
fail the same way: the hours say nothing about whether the work was the
right work, and typed status is stale the moment it is written and nobody
reads it. Chronicle already has the pieces of a third thing, per person,
derived from what actually happened:

| Alignment question | Where Chronicle already has the answer |
|---|---|
| What is each person on right now? | open tasks with anchors (ticket, branch), live feed |
| What did they do yesterday, in their words? | standup draft, journal entries |
| What is next / where did they stop? | checkpoints (state + next steps) |
| Did the day go where it was meant to? | needs the intent layer (post-m16 candidate #4): "today I'm on X", drift vs plan in the day digest |
| Is something stuck? | a task open for days with little focus, or a checkpoint whose next steps have not moved; no new signal needed |
| Are two people on the same thing? | two open tasks anchored to one ticket or branch across members |
| How much did project X actually get? | project totals (reports) |

The team product is this table, shared per project, with the same rows shown
to the member, the team and the manager. Hours are one column, not the
product. This is also the framing that survives a works council: the tool
answers "what needs to be done and is it happening", not "how busy are
you".

## The short version

1. **The gap is real and currently empty.** Nobody sells automatic capture +
   derived-only sync + per-project sharing + self-host in one product, and
   nobody derives team alignment (who is on what, what is next, what
   drifted) from activity: standup bots and project-update tools all rely on
   typed check-ins. Memtime and Timely prove people pay $12–29/seat/mo for
   "cannot be used for monitoring" automatic tracking; Swarmia/LinearB/
   Jellyfish prove engineering managers pay $150–800/dev/yr for "who worked
   on what" built from git metadata alone. Chronicle sits between them with
   a stronger signal (real activity, linked to tickets and branches) than
   either.
2. **Buyer:** team leads and engineering managers at small/mid dev shops who
   want the standup meeting to disappear and the plan to stay true; agencies
   billing by the hour as a second segment (they need the hours column).
   Not HR/compliance. Bossware (ActivTrak ~$50M revenue) is the biggest
   segment and the one to stay out of: screenshots, idle drill-downs and
   productivity scores are exactly what generates backlash and regulation.
3. **Price:** $9–15/seat/mo, free up to 3 seats, annual discount. Solo app
   stays free and accountless while it is Linux-only; revisit a paid solo
   licence at the cross-platform launch. Sync server source-available (FSL),
   client proprietary as today.
4. **Sync:** plain HTTPS outbox push + cursor pull against an axum + Postgres
   server. Rows get UUIDs and an outbox table fed by SQL triggers. No CRDT,
   no PowerSync/Turso/ElectricSQL: the data is append-mostly and each row has
   exactly one author, so those buy complexity Chronicle does not need and
   each carries a real risk (below).
5. **Hard product rules** that make it sellable to a works council and not
   creepy: only derived rows leave the machine, per-project opt-in, the
   member sees exactly what the manager sees, aggregate-first (hours per
   day, not minutes), no scores, no idle alerts, self-host available.
6. **Sequencing risk:** a teams tier is unsellable on Linux only. Teams are
   mixed-OS. The macOS/Windows ports (README M17/M18) have to land before or
   alongside the sync server. Everything below is ordered so the pre-server
   work is useful on its own.

## What competitors do (and what to copy)

| Product | Captures | Manager sees | Member control | $/seat/mo | Self-host | Take |
|---|---|---|---|---|---|---|
| Memtime | apps/docs, local only | only what the user submits to the timesheet | full: review before submit | 12–29 | no | closest philosophy; no dev/git link |
| Timely (Memory) | apps/docs/browser | project hours, timesheets | approves each entry before it is booked | 11–28 | no | consent-per-entry model, cloud only |
| Timing (Mac) | app/window/doc titles | team reports | per-project rules | 8–17 | no | structurally the same plan, Mac only |
| WakaTime / Wakapi | IDE + git | leaderboards, commit stats | none | 11–45 / free | Wakapi yes | dev-only, gamified; Wakapi exists *because* WakaTime is closed |
| Rize | apps, AI categories | utilisation by client/project | none | 30–40 | no | no screenshots, cloud only |
| RescueTime Team | apps/URLs | category aggregates | category privacy | 6–14 | no | no task/ticket link |
| Swarmia / LinearB / Jellyfish | git/PR/ticket metadata | DORA, allocation, "who worked on what" | org-wide | 150–800/yr | no | same buyer, weaker signal |
| ZeroStandup / DailyBot / Spinach | Jira/GitHub/Slack | standup summaries | n/a | low SaaS | no | standup demand is validated; none use device activity |
| Hubstaff / Time Doctor / ActivTrak / Insightful / DeskTime | screenshots, activity %, stealth mode | per-minute idle, scores, video | minimal (DeskTime has a "private time" button) | 5–25 | no | the segment to avoid; 42% of monitored staff plan to quit |

The alignment side of the market, checked 2026-09-02: every async status
tool is typed. Range ($8/user, free ≤20), Status Hero ($7–14), Geekbot
($2.50–3 per answering participant, free ≤10), DailyBot ($2.40–5, "AI
summaries" of typed answers), Standuply, Slack Workflow standups, Basecamp
"automatic check-ins" (auto-scheduled questions, typed answers). Spinach
pivoted to transcribing live standup calls ($19–29). Friday.app shut down in
2022. Linear's initiative updates (2025-02) compute a progress rollup from
issue completion but the status text is still written by the owner. Swarmia
"working agreements" checks real git data against team targets (process
metrics, not who is on what); Jellyfish/LinearB derive roadmap-vs-maintenance
allocation for leadership, not a per-person today view. What users of the
typed bots complain about: answers go unread, it is busywork for a bot,
people write vague or gamed answers to satisfy the prompt. What they say they
want: status that reflects reality without stopping to type it, blockers and
drift surfaced before the daily prompt, one place to see who is on what and
what is stuck without a meeting. That is the frame table above, derived.

Copy: Memtime/Timely's "you approve what is shared", Anytype's per-space
(per-project) invite and roles, DeskTime's visible private-time control,
Obsidian's page that enumerates every network call.

Team view, in the order the alignment frame wants it (evidenced demand from
the competitor reviews in brackets): (1) today's board: each member's open
tasks with ticket/branch anchors and what they said they were on
[standup-bot category], (2) standup rollup written from activity, one page
for the team [ZeroStandup/DailyBot/Spinach demand, none derive it],
(3) checkpoints: where each task stopped and what is next [no competitor],
(4) drift and stuck: plan vs actual per person, tasks open for days without
movement, two people on one ticket [Swarmia/LinearB sell a weak version from
git], (5) who touched project X (branch/PR/commit rollup) [WakaTime,
Swarmia], (6) project hours by person/week and timesheet export to
Tempo/Harvest [the top commercial ask in time-tracker reviews; the agency
column], (7) journals visible to the team (opt-in), (8) leaderboards (opt-in,
per team). Idle/blocked *alerts* on individuals and screenshots are the top
complaints in every review set: "stuck" is shown as a task state, never as a
person score.

Regulatory shape to design for (not legal advice): German Betriebsrat
co-determination on monitoring software (§87(1) No. 6 BetrVG) stalls sales
for months unless the tool is aggregate-only and self-hostable; UK ICO 2023
guidance wants least-intrusive means and a DPIA; NY/CT/DE require written
notice; Ontario Bill 88 requires a written policy at 25+ staff. A setup step
that emits the notice/policy text from the actual configuration is cheap and
sells.

## Business model

Comparables: Obsidian (free app, paid Sync $4/mo, no team tier, profitable
solo studio), Bitwarden (Teams $4, self-host at the same price: the licence is
the product, not the hosting), Tailscale (free ≤6 users, teams pay for the
coordination server), Raycast (free / Pro $8 / Teams $12), Plausible
(self-host free but behind on features), Standard Notes (self-host licence at
67% off), Logseq (sync in paid beta for years: the cautionary tale on
timeline), Wakapi vs WakaTime (the clone risk of an open client + closed
server).

Proposal:

- **Solo:** free, no account, as today. Payment for a solo licence would need
  a store or licence key but no server identity; decide at the macOS launch.
- **Teams:** hosted, $9–15/seat/mo (anchor against Linear Basic $10 and
  Raycast Teams $12, under Swarmia/LinearB), free up to 3 seats, annual
  billing default. Merchant of record (Paddle/Lemon Squeezy) so a solo founder
  does not run tax.
- **Self-host:** the sync server ships as one container under a
  source-available licence (FSL, converts to Apache after two years). Same
  seat price as hosted for teams above the free cap (the Bitwarden pattern);
  the licence key gates seats, not features. This is the answer to "we can't
  send anything to your cloud" without giving hosted revenue away.
- **Trust signals** in lieu of open source: a page listing every network
  call the app can make, an in-app "what left this machine" view (below), a
  firewall-verifiable offline switch, inspectable server source.

## Sync architecture

Ranked for Chronicle's shape (append-mostly, one authoring device per row,
selective by project, laptops offline half the day, Rust client):

| Option | Rust client | selective sync | offline writes | E2EE | self-host | licence | verdict |
|---|---|---|---|---|---|---|---|
| HTTPS outbox push + cursor pull (own) | you write it | by construction | by construction | yes (encrypt payload) | one container | n/a | **recommend** |
| PowerSync | Rust SDK pre-alpha (Tauri alpha 2026-03) | yes, buckets keyed on JWT claims | yes | no | free Open Edition, FSL | FSL server | best-fit architecture, no production Rust embed; re-check in 6–12 months |
| cr-sqlite | C extension via `load_extension` | table-level | yes | unproven | yes | MIT | last release v0.16.3 2025-01, no activity since: maintenance risk |
| Turso / libSQL | `turso` crate healthy | page-granular, not per-project | beta since 2025-03 | no | server closed-source since 2025-01 | client OSS | conflicts with self-host |
| Automerge / Loro / yrs | Loro strong | n/a (document CRDT) | yes | yes | embed | MIT/Apache | wrong shape: solves concurrent editing Chronicle does not have |
| ElectricSQL | none | shapes, read-only | no write path | n/a | yes | Apache | dismiss |
| Ditto / Replicache / Zero | Ditto only | — | — | — | enterprise / JS-only | — | dismiss (Replicache archived 2026-06) |

The recommended shape is the CouchDB/PouchDB replication protocol: rows keyed
by UUID, the server assigns a monotonic `seq` on ingest (never cursor on
client clocks), clients pull `?since_seq=N`, deletes are tombstones with a
retention window (a client absent longer than the window does a full
resync). E2EE is additive later (encrypt the payload blob, team key shared
out of band) and costs server-side filtering, so v1 ships server-visible
plaintext scoped to the team.

Server: axum + Postgres (sqlx), one binary, `docker compose` with Postgres
for self-host. Multi-tenant by `team_id` column. Tiny schema: `teams`,
`members`, `devices`, `projects` (the allowlist), `rows(team_id, table,
uuid, seq, author_device, payload jsonb, deleted_at)`. The dashboard reads
`rows` into normal tables per kind.

Auth: magic-code email login issued by our own server, long-lived device
token stored at mode 0600 next to `mcp.toml` (the credential-file pattern
already exists). OAuth device flow and SSO/SAML via WorkOS AuthKit later;
its per-connection SSO pricing suits SMB teams better than per-user vendors.
No third-party auth dependency in v1.

Who reads the shared data: **v1 is the desktop app itself** (a Team tab that
pulls the team's rows and reuses the reports project grouping). Everyone on
a team runs Chronicle anyway, so the first manager view costs one egui tab,
not a second front end. A web dashboard for managers who do not install
comes after, server-rendered from the same tables.

### What leaves the machine

Per team-tracked project only, and the member can see the exact rows:

| Synced | Not synced |
|---|---|
| task label, project, ticket ref, status, description | raw `events` / `spans` (window titles) |
| intent: what the member said they are on today (intent layer) | intervals with timestamps (opt-in later, per team) |
| checkpoints: state + next steps per task | chat conversations |
| journal entries | corrections, proposals, `meta` |
| standup draft (after the member has seen it) | AI session prompts, call spans, mic state |
| per-task **daily totals** (not minute intervals) | the day digest, app/site breakdowns |
| commits and PRs already public in the team's repos | anything from a project not marked shared |

A member's view of shared data and a manager's view are the same query with
a different `member_id` filter. This one rule carries most of the
regulatory weight.

## What the codebase needs first

From the survey (cites are current as of 2026-09-02):

- **No global ids, no `updated_at`.** Every table is an integer rowid;
  `tasks` has `created_ts`/`closed_ts` only
  (`crates/core/migrations/004_task_identity.sql:21-29`), `intervals` has none
  (`012_interval_tail.sql:9-18`). Fix: add `uid TEXT` (UUIDv7) to tasks,
  journal_entries, checkpoints, standup_drafts, activity_events, backfilled
  in a migration; add an `outbox(seq, table, uid, op, ts)` table fed by
  AFTER INSERT/UPDATE/DELETE triggers. Triggers, not Rust call sites: writes
  are spread over a dozen functions (`insert_user_task` `storage.rs:814`,
  batch apply `:1427-1454`, `insert_journal_entry` `:960`,
  `upsert_checkpoint` `:1146`, `upsert_standup_draft` `:1283`,
  `insert_activity_event` `:94`, `merge_task` `:2096`), and the codebase
  already uses triggers for FTS (`001_schema.sql:65-86`).
- **`project` is free text with two normalisations.** The repo↔project join
  lowercases (`storage.rs:306`), weekly reports compare exactly
  (`report.rs:41`), so "chronicle" and "Chronicle" split in Reports but not in
  attribution. A team allowlist needs a `projects` table (normalised name,
  display name, `shared` flag, colour) that tasks reference. This is a
  standalone fix worth doing regardless of teams.
- **Overwrite-in-place rows** (`checkpoints`, `task_context`,
  `standup_drafts`) have no version; the outbox seq covers it.
- **Local deletes without tombstones:** orphan-task cleanup
  (`storage.rs:1389-1391`) and retention pruning cascade through
  journal/checkpoint FKs (`008_task_workspace.sql`). The outbox DELETE trigger
  turns these into tombstones; retention must not prune the outbox before it
  is pushed.
- **`merge_task` closes the loser and moves its intervals** (`storage.rs:2096-2136`):
  sync consumers see a task close and another grow; fine with daily totals,
  document it.
- **No identity anywhere** (no git author or `gh` user lookup); no outbound
  HTTP except the model download (`crates/derive/src/model.rs:65,138`,
  `ureq`). A sync client is net-new: `ureq` is already a dependency and is
  enough for push/pull.
- **Where it plugs in:** a sync worker is pure I/O, so it is its own thread
  on the daemon's `crossbeam` pattern (`main.rs:1466-1470`) with a timer arm
  next to `next_refresh` in the select loop (`main.rs:1495-1553`), not an
  `ai_jobs` kind. Settings gets a "Team" section via the existing
  `section(ui, …)` helper (`ui/settings.rs:97`); the Team tab reuses
  `report::project_totals` (`core/src/report.rs:33-124`) and
  `reports.rs` `project_group` (`:332`); a team standup panel mounts beside
  `standup_card_ui` (`home.rs:72`).
- **Existing privacy gates to respect:** `excluded_apps`/`excluded_titles`
  drop events before storage (`main.rs:2136`), MCP text is capped and
  treated as untrusted (`digest.rs:341`). The `shared` flag must be checked
  in the sync worker and nowhere else needs to change for v1, because only
  derived rows are pushed.

## Proposed milestones

Each step is useful without the next one.

- **m25 — UI polish 3** (separate plan, `m25-polish-plan.md`). Independent.
- **m26 — Foundations + intent, single player.** `projects` table +
  normalisation fix; UUIDs + outbox triggers; a stable `machine_id` in
  `meta`; Settings › "What leaves this machine" view that today truthfully
  says "nothing" and lists the model download. Plus the **intent layer**
  (post-m16 candidate #4): morning "today I'm on …" on Home (pick from open
  tasks or type one), evening drift line in the day digest and standup
  ("planned X, spent most of the day on Y"), and a "stuck" state on tasks
  open for days without movement. Zero network. Every team-view row in the
  frame table then exists locally for one person, which is the thing worth
  syncing.
- **m27 — Publish outward, no server.** The first team value with no sync:
  post the standup draft to Slack and a worklog to Jira through the MCP
  action allowlist (the write-back path already on the roadmap), always
  behind an explicit confirm. This tests whether the derived layer is good
  enough for other humans to read, which is the whole bet, before a server
  exists.
- **Ports (M17 macOS, M18 Windows).** Gate for m28's revenue. Big, separate
  track; can run in parallel with m26–m27.
- **m28 — Sync v1.** axum + Postgres server (`crates/sync-server`, FSL),
  magic-code login + device token, teams/members/projects, outbox push +
  cursor pull, per-project `shared` opt-in in Settings, Team tab in the app:
  today's board (each member's open tasks, intent, anchors), the team
  standup page, checkpoints per task, drift/stuck/duplicate flags, who
  touched what. Hours are a column on the board, not a view. Single
  container self-host from day one.
- **m29 — Manager surface + money.** Web dashboard (server-rendered) with
  the same board for managers who do not install, CSV / Tempo / Harvest
  export for the agency segment, notice/policy text generator, seat billing
  via a merchant of record, free ≤3 seats.
- **Later:** E2EE payloads, SSO/SAML via WorkOS, opt-in interval-level
  sharing, journals visible to team, leaderboards.

## Open questions for James

- Solo app free forever, or a paid licence at the macOS launch?
- Daily totals vs minute intervals in v1: totals are the safer default; do
  agencies billing clients need intervals for timesheets on day one?
- Self-host price: same seat price as hosted (Bitwarden) or a discounted
  licence (Standard Notes)?
- Does m27 (Slack/Jira publish) come before the ports, or do the ports jump
  the queue because they gate everything commercial?
- Where does the plan come from? Intent typed each morning is the simplest;
  pulling the sprint from Jira/Linear (assigned, in-progress issues) through
  the existing MCP allowlist would make "drift vs plan" automatic and is
  probably what a manager means by "what needs to be done". Both, in that
  order?
- Should teammates be able to comment on or nudge a task (the first shared
  write), or is v1 strictly read-only across members?

## Sources (checked 2026-09-02)

Sync: CouchDB replication protocol docs; PowerSync sync-rules docs and
"new open era" licence post; releases.powersync.com (Rust SDK pre-alpha);
vlcn-io/cr-sqlite releases (v0.16.3, 2025-01-17); turso.tech blog 2025-01-21
(closed multitenant server) and 2025-03 (offline sync beta); automerge.org
3.0 changelog; loro.dev; electric-sql.com/docs/guides/writes; RFC 8628;
workos.com/blog/cli-auth.
Competitors: wakatime.com/pricing; getlatka.com (WakaTime, RescueTime,
ActivTrak revenue); timingapp.com/help/teams-faq; rize.io/pricing;
Memtime and Timely via GetApp/Digital Project Manager 2026; hubstaff.com
pricing guide; business.com ActivTrak review; codepulsehq.com engineering
analytics comparison; NELP "When Bossware Manages Workers" 2025-07; UK ICO
monitoring guidance 2023-10; Blakes on Ontario Bill 88; mosey.com on NY/CT/DE
notice laws; Forbes 2024-06-18 (Wells Fargo mouse jigglers).
Alignment tools: range.co/pricing; geekbot.com/pricing; dailybot.com/pricing;
Status Hero and Standuply via Capterra; Spinach via SaaSworthy;
friday.app/p/shutting-down; linear.app/changelog/2025-02-13-initiative-updates;
swarmia.com/product/working-agreements; geekbot.com/blog on standup fatigue.
Business: obsidian.md/privacy; raycast.com/pricing;
standardnotes.com/help/self-hosting/subscriptions; costbench.com (Bitwarden,
Tailscale, Logseq); Sentry FSL announcement; HashiCorp BSL post; Plausible
self-hosting comparisons. Pricing figures from aggregators are directional:
verify on vendor pages before setting a price.
