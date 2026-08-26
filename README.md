# Chronicle

Local-first, privacy-driven activity tracker. Captures window/app focus events, derives "what was I working on" with an embedded local LLM, shows a correctable timeline + chat. Zero cloud. Zero telemetry. Single binary.

## How this doc works

Everything below is the current plan, not hard rules.

## Stack

| Area | Choice |
|---|---|
| Language | Rust stable, edition 2024, cargo workspace |
| UI | egui via `eframe` (no webview) |
| Tray: **mac/win only** | `tray-icon` |
| Storage | `rusqlite` (`bundled` + FTS5), WAL, `rusqlite_migration` |
| Time | `jiff` (IANA tzdb, DST-correct; `time`'s local-offset lookup is unsound in threaded daemons) |
| LLM | llama.cpp embedded via `llama-cpp-2` (pin exact version; no semver) |
| Model | Qwen3-1.7B Q4_K_M GGUF, non-thinking mode (`/no_think`) |
| HTTP | `axum`, `127.0.0.1` only |
| MCP | `rmcp` (official SDK, pin version), stdio only |
| Linux capture | `x11rb` (X11 only) |
| Windows capture | `windows` crate |
| macOS capture | `objc2`/`objc2-app-kit`; `macos-accessibility-client` (AX trust prompt); `objc2-application-services` or `axuielement` (AXObserver) |
| Concurrency | sync threads + `crossbeam-channel`; tokio confined to `server` + `mcp` |

## Platform matrix

| | Linux (X11 only) | macOS | Windows |
|---|---|---|---|
| Focus events | `_NET_ACTIVE_WINDOW` + per-window `PropertyChangeMask` | `NSWorkspace.didActivateApplicationNotification` + AXObserver | `SetWinEventHook` (FOREGROUND + NAMECHANGE) |
| Titles | `_NET_WM_NAME` → `WM_NAME`; app = `WM_CLASS` | AX `kAXTitle` | `GetWindowTextW`; app = `QueryFullProcessImageNameW` |
| AFK | XScreenSaver `QueryInfo` | `CGEventSourceSecondsSinceLastEventType` | `GetLastInputInfo` |
| Screen lock | logind D-Bus `LockedHint` | `com.apple.screenIsLocked` notification | WTS session notifications |
| Tray | **none** | yes | yes |
| Open UI | app icon / `chronicle toggle` | tray click | tray click |
| Autostart | `~/.config/autostart` `.desktop` | LaunchAgent plist | HKCU `Run` key |
| LLM accel | CPU (Vulkan opt) | Metal | CPU (Vulkan opt) |
| Battery guard | `/sys/class/power_supply` | IOKit (or skip v1) | `GetSystemPowerStatus` |

Platform notes:
- **Linux launch UX:** daemon holds a unix socket. Any second invocation (`chronicle` or `chronicle toggle`) sends toggle and exits; daemon spawns the UI child, or forwards a raise if it's already alive. No tray.
- **X11:** windows die racily, all property reads must tolerate `BadWindow`/`BadDrawable` as non-fatal. Subscribe `PropertyChangeMask` on each new active window (catches tab-title changes), unsubscribe previous. Debounce title changes 1 s.
- **macOS:** AX permission is a hard gate. Detect via `AXIsProcessTrustedWithOptions`, onboarding screen with deep link to System Settings, degrade to app-only tracking until granted. Message clearly: titles via AX, **no screen recording**. Daemon owns the main-thread run loop (required for NSWorkspace + AXObserver anyway) and hosts the tray; tray-icon must be created on the main thread after the event loop starts (`StartCause::Init`). UI stays a child process.
- **Windows:** dedicated capture thread with `GetMessage` pump, `WINEVENT_OUTOFCONTEXT`; tray shares the daemon's message pump. UI stays a child process.

## Non-functional requirements

- Daemon is headless. UI, derivation, and chat are all child processes of the same binary: UI = `ui` child spawned by `toggle` (spawn-or-raise), exits fully on window close (crash isolation: a GPU/driver failure can't take down capture); derivation = ephemeral worker; chat = warm worker, killed on panel close. Model never resident in the daemon.
- Daemon RSS < 50 MB. UI-child RSS is measured but not bound by this. Cost accepted: ~100–300 ms cold open on toggle.
- Event-driven capture. Only polls: AFK check ≤ 1/30 s, scheduler tick.
- egui never repaints while idle: no unconditional `request_repaint()` in `update()`; background threads wake the UI via the repaint callback.
- All servers bind `127.0.0.1` **and** strictly validate the `Host` header (DNS-rebinding defense: ActivityWatch shipped CVE-2022-31149 for exactly this).
- Platform code isolated behind traits: `FocusProvider`, `AfkProvider`, `LockSignal`, `Autostart`, `ShellIntegration` (tray/activation). Everything downstream consumes `CaptureEvent` only.

## Workspace

```
chronicle/
├── crates/
│   ├── core/       # types, config, storage, sessionizer, digest
│   ├── capture/    # provider traits + platform impls (#[cfg])
│   ├── server/     # AW-compatible localhost HTTP (axum)
│   ├── derive/     # digest→prompt, llama runner, GBNF, corrections
│   ├── mcp/        # rmcp client wrapper
│   └── app/        # binary: clap, shell integration, egui UI
├── grammars/task_output.gbnf
├── prompts/derive_v1.txt
└── fixtures/       # recorded JSONL event streams for tests
```

Subcommands: `run` (daemon, default) · `ui` (internal: egui window process, spawned by daemon) · `derive --batch <id>` (ephemeral worker) · `chat-worker` · `toggle` (spawn-or-raise UI via socket) · `dump [--day YYYY-MM-DD]`.

## Capture core

```rust
pub struct FocusEvent { ts: jiff::Timestamp, app: String, title: String, pid: Option<u32> }
pub enum CaptureEvent { Focus(FocusEvent), TitleChanged(FocusEvent), Afk { idle: bool, ts: jiff::Timestamp } }
pub trait FocusProvider: Send { fn run(self, tx: Sender<CaptureEvent>) -> Result<()>; } // blocking loop, own thread
pub trait AfkProvider:  Send { fn idle_ms(&self) -> Result<u64>; }                       // polled
```

## Storage

SQLite, WAL. Tables: `events`, `spans`, `batches`, `tasks`, `corrections`, `chat_messages`; FTS5 over `spans.title` + `tasks.label`. Data dir: XDG / `%APPDATA%` / `~/Library/Application Support`.

- **Time:** all stored timestamps are UTC unix milliseconds (INTEGER); no tz in storage, ever. Day windows (picker, `dump --day`, digest) = local civil day via jiff at query time: DST days are 23/25 h and that's correct. Timestamp once at capture, never re-stamp downstream.
- **Retention:** configurable window (default 180 days). `PRAGMA auto_vacuum=INCREMENTAL` at DB creation, **before the first table** (can't be flipped later without a full VACUUM rebuild). Prune job runs in the scheduler's idle gate: batched deletes (~1000 rows/tx), FTS index fed the deletes, then `incremental_vacuum(N)` with small N per tick + occasional `wal_checkpoint(TRUNCATE)`.
- **At rest:** plaintext SQLite in v1, documented. SQLCipher opt-in is v1.1.
- Excluded apps/titles (regex, in settings) are never stored at all.

## Sessionizer + digest (deterministic, no LLM)

- Merge consecutive events (same app, title-similarity ≥ threshold) → spans. Collapse spans < 5 s into a `context-switching` span. Close spans on AFK ≥ 120 s (configurable).
- Batch = 30 min of non-AFK activity (configurable). Digest ≤ 3 K tokens: top apps by duration, top titles per app, URL domains + first path segment, switch count, AFK total, last 3 task labels (continuity), top-k similar past corrections via FTS (keyed on title **and** app/domain, count-capped), MCP context.

## Derivation

- Ephemeral worker: mmap model → infer → write tasks → mark batch done → exit. Hard timeout 5 min → mark failed, retry once at next idle.
- GBNF-constrained JSON: `{ "tasks": [ { label, project|null, start_offset_min, end_offset_min, confidence } ] }`. Use bounded repetition `x{0,N}` in the grammar, never chained `x? x?` (pathologically slow).
- Threads = physical cores − 1, cap 8. Features: `metal` / `vulkan` / CPU fallback, wire the feature gating at M4 so ports are just `#[cfg]`.
- Model manager: download to data dir, SHA-256 verify, resume; first-run progress UI. Never bundled in installer.
- **M4 gate:** benchmark Qwen3-1.7B vs Qwen3-4B-Instruct-2507 on fixture evals before pinning the default.
- Scheduler: derive when AFK ≥ 5 min OR screen locked OR low 1-min load. Defer on battery < 30 %. Queue drains oldest-first, one worker at a time. Manual "derive now" button.

## Browser URLs, ActivityWatch-compatible endpoint

`axum` on `127.0.0.1:5600` (AW default so stock extensions work; configurable; port conflict → log + UI warning). Minimum for stock `aw-watcher-web`:

- `GET /api/0/info` · `GET/POST /api/0/buckets/{id}` (304 if exists) · `POST /api/0/buckets/{id}/heartbeat?pulsetime=`
- Heartbeat merge semantics: identical `data` within `pulsetime` seconds → extend previous event's duration; else new event.
- CORS: hardcode stock Firefox/Chrome extension origins + configurable regex for sideloads.
- Strict `Host` check (`127.0.0.1:5600` / `localhost:5600`), reject everything else.
- Map heartbeats → `events(kind='url', url, title, app='browser:<name>')`.

## MCP context

stdio transport only. TOML config with explicit allowlisted `context_calls` (tool + `args_json`), no dynamic tool selection in v1. At derive time: run allowlisted calls, 10 s timeout, truncate ≤ ~800 tokens, inject as `## Workspace context`. Failures non-fatal.

**All MCP/title/URL text is untrusted labeling data.** Grammar-constrained output is the containment, the model can only emit task JSON. Never act on instructions embedded in captured or fetched text. No MCP from chat in v1.

## Chat

Panel open → spawn `chat-worker` (warm llama session over unix socket/stdio), keep alive; kill on close. Retrieval is local-DB only: parse time refs → SQL range, else FTS over labels/titles top-k. Context ≤ 3 K tokens, tokens streamed via channel.

## UI

- **Timeline:** day picker, task blocks (label, project, confidence tint), raw-spans toggle, virtual scrolling.
- **Task card:** edit label/project → writes `corrections`. Split/merge deferred.
- **Chat panel:** dockable right.
- **Onboarding:** model download progress, autostart opt-in, macOS AX flow.
- **Settings:** batch length, idle threshold, exclusions, model path/choice, MCP config path, port.

## Conventions

`anyhow` in binaries, `thiserror` in libs · `tracing` + rotating file log (5 MB) · `rust-toolchain.toml` pins stable · CI: fmt + clippy + tests on Linux (3-OS matrix from M9) · `cargo clippy -- -D warnings` + `cargo fmt` clean at every milestone · fixture-driven tests: mock `FocusProvider` replays JSONL; sessionizer/digest have golden-output tests · every milestone ends with tests passing, `chronicle dump` demonstrating the capability, and a short `docs/mNN-notes.md`.

## Milestones

Order: **Linux (X11) → macOS → Windows.**

| M | Deliverable | Acceptance |
|---|---|---|
| 0 | Workspace, core types, config (TOML), SQLite + migrations, logging, `dump`, toolchain pin + CI | `chronicle dump` on empty DB |
| 1 | X11 capture + AFK; daemon writes events | 10 min of real window/tab switching → correct app/title/AFK stream in `dump`; RSS < 30 MB |
| 2 | Sessionizer + digest + golden tests | fixture-day digest matches golden; ≤ 3 K tokens |
| 3 | egui timeline (spans only) as `ui` child + single-instance `toggle` socket | idle CPU ~0 % with window open; toggle spawns/raises UI child; window close exits it fully, daemon RSS unchanged |
| 4 | Derivation end-to-end + **model benchmark gate** | work 30 min → go idle → tasks appear; daemon RSS unchanged |
| 5 | Corrections loop (FTS few-shot) | a correction changes the next batch's output on a crafted fixture |
| 6 | AW endpoint + Host/CORS hardening | stock AW extension → per-site spans, not just "firefox" |
| 7 | Chat | "what did I do this morning?" answers grounded in DB |
| 8 | MCP | with a jira MCP server, derived labels reference ticket keys |
| 9 | **macOS port:** capture, AX onboarding + degraded app-only mode, tray, LaunchAgent, `metal`; ad-hoc sign + documented right-click-open | M1–M8 acceptance re-run on macOS |
| 10 | **Windows port:** capture thread, tray, HKCU autostart, power guard, WTS lock | same re-run on Windows |
| 11 | Packaging: Linux `.desktop` + tarball/AppImage; macOS `.app`; Windows installer | clean install on all three |

Get the $99 Apple Developer account **before M9** so notarization is a v1.1 config flip, not a scramble.

## Deferred (v1.1+)

- **Wayland**: awatcher-style provider matrix: `wlr-foreign-toplevel-management` (Sway/Hyprland/wlroots), KWin script (KDE), Focused-Window-D-Bus GNOME extension, `ext-idle-notify-v1` AFK; runtime provider selection on Linux.
- Global hotkey (X11 grab; GlobalShortcuts portal where sane).
- Linux tray (`ksni`, best-effort, never load-bearing).
- Task split/merge UX.
- SQLCipher at-rest encryption.
- Signing: Apple notarization; Windows OV/EV cert (SmartScreen).
- MCP from chat; dynamic tool selection.

## License

Closed source. All rights reserved, see `LICENSE`.
