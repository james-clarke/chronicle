# M39 — Wayland: wlroots and KDE behind the existing traits

Status: written 2026-09-10 on James's standing "grab next task and
execute" instruction; rationale in `dev-tools-direction.md` ("Platforms
and distribution", the M39 line). Runs after M38. Every "today" claim
carries a `file:line`.

## The short version

Chronicle on Linux captures X11 only: `spawn_capture` constructs the X11
providers unconditionally and the daemon fails to start on a Wayland
session (no `DISPLAY`, or an Xwayland `DISPLAY` that sees only X clients).
Wayland is 60–80 % of Linux sessions. This milestone adds two focus routes
behind `FocusProvider` — `wlr-foreign-toplevel-management` for wlroots
compositors (Sway, Hyprland, Niri, river, labwc) and a KWin script over
D-Bus for KDE Plasma — one idle route for both (`ext-idle-notify-v1`, with
the older `org_kde_kwin_idle` as fallback), and picks the route at start
from the session environment. Lock stays logind. Presence counts stay off
on Wayland: no protocol reports input to an ordinary client. GNOME needs a
Shell extension the user installs and stays a later milestone, but the
daemon must say so in one line instead of dying on a missing global.

This box runs X11 (`XDG_SESSION_TYPE=x11`) with no compositor installed.
Debian 13 ships `sway 1.10.1` (wlroots 0.18: both protocols) and
`kwin-wayland 6.3.6`; each runs nested inside an X11 window, which is the
runtime verification route. Installing them needs sudo, so it is James's
call (see "Owed"). Without them the milestone ships as M38 did: compiled,
fixture-tested, unverified at runtime.

Gate from the direction doc: none stated beyond "wlroots and KDE behind
the `FocusProvider` trait, `ext-idle-notify-v1`, logind lock as now". The
gate here: on a nested Sway session the daemon starts without X11, the
focus stream shows app_id/title switches and the AFK threshold fires from
`ext-idle-notify`; on nested KWin the same, with pids and therefore cwd
rows. The X11 path is unchanged: same tests, same install.

## What the code does today

- The four provider traits are in `crates/capture/src/lib.rs`:
  `FocusProvider` (lib.rs:41), `LockSignal` (lib.rs:48),
  `PresenceProvider` (lib.rs:56), `AfkProvider` (lib.rs:61, polled at most
  every 30 s by `afk_loop`, capture.rs:710). Linux implementations are X11
  (`x11.rs`), XI2 (`presence.rs`) and logind (`lock.rs`), all behind
  `cfg(target_os = "linux")` (lib.rs:15-30).
- The Linux `spawn_capture` (capture.rs:14-30) builds `X11FocusProvider`
  and `X11AfkProvider` with `?`: a Wayland session with no `DISPLAY` fails
  the daemon at start; one with Xwayland starts and sees only X clients.
- The X11 focus provider is event-driven (`_NET_ACTIVE_WINDOW` property
  notify, x11.rs:207), debounces titles 1 s trailing-edge (x11.rs:33) and
  emits `FocusEvent { app, title, pid }` (types.rs:4). The pid comes from
  `_NET_WM_PID` (x11.rs:193) and feeds the terminal cwd probe
  (`probe_cwd`, x11.rs:100; `terminal_cwd` walks `/proc`, x11.rs:53),
  which is the `ActivityKind::Cwd` row placement reads (extract.rs:735,
  anchor.rs:113). The same `State` (x11.rs:41) owns the debounce and the
  cwd `(pid, place)` memo; it is private to `x11.rs`.
- Without a pid the terminal still places through the shell hook's `Shell`
  rows (extract.rs:735 accepts `Shell | Cwd`), which is the M37 route James
  already installed and the documented WSL2 answer.
- Idle on X11 is `MIT-SCREEN-SAVER` (x11.rs:323). Lock is logind over the
  system bus with `zbus` blocking (lock.rs:8, :25), independent of the
  display server.
- `zbus 5` is already a workspace dependency (Cargo.toml:27); no Wayland
  crate is in the tree. `wayland-client 0.31` defaults to the pure-Rust
  backend, so no `libwayland` at build time and no new CI package.
- Config has `capture_presence` (config.rs:35) and `afk_close_secs`; no
  route override.
- README calls Linux capture "X11 only" (README.md:22, :29) and lists the
  Wayland provider matrix under future work (README.md:324).
- CI runs an ubuntu job and a macos job (`.github/workflows/ci.yml`).

## Decisions

