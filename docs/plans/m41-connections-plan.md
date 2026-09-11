# M41 — Connections as a surface: what you work with, and what you want next

Status: written 2026-09-10 on James's ask for "a first-class setup tool,
with options for users to suggest tool connections / what they work with,
which I can see and then get to integrating". Placed after M40 Windows in
`dev-tools-direction.md`, but chunks 0, 3 and 4 depend on no OS port and
can be pulled forward — see "When this should run". Every "today" claim
carries a `file:line`.

## The short version

Chronicle's accuracy and its appeal are both a function of how many of a
developer's tools it can read. That surface is currently a hand-rolled
Settings panel — 1752 lines in `crates/app/src/ui/connections.rs`, one
bespoke row per source, detection inlined (`on_path`, connections.rs:554;
session formats, :769; browser profiles, :778; per-repo hooks, :789),
Linux paths assumed throughout, and no representation at all for a tool
that exists but is not supported yet. A user who opens it learns what
Chronicle already does; they cannot learn what it *could* do, and they
have no way to tell James what they use.

This milestone turns connections into data: one registry of connectors
that drives the app, the CLI and the site; declarative per-platform
probes so Linux, macOS and Windows rows are the same code; a setup flow
that gets a fresh machine connected in one pass instead of a config file;
a list of the tools Chronicle *saw the person use and could not read*,
mined from their own activity; and a request path that files an
integration request from the user's browser — never from Chronicle — into
GitHub issues, where James reads and ranks demand.

## Decisions

**1. The registry is the product, not the panel.** One
`Connector` descriptor per integration in `crates/core/src/connectors.rs`
— id, display name, category, platforms, support state, connection kind,
probes, setup steps, the `ActivityKind` it produces, docs anchor. The
Settings panel, the new setup view, `chronicle connections`, `chronicle
status`, and the site's supported-tools page all render the same list.
Adding a tool becomes one descriptor plus a collector, and a tool that
has no collector yet still has a row.

**2. Support state is five values, and "planned" is first-class.**
`Supported`, `Partial(note)`, `Planned`, `Detected-but-unsupported`,
`WontDo(reason)`. Today the panel can only show what exists, so the
product looks smaller than the roadmap and gives a user nothing to push
on. A `Planned` row with a "tell me you want this" button is both honest
and the cheapest demand signal in the product.

**3. Requests leave through the user's browser, never through
Chronicle.** The site sells "No account, no server, no telemetry"
(site/index.html:7) and "Nothing leaves your machine" (:10, :36). A
background POST — even opt-in, even tiny — puts an asterisk on the one
sentence the product is sold on, and at beta scale the data is not worth
that. So the request button builds the text, shows it in full, and then
opens a prefilled GitHub issue in the user's browser under their own
account; "Copy" and "Save to file" are there for people without a GitHub
account. Chronicle makes no request of its own; the outbound list in
Settings › Storage & server (`egress_line`, settings.rs:514) stays true
as written.

*The escape hatch, deliberately not taken now:* the same payload can POST
to a small endpoint beside the Render site. It becomes worth building
when the local drop-off counter (requests composed, never filed) shows
issue friction is losing signal, or when private-beta users cannot file
publicly. It is a one-function change because the payload shape is fixed
here — and it costs a consent line in the UI and an edit to that sentence
on the site, which is the real price.

**4. Evidence is opt-in, redacted, and shown before it leaves.** A
request for "Zed" is worth ten times more when it carries "seen 4 h 12 m
over 30 days, process `zed`, sample title `main.rs — chronicle`". The
compose box shows exactly those lines with paths and hostnames redacted
through the M36 redaction pass, each attachable line individually
removable, and nothing else — no config, no DB, no project names unless
the person leaves them in.

**5. The unrecognized list is mined from real activity, not a blank
box.** Chronicle already knows which apps and domains it could not file:
`activity_events` and window titles that no connector claims. Ranking
those by minutes over 30 days gives the user a list to tick rather than a
field to fill, and gives James demand weighted by *time*, not by
enthusiasm. This is the part of the milestone no competitor can copy
without the capture layer.

**6. Cross-OS is a property of each probe, not a `cfg` fork.** Probes are
data — `OnPath(bin)`, `PathGlob(pattern)` with `~`/`%APPDATA%`/
`~/Library` expansion per platform, `MacBundle(id)`, `WinRegistry(key)`,
`LocalPort(n)`, `DbRows(kind)`. One evaluator per platform, one table of
patterns, so a macOS or Windows row is a data edit. Today the probes are
Linux-shaped Rust in the UI layer (connections.rs:554-805).

## What the code does today

- Settings is eight cards (settings.rs:662-898): Connections (:662,
  delegating to `connections::ui`, connections.rs:813), Projects, Model,
  Capture, Derivation, Standup & journal, Storage & server (:830, with
  `egress_line` at :514), Window & appearance.
