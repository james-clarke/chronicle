# M18 — Settings as the setup home

Goal: Settings becomes where a customer wires Chronicle into their work life.
Everything they can supply — connections, model, capture rules, UI behavior —
lives there, discoverable, with live status ("connected", "3 repos watched",
"model ready"). Today's panel (`crates/app/src/ui/settings.rs`, sections
Capture / Derivation / Model / Storage & server / Integrations / Appearance)
is a config-file editor; this milestone turns it into setup.

## Principles

- Every integration shows **status + a test button**, not just a path field.
  ("mcp config path" tells a customer nothing; "Jira — connected as
  sam@… · test" does.)
- Anything the customer can hook up is signal for derivation, standups, and
  task anchoring. Prioritize sources that feed those.
- MCP is the plug shape. The Jira client (M8) generalizes: one MCP server
  list, each entry = name + transport + status. New sources become MCP
  servers, not bespoke clients.

## Connection candidates, by value

| Source | Feeds | Notes |
|---|---|---|
| Git repos (have) | commits as task evidence, ticket anchoring | promote from raw path list to picker + per-repo status |
| Jira via MCP (have) | task context, external refs | surface connection status + account |
| Calendar (Google/Outlook, via MCP) | meetings on the timeline, standup "in meetings 2h", AFK explanation | highest-value add: meetings are the biggest unexplained gap |
| GitHub/GitLab (via MCP) | PRs authored/reviewed as evidence + standup lines | second-highest: review time is invisible today |
| Slack (via MCP) | standup posting (outbound first; presence/channels later) | outbound-only keeps scope small |
| Linear/Notion (via MCP) | same slot as Jira for teams on those tools | schema already anchored on `external_ref` |
| Browser extension (have, AW-compatible) | per-site spans | surface "extension connected, last heartbeat Xs" status |
| Email (via MCP) | thread time as evidence | later; privacy-sensitive, off by default |

## Section layout (target)

1. **Connections** — MCP server list (add/remove/test), git repos with
   per-repo last-commit-seen, browser extension status, calendar/GitHub
   entries when configured.
2. **Model** — current: path + download; add per-job model choice later.
3. **Capture** — AFK, exclusions, distraction patterns, `background_minutes`
   (M17), retention.
4. **Standup & journal** — draft time-of-day, journal cadence, checkpoint
   AFK threshold.
5. **Window & appearance** — scale, stay-open/autohide toggle, start on
   login (packaging), position reset.

## Build order (draft)

1. Config: `mcp_servers` as a first-class list (name, command/url, enabled)
   replacing the single `mcp_config` path; migration reads the old file.
2. Connections section UI: server list CRUD + "test" (handshake + tool list)
   with cached status in `meta`.
3. Git repos: picker (dir dialog), per-repo status line from `vcs_events`.
4. Calendar MCP integration: events land in a `calendar_events` table;
   timeline renders meeting blocks; standup digest gains a meetings line.
5. GitHub MCP integration: PR/review activity as task evidence.
6. Standup section: scheduled draft time (cron in daemon scheduler),
   Slack post button on the Home card when a Slack server is configured.

Steps 1-3 are one coherent slice (no new data model); 4-6 are each their own
feature with schema + digest + UI work — order by customer pull.
