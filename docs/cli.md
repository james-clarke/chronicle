# CLI reference

`chronicle` is one binary: the daemon, the window, and the subcommands for reporting, tasks and setup. Running it with no subcommand is the same as `chronicle run`.

Subcommands are grouped here by what they touch; `chronicle --help` lists them in one flat list. Commands the daemon runs for itself are hidden from `--help` and not listed.

## Daemon

### run

Starts the daemon: activity capture, sessionizing, derivation, and the local HTTP server.

No flags.

```sh
chronicle run
```

Output: none. It stays in the foreground, and logs go to `logs/chronicle.log` in the data directory and to stderr. `chronicle service install` runs it at login instead.

### toggle

Shows or hides the UI window of the already-running daemon.

No flags.

```sh
chronicle toggle
```

Output: nothing on success; exits with an error if no daemon is running.

### status

Reports daemon health and recent activity: whether the daemon is up, what the derive worker is doing, model state, pending batches, capture gaps, self-score history, and per-connector status.

| Flag | Type | Default | Meaning |
|---|---|---|---|
| `--json` | boolean | `false` | Print machine-readable JSON instead of the human-readable summary. |

```sh
chronicle status --json
```

Output: a summary of liveness, uptime, derive worker activity, model file, idle time, last event age, pending batches and warnings, then one line per connector. Exits with status 1 if the daemon is not running.

With `--json`, the top-level keys are `liveness`, `daemon` (uptime, derive activity, worker label, model residency, idle seconds, UI open, focus route; null when the daemon is down), `focus_route`, `last_event_age_secs`, `last_batch_end_ms`, `model_present`, `model_file`, `server_error`, `last_prune_age_secs`, `last_derive`, `pending_batches`, `not_captured_today_ms`, `underived_today_ms`, `self_score`, `backend_score`, `connections`.

### service

Runs Chronicle at login: a systemd user unit on Linux, a LaunchAgent on macOS.

#### service install

Writes the unit or plist and enables it.

No flags.

```sh
chronicle service install
```

Output: a confirmation message.

#### service remove

Disables it and deletes the unit or plist.

No flags.

```sh
chronicle service remove
```

Output: a confirmation message.

#### service status

Reports whether it is enabled and running.

No flags.

```sh
chronicle service status
```

Output: the enabled and running state.

## Reading the data

### dump

Prints stored data, optionally scoped to one local civil day.

| Flag | Type | Default | Meaning |
|---|---|---|---|
| `--day` | date (`YYYY-MM-DD`), optional | none (everything) | Restrict output to this local day. |

```sh
chronicle dump --day 2026-09-01
```

Output: raw stored rows for the day (or everything), printed for inspection or debugging.

### report

Prints a timesheet for a day or a Monday to Sunday week, as CSV or Markdown.

| Flag | Type | Default | Meaning |
|---|---|---|---|
| `--day` | date (`YYYY-MM-DD`), optional | none | One local civil day. Conflicts with `--week`. |
| `--week` | date (`YYYY-MM-DD`), optional | current week | Any date in the week to report. Conflicts with `--day`. |
| `--format` | `csv` \| `md` | `csv` | Output format. |

```sh
chronicle report --week 2026-09-08 --format md
```

Output: a CSV table or Markdown report of time by task/project for the requested range, including capture gaps and underived time.

### standup

Prints the drafted standup for a day.

| Flag | Type | Default | Meaning |
|---|---|---|---|
| `--day` | date (`YYYY-MM-DD`), optional | yesterday | Day to print the draft for. |

```sh
chronicle standup --day 2026-09-12
```

Output: the standup draft text, or a message saying no draft exists yet for that day.

## Tasks and projects

### task

Declares, lists and edits tasks.

#### task list

Lists open tasks by project (id, project, label).

| Flag | Type | Default | Meaning |
|---|---|---|---|
| `--project` | string, optional | none (all projects) | Keep only this project (its configured name, or a repo folder that names it). |
| `--all` | boolean | `false` | Include closed tasks too. |

```sh
chronicle task list --project chronicle
```

Output: tasks grouped under a project heading, each line showing id, label, and markers such as `declared`, `current`, `closed`. A declared task with pinned scope shows it on the next line.

#### task add

Declares a task, the way the Home view's declare row does.

| Argument/Flag | Type | Default | Meaning |
|---|---|---|---|
| `label` (positional) | string | required | What you are working on. A ticket key or URL in it becomes the task's anchor. |
| `--project` | string, optional | none | A configured project, or a repo folder that names one; anything else is kept as typed and stays unfiled until a rule matches it. |
| `--description` | string, optional | none | Task description. |

```sh
chronicle task add "ACME-123 fix login redirect" --project acme
```

Output: `task <id>: <label> [<project>]`.

#### task close

Closes a task; it stops accumulating time.

| Argument | Type | Meaning |
|---|---|---|
| `id` (positional) | integer | Task id, from `task list` or the UI. |

```sh
chronicle task close 42
```

Output: `task <id>: <label> closed`.

#### task rename