- Connections renders MCP servers from five presets (connections.rs:63),
  git repos with per-repo hook chips (:1299), and local sources from
  `LocalSources` (:486) as toggle rows (:1517-1725). Detection is
  per-source code in the same file: `on_path` (:554), session formats
  (:769), browser profiles (:778), hooks status (:789), link files and
  editor workspaces read from the DB (:797).
- Configuration is a `deny_unknown_fields` TOML (config.rs:17) where each
  source is a typed field, plus `mcp.toml`, `models.toml`, `google.toml`
  and `meta` rows for probe caches and the WakaTime key.
- `chronicle status` reports a sources block by `ActivityKind` with rows
  and last-seen this week (M37 chunk 5) — kinds, not connectors, so
  "Cursor is installed and unread" is not expressible.
- The CLI has `shell-init` and `hooks {install,remove,status,backfill}`
  (main.rs:311, :358, sources.rs) but no `connections` command, and there
  is no setup flow: a new machine is a config file and a tour of cards.
- Nothing in the tree sends anything off-machine except cloud model jobs
  and MCP calls the user configured; there is no feedback path of any
  kind.
- The tool landscape is already surveyed in prose in
  `dev-tools-direction.md:285-431` (editors, AI agents, terminal and
  infra, git tooling, link files, VCS hosts and trackers, observability
  and consoles, chat and calendar, plus the connection-kind taxonomy).
  That table is the seed data for the registry; it currently lives only
  in a document.

## Chunks and gates

### 0. The registry

- `crates/core/src/connectors.rs`: `Connector { id, name, category,
  platforms: &[Platform], state: Support, kind: ConnectKind, probes:
  &[Probe], produces: &[ActivityKind], setup: &[SetupStep], docs }`, as a
  `const` table seeded from `dev-tools-direction.md:285-431`. Categories
  are the taxonomy already decided there (:400-431): files on this
  machine, local servers and sockets, your CLIs, accounts, MCP servers,
  tokens.
- `SetupStep` is one of `Toggle(config field)`, `Command(string)` (with a
  copy button, e.g. `chronicle shell-init zsh`), `Field(config field,
  hint)` (an ICS URL), `Install(url)`, `Account(flow)`.
- `chronicle connections [--json]` prints the table with each row's state
  on this machine; `--json` is the interchange for the site.
- `docs/connectors.json` is generated by the CLI and committed; a test
  fails when it drifts. Render has no cargo, so the site reads the file.
- Gate: every source the panel shows today exists as a descriptor and the
  CLI lists it; the JSON round-trips in a test.

### 1. Probes and health, on three platforms

- One evaluator per probe kind with per-platform path expansion; a
  `Health` per connector: `Absent`, `Found` (installed, not connected),
  `Connected` (no data yet), `Working { last_seen, rows_7d }`,
  `Broken { reason }`. `Working` and `Broken` read the same per-kind
  query `chronicle status` uses, keyed by connector rather than kind.
- macOS and Windows probe tables are filled in even where the collector
  is `Planned` — an installed-but-unsupported tool is exactly the row
  that should ask for a request.
- `chronicle status` gains a connector line per non-`Absent` row.
- Gate: fixture tests per probe kind; on this box every currently working
  source reports `Working` with a last-seen matching the sources block;
  `scripts/mac-check.sh` and the Windows CI job compile the tables.

### 2. Setup: the first five minutes

- A `Setup` view (a fifth `View`, mod.rs:559) shown on first run and
  re-runnable from Settings and from `chronicle setup`: it scans, then
  presents three groups — *already working*, *one step each* (with the
  command or field inline, copy button, and a "done" check that re-probes
  on the spot), *needs an account*. Skipping is one click and is
  remembered.
- The same flow in the terminal for headless installs, printing the same
  steps.
