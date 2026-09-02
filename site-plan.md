# Site plan: engineery, honest, simple

Goal: the page should read like an engineer explaining a tool they built and use, not a pitch. Three things it has to do: show the tool is actually useful, make "nothing leaves your machine" believable, and stand out while staying one page with no framework.

Research basis: fetched pages for Obsidian, Zed, Sublime, Plausible, Tailscale, Mullvad, Kagi, Timing, Rize, Superwhisper, Ente, Bitwarden, Cryptee, SQLite, ActivityWatch, Overcast, Nova, Bear, Tarsnap, Pinboard, Sourcehut, Bear Blog, Marginalia, Ghostty, Helix, mise, Bun, uv, IVPN. Patterns cited inline.

## Diagnosis of the current page

1. **Headline works against the privacy story.** "Track everything, lose nothing" is the language of surveillance software. An engineer landing here reads "track everything" and reaches for the tab-close. Nothing in the hero says local, single binary, or no server.
2. **Privacy is asserted once, at the bottom, in one sentence.** "The model and your history live on your machine." That is a claim. The source is proprietary, so "read the code" is unavailable as proof. Proof has to come from mechanism and from things the reader can check themselves (Tailscale, Mullvad, IVPN pattern).
3. **The hero screenshot's standup text reads as generated.** "Validate workspace detail sections with UX team for clarity and consistency" is the exact register that makes engineers distrust LLM products. The rest of the shot (ticket keys, repo names, 12m10s Terminator) reads real and is the strongest material on the page.
4. **No developer voice, no limitations, no scope.** Nothing says who built it, what platform it runs on today, what is rough. The disabled "Download" button is a dead end that says less than one honest sentence would.
5. **Seven near-identical screenshot-plus-paragraph sections.** Nothing changes register, so nothing stands out. The best simple pages have one section that is a different shape: a table, a terminal block, a list of "No X" (Helix, Mullvad, Bun).
6. **"How it works" describes but does not show.** Capture, derive, correct, use is right. It would land harder as one worked example: three raw events in, one task out.

## Principles

- Every claim gets a mechanism or a number. "Small" becomes 52 MB. "Private" becomes a file path and a command.
- Say what it captures and what it does not, in the same breath. Window title, app name, process id. No keystrokes, no screenshots, no clipboard, no screen recording.
- Say what is rough. Pre-download, Linux X11 only, grouping is a best guess you correct. Stated limits are the cheapest trust there is (Plausible's "when not to use", McWig's "lots of bugs").
- The page itself is evidence. No scripts, no analytics, no fonts fetched, no cookies. Say so in the footer (Bear Blog, Tarsnap).
- Keep everything James already cut out cut out: no fact cards, no separate feedback section. Fold the substance into one table and one signed note.

## Proposed structure

Order matters: usefulness first (the reader needs a reason to care), then mechanism, then the privacy table, then the note from the developer.

### 1. Hero

Headline candidates, in the Tarsnap register (short, specific, no adjectives):

- "Your day, reconstructed from window titles. On your disk, nowhere else."
- "A timeline of what you actually worked on. No account, no server, nothing sent anywhere."
- "Standup, written from what your computer saw. Not from memory."

Subhead, one line: "Chronicle watches which window is in front, groups it into tasks with a model that runs inside the app, and keeps all of it in one SQLite file in your home folder."

Under it, the "No" list, monospace, one line (Helix pattern):

`No account. No server. No telemetry. No cloud model. No screenshots. No keylogging.`

CTA row: one button, "Email me" (mailto). Replace the disabled Download with a plain status line: "Linux (X11) today. macOS and Windows are next. Ask for a build."

Hero image: keep the home screenshot, but retake it on a day whose standup draft reads like a human wrote it, or with a draft James has redrafted. This is the single most important pixel on the page.

### 2. The output (usefulness)

Keep the three timeline charts and the task card, reports, chat sections, but compress to two: "The day" (timeline views plus task card) and "The week and the questions" (reports plus chat). Lead each with a concrete result from real data rather than a feature name. Examples from the current screenshots:

- "Tuesday: 5h51m of deep work, 65 switches, 15:00 is the hour that fragments. Down 3h28m on last week."
- "'What did I work on this morning?' answered from the local database, by the local model."

Rize's framing is useful here: state the problem in the header ("Standup from memory is a guess"), then the output.

### 3. What it sees (mechanism)