Changes a task's label, project, or description. Label and project edits are stored as a correction the model reads before naming similar work again.

| Flag | Type | Default | Meaning |
|---|---|---|---|
| `id` (positional) | integer | required | Task id. |
| `--label` | string, optional | unchanged | New label. |
| `--project` | string, optional | unchanged | New project; an empty string clears it. |
| `--description` | string, optional | unchanged | New description; an empty string clears it. |

```sh
chronicle task rename 42 --label "fix login redirect loop"
```

Output: a line per change.

#### task current

Marks an open declared task as its project's sink: new time in that project goes to it ahead of any newer declared task.

| Argument | Type | Meaning |
|---|---|---|
| `id` (positional) | integer | Task id, from `task list` or the UI. |

```sh
chronicle task current 42
```

Output: `task <id>: <label> is current in <project>`.

#### task attach

Pins what a declared task covers besides its ticket. Time that carries a pinned value files to the task's project ahead of the project rules, and lands on the task, for as long as the task is open. Use it when a ticket prefix is shared by sibling projects, or when the work lives in a repo, branch, document or site the project rules do not name.

| Argument/Flag | Type | Meaning |
|---|---|---|
| `id` (positional) | integer | Task id, from `task list` or the UI. |
| `--repo` | string, repeatable | A repo path or folder name. |
| `--branch` | string, repeatable | A branch name. |
| `--doc` | string, repeatable | Part of a document, page or file name. |
| `--domain` | string, repeatable | A site; its subdomains count. |
| `--item` | string, repeatable | A work-item key besides the task's own. |
| `--remove` | boolean | Unpin the given values instead. |

```sh
chronicle task attach 183 --repo ~/dev/agent-backend --branch feat/ACME-11342-agent
```

Output: a line per value pinned or unpinned, then the number of spans re-filed since the task was declared.

### project

Manages the rules that file time into a project.

#### project list

Lists the projects in force: name, remote, paths and rules.

No flags.

```sh
chronicle project list
```

Output: one block per project with its matching rules.

#### project test

Matches the last N days against the current config without writing anything, to show minutes per project, unfiled minutes and the top unfiled places and titles. Use it to tune rules before `project rebuild`.

| Flag | Type | Default | Meaning |
|---|---|---|---|
| `--days` | integer | `7` | Window in days. |
| `--top` | integer | `15` | Number of unfiled places and titles to list. |

```sh
chronicle project test --days 14 --top 20
```

Output: minutes per project, unfiled minutes, and a ranked list of unfiled places and titles.

#### project rebuild

Re-files stored spans after a config edit.

| Flag | Type | Default | Meaning |
|---|---|---|---|
| `--days` | integer, optional | none (all history) | Limit the rebuild to this many recent days. |

```sh
chronicle project rebuild --days 30
```

Output: a count of spans re-filed, any tasks whose project was renamed to a configured one, open tasks in a project no rule knows, and any derived tasks that became proposals because nobody had confirmed, renamed or placed time on them.

#### project attach

Adds rules to a project, writes them to `config.toml`, tells the running daemon to pick them up, and re-files the last N days so the change shows at once. A name no project has becomes a new project. The timeline and Home menus and the Projects screen write rules the same way.

| Argument/Flag | Type | Default | Meaning |
|---|---|---|---|
| `name` (positional) | string | required | The project's configured name. |
| `--repo` | string, repeatable | | A repo or folder path (`~` allowed); its worktrees count. |
| `--ticket` | string, repeatable | | A work-item key prefix. |
| `--domain` | string, repeatable | | A site; its subdomains count. |
| `--title` | string, repeatable | | A window-title regex, matched anywhere. |
| `--app` | string, repeatable | | A whole app, by name. |
| `--parent` | string, optional | unchanged | Put the project under this one; an empty string lifts it to the top. |
| `--days` | integer | `30` | How many days back to re-file. |

```sh
chronicle project attach acme-web --repo ~/dev/web --parent acme
```

Output: the rules added, then minutes per project before and after the re-file for every project whose minutes changed.

## Models

### model

Manages local LLM models.

#### model pull

Downloads a model preset (resumable, SHA-256 verified).

| Argument | Type | Default | Meaning |
|---|---|---|---|
| `preset` | string, optional | `qwen3-4b` | Preset name to download. |

```sh
chronicle model pull qwen3-4b
```

Output: a progress percentage while downloading, then the verified file path.

#### model list

Lists presets and their download state.

No flags.

```sh
chronicle model list
```

Output: one line per known preset (name, downloaded/not downloaded, file path), then which one is currently active.

## Sources

### connections

Lists every tool Chronicle can read, what it does with it, and what connecting it here would take.

| Flag | Type | Default | Meaning |
|---|---|---|---|
| `--json` | boolean | `false` | Print the connector registry as JSON, the same data the site's tools page reads. Conflicts with `--html`. |
| `--html` | boolean | `false` | Print the site's tools-page HTML body. Conflicts with `--json`. |

```sh
chronicle connections --json
```