- Gate: on a scratch `XDG_DATA_HOME`, setup takes a fresh profile from
  zero to git repos discovered, sessions read and the shell hook line
  copied, with no editor open on `config.toml`; the M38 gate sentence
  ("a clean Mac reaches Home with projects populated in under five
  minutes") becomes a thing the product does rather than a thing the
  README explains.

### 3. What you work with

- **Mined:** a 30-day rollup of app classes, window-title tool names and
  domains that no connector claims, ranked by minutes, with the count of
  distinct days. Matching is by the same matcher the anchors use, so a
  tool that *is* supported but is not connected shows in group two of
  setup instead.
- **Declared:** a searchable list of the registry with tick boxes ("I use
  this"), plus free text for anything absent. Ticks are stored locally
  (`meta`), never sent on their own, and drive the panel's ordering: what
  you said you use sorts to the top, `Planned` rows included.
- Gate: on this box the mined list names tools Chronicle genuinely cannot
  read today and does not name any it can; ticking a `Planned` connector
  puts it at the top of the panel with a "requested?" affordance.

### 4. Requests

- One compose box, reachable from any row and from the mined list:
  category and name prefilled, an editable "why / what you'd want out of
  it" field, and the evidence lines from chunk 3 as individually
  removable chips. Below it, the exact issue body in a monospace preview.
- Three buttons: **Open GitHub issue** (a prefilled `issues/new` URL with
  `labels=integration-request` and the template's fields), **Copy**,
  **Save to file**. The first opens the browser; Chronicle makes no
  network call. A local counter records composed vs. filed for chunk 5's
  question about friction.
- `.github/ISSUE_TEMPLATE/integration-request.yml` with the same fields,
  so requests filed from the browser by hand land in the same shape.
- Gate: the URL round-trips (a body with newlines, backticks and a title
  containing `#` survives encoding); the preview is byte-identical to
  what the issue shows; a request composed with every evidence chip
  removed still carries tool, category and platform.

### 5. The loop James sees

- `gh issue list --label integration-request --json number,title,
  reactionGroups,body` ranked by 👍 then by count, in a short script under
  `scripts/`; the top of that list is the input to the next sources
  milestone. Duplicates get closed as duplicates, which is itself an
  answer to the user.
- `site/tools.html`, spliced by `site/build.sh` from
  `docs/connectors.json` (the splice mechanism exists, build.sh:22): every
  connector, its state, and per-platform support — the page a developer
  checks before downloading, and the page that makes "planned" credible.
  The site's byte budget check covers it.
- In-app, a `Planned` connector that has an open issue shows its number,
  so a user sees their request is on a list and not in a void.
- Gate: the site page matches `chronicle connections --json` on a clean
  build; the ranking script runs against the real repo.

## When this should run

The registry (0) and probes (1) are worth doing before M40 Windows, not
after: they are what makes the Windows port show up correctly in the
product instead of needing another panel edit, and they retire the
Linux-shaped detection currently sitting in the UI layer. Setup (2) wants
the OS ports to exist to be worth its gate. Requests (3, 4, 5) are worth
having on the day the first stranger runs the binary and are worthless
before it — so the honest sequencing is: 0 and 1 with or before M40, then
2, then 3-5 in the same push as the first public release. The prerequisite
that has nothing to do with code is that release: main is unpushed, there
is no tag, and no one can request anything for a binary they cannot get.

## Out of scope

Any automatic or background submission; a hosted endpoint (kept as the
documented escape hatch in decision 3); telemetry of any kind, including
anonymous usage counts; in-app voting on other people's requests (that is
what 👍 on the issue is for); writing the collectors this milestone
surfaces demand for; per-connector OAuth beyond what M37 left open.

## Open questions

1. Private beta users may not want to file publicly under their own
   account. If that shows up, the answer is "Copy" plus a mail alias
   rather than an endpoint, at least until volume justifies otherwise.
2. Whether the declared list ("I use this") should also gate collectors —
   telling Chronicle you do not use a tool could switch its poll off. It
   would save cycles and is one config field, but it can also silently
   turn off a source the person later installs, so it is left out here.
3. Whether `Planned` rows should carry a rough order rather than a flat
   list. An order is a promise; the issue ranking is evidence. Start with
   evidence.

## Shipped (2026-09-11, chunks 0–2)

Chunks 0 and 1 landed 2026-09-10/11 as planned (`connectors.rs`,
`health.rs`, `chronicle connections`, the Settings panel drawing the
registry, `chronicle status`'s per-connector block). Chunk 2 landed
2026-09-11 with these deviations:

- **The grouping is one function, shared.** `core::setup::plan` sorts every
  `Supported`/`Partial` connector: `Working` or `Connected` → *already
  working*; a descriptor with an `Account` or `Install` step → *needs an
  account*; anything else with steps → *one step each*. A supported tool
  with no steps that is simply not on the machine is left out — Settings
  still lists it, setup has nothing to offer for it. The view and
  `chronicle setup` render the same `Vec<Item>`.
- **Steps write config.toml directly** (`Config::save`), so the gate's "no
  editor open on config.toml" holds, but the daemon reads the file at
  start: the status line says so rather than pretending a switch is live.
  "check" re-probes the row against the edited config on the spot.
- **`git_repos` gets a scan.** Discovery walks the parents of watched
  repos, which a fresh profile has none of, so the `Field` row carries
  "find my repos": `~/dev`, `~/src`, `~/code`, `~/projects`, `~/work`,
  `~/repos`, `~/git`, each hit ticked, one button to add them as `~/…`.
- **The shell hook line follows `$SHELL`**: the descriptor names
  `chronicle shell-init zsh`; the view and CLI show the `eval` line for
  zsh/bash, `| source` for fish, `| Invoke-Expression` for pwsh, with the
  rc file to put it in.
- **First run is keyed on `meta` `setup_seen`** ("done" or "skipped"), not
  on the model's absence, so an existing profile sees the view once and
  `CHRONICLE_UI_VIEW=setup` opens it for the visual loop. It is not a tab;
  Settings › Connections has "run setup again".
- **`chronicle setup` prints and writes nothing**: the switches are
  `config.toml` fields and the commands are the person's to run.

Chunks 3–5 (what you work with, requests, the loop James sees) are open
and wait for the first public release.
