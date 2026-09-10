# M42 — Open source: the licence, a synthetic corpus, and a clean history

Status: written 2026-09-10 on James's decision to publish the repository
under AGPL-3.0. Runs before any public release, and before the repository's
visibility is flipped. Every "today" claim carries a `file:line`.

## The short version

Chronicle is going public. The licence is decided and already in the tree.
What is not done is the reason this milestone exists: the repository, the
test corpus and the marketing site are full of real captured work, and
publishing them publishes that. A prefix rename does not fix it, because
the same strings are committed across 323 commits of history and baked into
sixteen screenshots.

So this milestone does three things in order. It replaces every real
identifier with a designed synthetic scenario, in the working tree. It
rewrites history so the past agrees with the present. Then it says, in the
README and on the site, what this project is and how it was built.

The ordering matters and is not negotiable: the content work lands first,
then one history rewrite pass over the result. Rewriting first and fixing
forward would leave the historical replacements disagreeing with the
designed ones, and would mean doing the rewrite twice.

## What the repository holds today

Counted with `git grep` over `HEAD`; the same strings run through the
history. The identifiers themselves are deliberately not written down here,
since this plan is published with the repository and naming them would put
back exactly what the milestone removes. The replacement map lives outside
the repository for the same reason.

| Kind | Occurrences |
|---|---|
| two repository names, one with its own Jira host | 419 |
| an employer Jira project prefix, some keys with real titles | 211 |
| the author's machine name, in shell prompts | 86 |
| a brand, in three environment variants | 119 |
| employer hostnames, including a staging host | 45 |
| an AWS RDS instance identifier | 10 |
| the employer Jira tenant | 8 |

Where they live:

- **Golden fixtures.** `fixtures/day3_sms.jsonl` (167 hits),
  `fixtures/day4_heroku.jsonl` (118), and their `.spans.golden` and
  `.digest.golden` companions. These are real captured days, not invented
  ones.
- **Source and tests.** `crates/core/src/extract.rs` (66),
  `segmenter.rs` (61), `tests/golden.rs` (50), `storage.rs` (28),
  `project.rs` (28), and the UI placeholder at
  `crates/app/src/ui/projects.rs:168`.
- **Plan documents.** `m33-interleaving-plan.md` (53),
  `m35-project-first-plan.md` (35), `m32-attention-plan.md` (31),
  and the Google Cloud setup notes in `m26-daily-driver-plan.md:47,207`.
- **The site.** `site/index.html:100-125` is a full day of real capture,
  and the screenshots under `site/img/` carry the same rendered. The
  OpenGraph image `site/img/og.webp` is the worst case, since it is what
  renders in every link preview.

Two facts that shape the work:

- **Commit messages are clean.** Zero hits across all 323 commits. The
  rewrite only has to touch file content, so every message, date, author
  and the shape of the graph survive untouched.
- **Some invented data is already right.** Contoso, Northwind, Acme,
  `jordan@studio.co`, `linear.app/acme`, `acme.atlassian.net` and
  `example.atlassian.net` are correct fixtures. They stay, and the new
  scenario should extend that cast rather than invent a second one.

## Decisions

- **AGPL-3.0-only.** Already in the tree: `LICENSE` holds the verbatim
  text, `Cargo.toml:10` carries the SPDX identifier. Verbatim matters, so
  the copyright line lives in the README rather than at the top of
  `LICENSE`, where it would confuse licence detectors.
- **One designed scenario, not a search and replace.** Mapping each name to
  a random word leaves fixtures that read like noise. The replacement is a
  coherent invented developer working invented tickets in invented repos,
  extending the Acme and Contoso cast already in the corpus. The golden
  files then read as a demo someone designed, which is also better copy.
- **Replacement is mechanical once the map exists.** Both sides of every
  assertion get substituted, so the golden tests stay meaningful. That is
  an assumption to verify, not to trust: the suite runs at the rewritten
  `HEAD` as a gate.
- **`git filter-repo`, not squash.** The history is the artefact this
  project wants to show. Squashing 323 commits to hide eight strings throws
  away the thing worth publishing. filter-repo replaces text in blobs and
  leaves messages, dates, authorship and graph shape alone. It is a single
  Python script, so it needs no sudo.
- **Screenshots leave history entirely.** filter-repo substitutes text and
  cannot touch a `.webp`. Old image blobs are stripped from history with
  `--path site/img --invert-paths`, and the regenerated ones land in a
  fresh commit. No story is lost, and the repository gets smaller.
- **Rewrite before the visibility flip.** The repository is private and has
  no collaborators, so a force-push is clean and this option exists exactly
  once. After the flip, forks and clones make it permanent.
- **The contact address is a separate question.** `callplaybook.com` on the
  site (`site/index.html:28,47`) is deliberate and already public, so it is
  not a leak. Whether a work address belongs on a personal AGPL project is
  James's call, not this milestone's.

## Chunks and gates

### 0. The synthetic scenario

- A cast document (`fixtures/README.md`): the invented developer, their
  repositories, ticket prefix, hosts and customers, extending Acme and
  Contoso.
- The replacement map as data, one `old==>new` per line, so the tree edit
  and the history rewrite read the same file and cannot drift. It is kept
  outside the repository on purpose: a committed map is a published index
  of everything this milestone set out to remove.
- Case variants are separate literal rules rather than a case-insensitive
  match. Several exist because a test is exercising case-insensitive
  matching, and folding them together would delete the thing under test.
- No replacement carries a hyphen. These names are Rust variables in the
  test code as well as strings in the fixtures, and a hyphen there parses
  as a subtraction.
- Gate: every kind in the table above appears in the map.

### 1. Scrub the working tree

- Apply the map across `fixtures/`, `crates/`, `docs/`, `site/`.
- Re-read the golden files by eye: substitution keeps tests passing but can
  leave prose that no longer parses as English.
- Gate: `cargo test --workspace` green, and a case-insensitive `git grep`
  for every identifier finds nothing anywhere in the tree.

### 2. Site text and the screenshot debt

- `site/index.html:100-125` becomes the designed day.
- The sixteen `site/img/*.webp` are regenerated from a Chronicle instance
  running the synthetic corpus. This is the expensive part and it is what
  M43 picks up; this milestone only records the debt and blocks the
  visibility flip on it.
- Gate: no identifier from the table survives in `site/`, images included.

### 3. Rewrite history

- `git filter-repo --replace-text` with the same map the tree edit used,
  then a second pass stripping `site/img` history.
- Force-push to the private remote. Re-clone into a scratch directory and
  run the suite there, because a rewrite that passes in the rewritten
  working copy can still have broken the tree.
- Gate: `git grep` over every commit reachable from every ref finds
  nothing; a fresh clone builds and tests green.

### 4. README, authorship and the release path

- README: the AGPL section with the copyright line, and the agentically
  built section. Framed as evidence, not as a disclaimer: how it is built,
  what the gates are, what is still unverified. `docs/plans/` is the
  exhibit.
- The download story simplifies once the repository is public: the
  `github-releases-repo` split is no longer needed and `GH_RELEASES_TOKEN`
  is not needed either.
- Gate: `dist plan` still resolves; the README's install block matches what
  a public release actually produces.

## Owed

- One of the two repository names may be the author's own project rather
  than an employer's. It changes nothing mechanically, but James should
  confirm before it is described anywhere as an example.
- The visibility flip, the force-push and the tag are all James's actions.
- The screenshot regeneration is real work and lives in M43.