Output (default): one section per connector category, each row showing a status word, the connector name, and a status note.

With `--json`, an array of connector objects with the keys `id`, `name`, `kind`, `platforms`, `state` (`Supported`, `Partial`, `Planned`, `Detected` or `WontDo`, with a note where there is one), `blurb`, `probes`, `produces`, `setup` and `docs`.

### setup

Prints the checklist the Setup view shows, for a headless install: what already works on this machine, what is one step away, and what needs an account first.

No flags.

```sh
chronicle setup
```

Output: grouped checklist with a status word, connector name, and note per row; steps to take are listed under anything not already working.

### shell-init

Prints the shell hook to add to your rc file. Once installed, it posts each command's working directory, program name and duration to the local endpoint, never the command line.

| Argument | Type | Meaning |
|---|---|---|
| `shell` (positional) | string (`zsh`, `bash`, `fish`, `pwsh`) | Which shell to print the hook for. |

```sh
chronicle shell-init zsh
```

Output: a shell snippet. Add `eval "$(chronicle shell-init zsh)"` to your rc file rather than pasting it.

### hooks

Manages Chronicle's git hooks, which record exact-second checkout and commit times.

#### hooks install

Appends the Chronicle line to `post-checkout`, `post-commit` and `post-rewrite`, after any existing hook content.

| Argument | Type | Meaning |
|---|---|---|
| `repo` (positional, optional) | string | One configured repo; default is every configured repo. |

```sh
chronicle hooks install
```

Output: one line per repo confirming hooks were installed (or already present).

#### hooks remove

Strips the Chronicle line back out.

| Argument | Type | Meaning |
|---|---|---|
| `repo` (positional, optional) | string | One configured repo; default is every configured repo. |

```sh
chronicle hooks remove
```

Output: one line per repo confirming removal.

#### hooks status

Shows which repos have the hooks installed.

| Argument | Type | Meaning |
|---|---|---|
| `repo` (positional, optional) | string | One configured repo; default is every configured repo. |

```sh
chronicle hooks status
```

Output: one line per repo, installed or not.

#### hooks backfill

Reads checkouts from a repo's reflog that the regular 20-second poll missed.

| Argument/Flag | Type | Default | Meaning |
|---|---|---|---|
| `repo` (positional, optional) | string | every configured repo | Repo to backfill. |
| `--days` | integer | `30` | How far back into the reflog to look. |

```sh
chronicle hooks backfill --days 60
```

Output: a count of checkouts backfilled.

### gcal-login

Signs in to Google Calendar via a loopback OAuth flow and stores the refresh token in `<data dir>/google.toml`.

| Flag | Type | Default | Meaning |
|---|---|---|---|
| `--client-id` | string, optional | `$CHRONICLE_GOOGLE_CLIENT_ID` | OAuth client id. |
| `--client-secret` | string, optional | `$CHRONICLE_GOOGLE_CLIENT_SECRET` | OAuth client secret. |

```sh
chronicle gcal-login --client-id 123.apps.googleusercontent.com --client-secret abc123
```

Output: a URL to open if the browser did not launch, then the signed-in account and the token file path once the flow completes.

### mcp-check

Runs the allowlisted MCP context calls from `mcp.toml` and prints what derivation would inject into its prompt.

No flags.

```sh
chronicle mcp-check
```

Output: the config path, one line per server with its probe result (ok, with tool count and elapsed time, or FAILED with the error), then the gathered workspace context text (or a message saying nothing was gathered).

## Inspection and repair

### anchors

Reports how much focus time carries an anchor (a work item, document, or place) versus none, and which anchors cover the most time.

| Flag | Type | Default | Meaning |
|---|---|---|---|
| `--days` | integer | `7` | Window in days ending now. |
| `--top` | integer | `20` | Number of anchor values to list. |

```sh
chronicle anchors --days 30 --top 10
```

Output: a table of the top anchors by minutes, plus the overall anchored-vs-unanchored split for the window.

### evidence

Shows the evidence behind task placement: either one task's evidence rows, or a cross-task summary of the strongest evidence.

| Flag | Type | Default | Meaning |
|---|---|---|---|
| `--task` | integer, optional | none | Show this task's evidence rows instead of the cross-task summary. |
| `--top` | integer | `5` | Entries to list per task in the summary. |

```sh
chronicle evidence --task 118
```

Output: for `--task`, a list of evidence rows (source, strength, timestamp) for that task; without it, the top evidence entries across all tasks.

### backfill-embeddings

Embeds task labels and corrections into the example memory, then, if `embed_model` is set, every focus span without a vector yet plus the task centroids.

No flags.

```sh
chronicle backfill-embeddings
```

Output: progress lines as it embeds; a final count of rows embedded.

### backfill-descriptions

Generates descriptions for closed tasks that have none yet, newest first.

| Flag | Type | Default | Meaning |
|---|---|---|---|
| `--limit` | integer | `50` | Maximum tasks to describe in this run; rerun to continue further back. |

```sh
chronicle backfill-descriptions --limit 20
```

Output: one line per task described, with its new description.