- **Route from the environment, once, at start.** `WAYLAND_DISPLAY` set →
  Wayland; then `XDG_CURRENT_DESKTOP` containing `KDE` (or
  `KDE_SESSION_VERSION`) → the KWin route; otherwise connect and look for
  `zwlr_foreign_toplevel_manager_v1` in the registry. Neither → a
  one-line error naming the compositor (`XDG_CURRENT_DESKTOP`) and the
  reason ("GNOME Wayland needs the Chronicle Shell extension, not shipped
  yet"); `spawn_capture` keeps its bail semantics because focus is the
  load-bearing provider (capture.rs:57). A `focus_route` config key
  (`auto | x11 | wlr | kwin`, default `auto`) covers Xwayland-only setups
  and testing; not surfaced in Settings.
- **wlroots: event-driven, like X11.** One `wayland-client` connection on
  the focus thread, `blocking_dispatch` in the loop. State per toplevel
  handle: app_id, title, activated. `Focus` on the activated handle
  changing (or the current one closing), `TitleChanged` on the active
  handle's title, with the same 1 s trailing-edge debounce as X11 — a
  `dispatch` with a 1 s timeout replaces the `poll_for_event` replay.
  `app` is the app_id (the WM_CLASS equivalent, so `extract::family`
  matches as before).
- **The protocol carries no pid; the compositor's IPC does.** Sway
  (`$SWAYSOCK`, i3 IPC framing, `GET_TREE`), Hyprland
  (`$HYPRLAND_INSTANCE_SIGNATURE`, `.socket.sock`, `j/activewindow`) and
  Niri (`$NIRI_SOCKET`, `"FocusedWindow"`) each answer the focused
  window's pid in one request over a unix socket, no subprocess. Looked up
  on each `Focus` event, matched by app_id and title, `None` when no
  socket is set or the answer does not match. That restores the cwd probe
  on the three compositors that matter; elsewhere the shell hook carries
  placement.
- **KDE: a KWin script that calls back over D-Bus.** KWin exposes
  `org_kde_plasma_window_management` only to its own shell clients, so the
  protocol route is closed; `awatcher` and `kdotool` use scripting and so
  does this. The daemon owns `dev.chronicled.Chronicle1` on the session
  bus (zbus blocking server, object `/dev/chronicled/Chronicle1`, method
  `Focus(pid: u, app: s, title: s)`), writes the script to
  `$XDG_RUNTIME_DIR/chronicle/kwin-focus.js`, loads it through
  `org.kde.KWin /Scripting loadScript(path, "chronicle")` and starts it
  (`/Scripting/Script<id>` on Plasma 6, `/<id>` on Plasma 5, as awatcher
  handles both), and unloads it on exit. The script feature-detects
  `workspace.windowActivated` (Plasma 6) against `clientActivated`
  (Plasma 5), connects `captionChanged` on the active window, and calls
  `callDBus` with `pid`, `resourceClass`, `caption`. The pid makes the cwd
  probe work as on X11. The focus thread's `run` blocks on the channel
  the D-Bus interface feeds and applies the shared debounce.
- **One focus state machine, shared.** `State`, `terminal_cwd` and
  `probe_cwd` move out of `x11.rs` into `crates/capture/src/cwd.rs` and a
  new `wayland/focus.rs` holds the pure "raw event in, `CaptureEvent`s
  out" state (activated / title / closed, debounce as a timestamp
  comparison so tests need no sleep). Both Wayland routes drive it; X11
  keeps its own loop and only takes the moved cwd helpers. Smallest diff
  that lets the new logic be unit-tested without a compositor.
- **Idle through `ext-idle-notify-v1`, polled shape kept.**
  `AfkProvider::idle_ms` is a poll; the provider holds its own connection,
  registers one notification with a 1 s timeout, and on each call flushes,
  reads what is pending without blocking, and returns 0 when not idle or
  `1000 + elapsed since the idled event`. 1 s resolution against a
  threshold in minutes. When the global is absent, `org_kde_kwin_idle`
  (same two events, wlroots < 0.16 and KWin < 5.27); when both are absent,
  `new()` fails and the daemon runs without AFK as it does when
  `MIT-SCREEN-SAVER` is missing today.
- **Presence off on Wayland.** Input counts need evdev read access (the
  `input` group), which no packaged install grants. `spawn_presence_capture`
  logs once ("presence counts unavailable on Wayland; idle still works")
  and returns. An evdev route when `/dev/input` is readable is a later
  chunk, not this one.