One monospace block, the same shape as a log, showing the real pipeline. Three focus events in, one task out. This replaces the abstract "Derive" step with something an engineer can evaluate.

```
14:02:11  Terminator     ~/dev/contoso — git rebase -i
14:09:40  Google-chrome  ACME-10787 re-work · Jira
14:14:03  Sublime_merge  contoso — 3 conflicts
                ↓
task  ACME-10787 re-work   [contoso]   14:02–14:31   confidence 0.84
```

Below it, the four steps as they are now, each with its one technical line restored, but plainer than the version that was cut:

- Capture: X11 `_NET_ACTIVE_WINDOW` and window titles; idle from XScreenSaver. App name, title, process id. That is the whole record.
- Derive: Qwen3 1.7B, Q4_K_M, via llama.cpp, in-process on the CPU. Downloaded once from Hugging Face, then never again.
- Correct: rename, move, merge. Corrections are stored and the next pass reads them.
- Use: standup, report, task card, chat, all reading the same file.

### 4. What leaves your machine (proof)

A small table (Mullvad schema-table pattern). Every row is a fact from the code as of today.

| What | Where it lives | Network |
|---|---|---|
| Focus events, tasks, corrections, chat | `~/.local/share/chronicle/chronicle.db` (SQLite, FTS5) | none |
| The model | `~/.local/share/chronicle/models/Qwen3-1.7B-Q4_K_M.gguf`, 1.1 GB | one download, first run |
| Standup, reports, chat answers | generated in-process from that file | none |
| Browser tab titles | optional, from the stock ActivityWatch extension, over `127.0.0.1:5600` | loopback only |
| Ticket tracker, project folders | only the connections you add in Settings | to that server, only when you ask |
| Feedback | your mail client | when you hit send |

Then the verify-yourself lines (IVPN pattern, the one thing no comparable site actually does):

"Check for yourself. While Chronicle is running:
`ss -tnp | grep chronicle` shows one listener on 127.0.0.1. `tcpdump -i any -w chronicle.pcap` records nothing after the model is downloaded. Or pull the cable. Everything keeps working."

Numbers to put alongside, since the page currently has none:

- Binary: 52 MB, one file, no runtime.
- Eleven days of my own use: 8,236 focus events, 1.6 MB database.
- Model: 1.1 GB on disk, runs on CPU.

Because the license is proprietary, do not write "open source" anywhere. The table and the commands carry the proof instead.

### 5. Note from the developer (tone)

Short, signed, first person, near the bottom (Overcast's "A Normal Business", Plausible's signed footer). Draft:

"I built this because my standup was a guess and my timesheet was fiction. It is one person's project. There is no company behind it and no plan for one that involves your data: there is no server to send it to. It runs on my Linux machine every day; macOS and Windows capture are written against the native APIs but not yet shipped. Task grouping is a best guess and you will correct it, which is why the correction path is two clicks. If it would be useful to you, or if it would need something else first, tell me. I read every reply.

James Clarke · james.clarke@callplaybook.com"

This replaces the feedback section that was cut, without being a section about feedback.

### 6. Footer

Keep the copyright line. Add one line that makes the page its own proof: "This page: static HTML, no scripts, no cookies, no analytics, no fonts fetched." Verify it stays true before shipping (currently true).

## Visual changes, kept small

- Introduce one monospace face for the "No" line, the pipeline block, the table, and the numbers. System monospace, no font download. It signals engineer without a redesign.
- Keep the dark palette and screenshot treatment. The three-up timeline grid is good.
- Section rhythm: screenshot sections alternate with the two text-shaped sections (pipeline block, table), so the page changes shape twice.
- Drop `section + section` borders where a background-shift does the same job for the two text sections.

## Not doing

- No testimonials, badges, or press quotes. None exist and fakes would be spotted.
- No pricing page yet. Pre-download; say so.
- No "open source" claim.
- No re-adding the fact cards or the feedback block as they were.

## Implementation, in order

1. Retake the home screenshot with a non-generic standup draft.
2. Hero copy, "No" line, status line replacing the dead button.
3. Pipeline block and restored one-line technical notes in "How it works".
4. "What leaves your machine" table, verify-yourself commands, three numbers.
5. Developer note and footer line.
6. Compress the six screenshot sections to four.
7. Mobile pass at 360 and 400 wide; confirm the table scrolls inside its own container.

Each step is one commit. Steps 2 through 5 touch only `site/index.html` and `site/style.css`.