- **Lock unchanged.** logind reports lock on every session type.
- **Surface the route.** The daemon logs the route at start; the control
  socket `status` reply gains `focus_route` and `chronicle status` prints
  it on the capture line; the Settings capture card shows the same word.
  Nothing else in the UI changes.
- **Verification by nested compositors, not a fake server.** A
  `wayland-server` test compositor would be a chunk on its own. Nested
  Sway (`WLR_BACKENDS=x11 sway -c <minimal config>`) and nested KWin
  (`kwin_wayland --xwayland --x11-display :0`) run in an X11 window here;
  `scripts/wayland-check.sh` starts one, points a sandbox-copy daemon at
  it (`XDG_DATA_HOME` sandbox per the bench recipe), opens two terminal
  windows, switches focus and prints the focus rows. Unit tests cover the
  state machine, the three IPC parsers on JSON fixtures, the KWin script
  text (both API names present, the D-Bus triple correct) and the D-Bus
  interface through a peer-to-peer zbus pair.

## Chunks and gates

### 0. Route selection and scaffolding

- `crates/capture/src/wayland/mod.rs` behind `cfg(target_os = "linux")`:
  `Route`, `Choice`, `choose` (the environment half of the decision) and
  the `unsupported` error text. The submodules arrive with their chunks
  rather than as stubs.
- `cwd.rs`: `State`'s cwd half, `terminal_cwd`, `probe_cwd` moved from
  `x11.rs`; X11 unchanged in behaviour.
- Linux deps land with the chunk that uses them, not here:
  `wayland-client 0.31` and `wayland-protocols-wlr 0.3` in chunk 1,
  `wayland-protocols 0.32` (staging, for `ext-idle-notify-v1`) and
  `wayland-protocols-plasma 0.3` (the `org_kde_kwin_idle` fallback) in
  chunk 2.
- `focus_route` config key; `spawn_capture` (Linux) matches the route and
  builds the pair of providers; the GNOME/unknown error text.
- Gate: Linux tests and clippy unchanged on X11; `focus_route = "wlr"` on
  this box fails with the "no Wayland display" line, not a panic.

### 1. wlroots focus

- `WlrFocusProvider`: registry bind of the toplevel manager, per-handle
  state, `Focus`/`TitleChanged` through the shared state machine, the 1 s
  debounce as a dispatch timeout, the closing of the active toplevel as a
  focus change to "nothing" (same as X11's `_NET_ACTIVE_WINDOW = 0`).
- Tests: state machine sequences (activate A, retitle A twice within 1 s
  → one `TitleChanged`; activate B; close B → focus to A if A is the
  only remaining activated, else empty).
- Gate: unit tests; on nested Sway, two `foot` windows switched five
  times give five `Focus` rows with the right app_id and titles.

### 2. Idle

- `WaylandAfkProvider` over `ext_idle_notifier_v1`, fallback
  `org_kde_kwin_idle`, the non-blocking read on each `idle_ms`.
- Gate: on nested Sway with `afk_close_secs = 60`, hands off the keyboard
  for 70 s → one AFK close in the ledger, resumed on the next key.

### 3. KDE focus

- The D-Bus interface, the script text, load/start/unload against
  `org.kde.KWin`, Plasma 5/6 object paths, `KwinFocusProvider::run`.
- Tests: script text; the interface over a zbus peer-to-peer connection
  (a `Focus` call lands as one `FocusEvent`).
- Gate: on nested KWin the same five switches, each row with a pid and a
  `Cwd` row for the terminal windows.

### 4. pid through compositor IPC

- `ipc.rs`: Sway/i3, Hyprland, Niri clients (one request each), fixtures
  for the three replies, match by app_id + title.
- Gate: on nested Sway the terminal rows carry pids and `Cwd` rows appear
  as they do on X11.

### 5. Surface and docs

- `focus_route` in the `status` reply and `chronicle status`; the Settings
  capture card word; the presence log line; `scripts/wayland-check.sh`.
- README platform matrix: Linux X11 / Wayland (wlroots, KDE) / GNOME
  later, the presence note; direction doc M39 line ticked; shipped notes
  here.
- Gate: `chronicle status` on this box prints `focus: x11`.

## Owed

- `sudo apt install sway kwin-wayland foot` here, or the runtime gates
  above stay unproven (James).
- GNOME: the Shell extension route (M4x per the direction doc).
- Presence on Wayland through evdev when `/dev/input` is readable.
- Compositors without `wlr-foreign-toplevel` (older Niri and river builds)
  get the same one-line error as GNOME; an IPC-only focus route for them
  is not planned.
