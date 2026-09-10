# M38 — macOS: the same daemon on the second-largest developer platform

Status: written 2026-09-10 on James's standing "grab next task and
execute" instruction; rationale in `dev-tools-direction.md` ("Platforms
and distribution"). Runs after M37. Every "today" claim carries a
`file:line`.

## The short version

Chronicle captures on Linux X11 only. macOS is ~33 % of professional
developers and the direction doc puts it first in the platform order. This
milestone ports every collector that has a Linux-only body to macOS behind
the traits that already exist, gives the daemon a menu-bar icon and a
LaunchAgent, and wires cargo-dist so a release produces a Homebrew formula.
No Mac is attached to this box: the port is written against the vendored
crate sources, cross-checked with `cargo check --target
x86_64-apple-darwin` for the crates that do not build llama.cpp, and
compile-gated by a new macOS CI job. Runtime verification on a Mac is owed
and listed at the end.

Gate from the direction doc: a clean Mac reaches Home with projects
populated from the repos under `~/dev` in under five minutes from
`brew install`. What this box can prove: the macOS branch compiles, every
parser and file generator has a fixture test, and the Linux daemon is
unchanged (same tests, same install).

## What the code does today

- Providers are traits in `crates/capture/src/lib.rs`: `FocusProvider`
  (lib.rs:40), `LockSignal` (lib.rs:47), `PresenceProvider` (lib.rs:55),
  `AfkProvider` (lib.rs:60). Only X11 and logind implement them
  (`x11.rs`, `presence.rs`, `lock.rs`), all `#[cfg(target_os = "linux")]`
  (lib.rs:15-30).
- `spawn_capture` (crates/app/src/capture.rs:14) is Linux-only; the other
  platforms hit `bail!("capture on this platform lands in M9/M10")`
  (capture.rs:583). Lock, presence and mic spawns are Linux-only too
  (capture.rs:166, :211, :260); git, notes, AI sessions, ports, GitHub,
  GitLab, shell, tmux/docker, browser, ICS and Google Calendar are
  platform-neutral.
- The focused terminal's cwd walks `/proc` for the newest child shell
  (x11.rs:54); ports walk `/proc/net/tcp` and `/proc/<pid>/cwd`
  (ports.rs:59) and emit nothing elsewhere (ports.rs:115); the mic watcher
  parses `pw-dump` (mic.rs:38).
- Tray: a ksni StatusNotifierItem on a background tokio thread
  (daemon.rs:237); left click and "Show/Hide" send `CtrlMsg::Toggle`,
  "Quit" sends `Shutdown`. The icon is procedural ARGB (daemon.rs:214).
- Service: the onboarding card writes a systemd user unit from
  `packaging/chronicle.service` and enables it without `--now`
  (ui/onboarding.rs:128); no CLI.
- Data dir is already per platform through `directories`
  (core/src/lib.rs:31); the control socket is a unix socket under it
  (daemon.rs:41), which macOS has.
- The browser for OAuth is `xdg-open` (capture.rs:484).
- `metal` is a feature on `chronicle` and `chronicle-derive`
  (crates/app/Cargo.toml, crates/derive/Cargo.toml) that nothing turns on.
- CI is one ubuntu job (`.github/workflows/ci.yml`); no releases, no
  cargo-dist.

## Decisions

- **Poll, do not observe.** The X11 provider is event-driven; the macOS one
  polls `NSWorkspace.frontmostApplication` and the AX focused window's
  title once a second. X11 already debounces titles for 1 s (x11.rs:33),
  so nothing downstream sees a coarser signal, and polling needs no
  main-thread run loop, so the capture threads keep the Linux shape.
- **C ABI by hand, Objective-C through objc2.** `CGEventSource*`,
  `CGSessionCopyCurrentDictionary`, `AX*` and `AudioObjectGetPropertyData`
  are declared as `extern "C"` in one `ffi.rs` with `#[link(kind =
  "framework")]`; `NSWorkspace`/`NSRunningApplication` come from
  `objc2-app-kit`, CoreFoundation values from `objc2-core-foundation`.
  The bindings crates for the C frameworks would add build time for four
  functions.
- **Lock by polling the session dictionary** (`CGSSessionScreenIsLocked`,
  2 s) rather than the distributed notification, which needs a run loop on
  its thread. `LockSignal` fires on every edge either way.
- **Terminal cwd through libproc** (`proc_listchildpids`,
  `proc_pidinfo(PROC_PIDVNODEPATHINFO)`), already bound in `libc`; the
  `login` wrapper Terminal.app and iTerm2 insert is stepped through.
- **Ports through `lsof`** (`-iTCP -sTCP:LISTEN -Fpn`, then `-d cwd` for
  those pids): one call a minute, no root. The parser is pure and tested
  here.
- **Mic through CoreAudio** `kAudioDevicePropertyDeviceIsRunningSomewhere`
  on the default input device; the capturing app is not knowable without
  a TCC-gated tap, so the span's app is "microphone". `MicProvider` becomes
  generic over a `MicSource` so the span logic is shared.
- **Tray through `tray-icon`** on the main thread: on macOS `daemon::run`
  moves its loop to a `daemon-main` thread and the main thread runs
  `NSApplication` with activation policy Accessory (no Dock icon). Menu
  and click events arrive on channels and map to the same `CtrlMsg`s.
- **LaunchAgent** `dev.chronicled.chronicle` under `~/Library/LaunchAgents`,
  `RunAtLoad`, `KeepAlive { SuccessfulExit = false }` (the `Restart=on-failure`
  equivalent), `ProcessType Interactive`, `LimitLoadToSessionType Aqua`, a
  PATH that includes Homebrew and `~/.cargo/bin`, stderr to
  `<data dir>/logs/launchd.log`. A `chronicle service install|remove|status`
  CLI on both platforms; the onboarding card calls the same code.
- **AX permission is a card, not a blocker.** Without Accessibility the
  focus provider still emits app names with empty titles and logs once;
  the card (Home while untrusted, Settings › Connections after) opens
  System Settings › Privacy & Security › Accessibility.
- **EventKit is deferred.** ICS subscriptions (M37) are the calendar route
  on every platform; EventKit needs an entitlement and a TCC prompt for
  what an ICS URL gives without either.
- **Metal on by target**, not by feature flag: `chronicle-derive` enables
  `llama-cpp-2/metal` under `[target.'cfg(target_os = "macos")']`.
- **cargo-dist** with the shell installer and a Homebrew formula in
  `james-clarke/homebrew-tap`; signing and notarization stay a documented
  secret set (`dist` supports them natively) until James has the Developer
  ID.

## Chunks and gates

### 0. Scaffolding

- `crates/capture/src/macos/` module tree behind `cfg(target_os = "macos")`
  with the names the app crate will call: `focus::MacFocusProvider`,
  `input::{MacAfkProvider, MacPresenceProvider}`, `lock::MacLock`,
  `ax::trusted(prompt)`, `proc::{cwd, terminal_cwd}`, `lsof`, `mic::CoreAudioMic`,
  `ffi`.
- Per-target dependencies: `objc2`, `objc2-foundation`, `objc2-app-kit`,
  `objc2-core-foundation`, `libc` for capture; `tray-icon` for the app;
  `ksni` and the tokio tray runtime become Linux-only.
- `spawn_capture` gains a macOS body that mirrors the Linux one; lock,
  presence and mic spawns get macOS arms; `xdg-open` becomes `open`.
- `scripts/mac-check.sh`: the cross-check recipe (a fake `clang` that emits
  empty objects so `libsqlite3-sys`, `ring` and `aws-lc-sys` build scripts
  pass; `cargo check --target x86_64-apple-darwin` on capture, core, server,
  mcp). The app and derive crates cannot cross-check here: llama.cpp's
  cmake needs a real SDK.
- CI: a `macos-latest` job running clippy and the tests.
- Gate: Linux tests and clippy unchanged; `scripts/mac-check.sh` passes
  once chunks 1–2 land.

### 1. Focus, idle, presence, lock

- `MacFocusProvider`: 1 s poll of the frontmost application (name, bundle
  id, pid) and its AX focused window title; a `FocusEvent` on app change, a
  `TitleChanged` on title change, the cwd probe on terminals as in
  `x11.rs:97`.
- `MacAfkProvider::idle_ms` from
  `CGEventSourceSecondsSinceLastEventType(CombinedSessionState, AnyInput)`.
- `MacPresenceProvider`: per-second deltas of
  `CGEventSourceCounterForEventType` for key down, mouse buttons, motion
  and scroll, folded into `PresenceMinute` exactly as `presence.rs:60`.
- `MacLock`: `CGSessionCopyCurrentDictionary()["CGSSessionScreenIsLocked"]`
  every 2 s.
- Gate: cross-check passes; the fold logic that can run here (minute
  bucketing, edge detection) is unit-tested.

### 2. Terminal cwd, ports, mic

- `proc.rs`: `cwd(pid)` and `terminal_cwd(pid)` (newest child by start
  time, `login` stepped through). The "newest child" choice is a pure
  function over `(pid, name, start)` rows and tested here.
- `lsof.rs`: parsers for the two `-F` outputs and the `listeners()` body
  for macOS (`ports.rs:115` routes to it).
- `MicSource` trait in `mic.rs` (`fn active_inputs(&mut self) ->
  Result<Vec<String>, String>`), `PwDump` on Linux, `CoreAudioMic` on
  macOS; `mic.rs` compiles on both.
- Gate: fixture tests for the lsof parsers and the child pick; Linux mic
  tests unchanged.

### 3. Menu-bar icon

- `crates/app/src/tray_macos.rs`: `run_main(ctrl_tx) -> !` builds the
  status item (template icon from the same pixel routine, "Show/Hide",
  "Quit"), forwards click and menu events to `CtrlMsg`, runs
  `NSApplication`.
- `daemon::run` splits: single-instance check and socket bind stay, the
  loop becomes `run_loop(...)`; Linux calls it inline, macOS spawns it and
  gives the main thread to the tray; the loop's exit reason exits the
  process.
- Gate: the module compiles against `tray-icon` and `objc2-app-kit` in a
  scratch crate for `x86_64-apple-darwin` (the app crate itself cannot
  cross-check here); Linux behaviour unchanged.

### 4. Service and permissions

- `crates/app/src/service.rs`: `available()`, `installed()`, `install()`,
  `remove()`, `status()` with systemd and launchd bodies; the plist is
  generated from a template with a fixture test; `chronicle service
  install|remove|status` in `main.rs`; the onboarding card calls
  `service::install`.
- AX card: `ax::trusted(false)` from the UI child (same binary, same TCC
  identity); "Open System Settings" runs `open
  x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility`.
- Gate: plist fixture test; `chronicle service status` on this box reports
  the systemd unit; Linux card behaviour unchanged.

### 5. Distribution

- `dist init` → `dist-workspace.toml` with the three targets, shell and
  Homebrew installers, the tap, `macos-sign` behind secrets; the generated
  `release.yml`.
- README platform matrix and install section; the site's install block
  gets the brew line once the first release exists (a tag James pushes).
- Gate: `dist plan` succeeds locally; `dist build` for the Linux target
  produces a tarball with the binary.

## Owed on a Mac

- AX prompt and the trust card; titles after trust.
- The idle counter through sleep/wake; the lock flag on lock, on the
  screensaver, on a closed lid.
- `lsof` timing on a box with many listeners.
- The status item under a bare (unbundled) binary from launchd.
- `brew install`, then `chronicle service install`, then Home in under
  five minutes.
- Signing: an ad-hoc signature loses the AX grant on every rebuild; the
  Developer ID path through `dist` is the fix.

## Shipped (2026-09-10, main 88c61e9 → 212798e)

Written and executed in one sitting on the standing "grab next task and
execute" instruction: five chunk agents built against the interfaces set
here, an Opus review pass over every macOS file produced one BUG-grade
and a dozen LIKELY-BUG findings, all applied in the follow-up commit. No
Mac on this box: verification is the cross-check, the Linux tests (394)
and clippy, and the CI job — the "Owed on a Mac" list above stands.

### Chunks 0–4

- `scripts/mac-check.sh` cross-checks capture, core, server and mcp for
  `x86_64-apple-darwin` with a fake `clang` (empty objects satisfy the C
  build scripts); `cargo clippy --target x86_64-apple-darwin -p
  chronicle-capture --all-targets` is clean with the same env. The app and
  derive crates need llama.cpp's cmake and a real SDK: the `check-macos`
  CI job is their gate, and it has not run yet (main is unpushed).
- Focus polls `NSWorkspace.frontmostApplication` and the AX focused
  window's title once a second inside an autorelease pool with a 250 ms AX
  messaging timeout; the trust prompt fires once at provider start and
  again from the Home card's button (that is what makes TCC list an
  unbundled binary). Idle and presence come from `CGEventSource`
  (counters that go backwards are a session restart and fold as zero);
  the lock flag from `CGSSessionScreenIsLocked` every 2 s.
- Terminal cwd through libproc, stepping through every `login` wrapper
  and taking the newest shell; ports through `lsof -F` with pure parsers
  tested on Linux; mic through CoreAudio's `DeviceIsRunningSomewhere`
  behind the new `MicSource` trait (`PwDump` keeps Linux unchanged).
- Tray: `tray-icon` on the main thread with `NSApplication` (Accessory
  policy, 44 px template icon), the daemon loop on a `daemon-main` thread
  whose exit or panic exits the process; no GUI session (ssh, LaunchDaemon)
  runs the loop on the main thread without a tray.
- `chronicle service install|remove|status` on both platforms (systemd unit
  or the LaunchAgent from `packaging/dev.chronicled.chronicle.plist`,
  `launchctl enable` + `bootstrap` verified by `print`, `disable` on
  remove so the daemon is not killed from its own UI); `service status`
  on this box reports the systemd unit enabled and active.
- Browser history and editor workspaces also look under
  `~/Library/Application Support` (Chrome, Chromium, Brave, Edge, Vivaldi,
  Arc, Firefox; Code family, JetBrains, Zed) — without this the macOS gate
  ("projects populated") would have had no browser or editor evidence.
- Not done, from the review: iTerm2 with session restoration parents
  shells under `iTermServer`, so its tabs get no cwd rows; the Settings
  pane URL is the pre-Ventura anchor (still redirected); `RunAtLoad` on
  install toggles the running daemon's UI once.

### Chunk 5 notes

`dist init --yes --hosting github` (cargo-dist 0.32.0) wrote `dist-workspace.toml`
and `[profile.dist]` in the root `Cargo.toml`, then refused to generate CI —
"This workspace doesn't have anything for dist to Release!" — because
`publish = false` in `[workspace.package]` hides `chronicle` from dist by
default; the fix is `dist = true` in `crates/app/Cargo.toml`'s
`[package.metadata.dist]`, which is now there.

`dist-workspace.toml` was hand-edited to the settings this chunk specifies:
`targets = ["aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-unknown-linux-gnu"]`
(dropped `aarch64-unknown-linux-gnu` and `x86_64-pc-windows-msvc` from
`dist init`'s defaults), `installers = ["shell", "homebrew"]`,
`tap = "james-clarke/homebrew-tap"`, `publish-jobs = ["homebrew"]`,
`ci = "github"`, `install-path = "CARGO_HOME"`, `pr-run-mode = "plan"`,
`macos-sign = true`. All of these keys are present in dist 0.32.0's
`DistMetadata` (confirmed via `strings` on the `dist` binary, since no
source is vendored locally) and `dist generate` accepted `macos-sign`
without complaint, so no manual codesign/notarize note is needed here
beyond what's below.

Cargo metadata dist needs: `crates/app/Cargo.toml` gained `repository`,
`license`, `homepage` (`https://chronicled.dev`, the live site) and
`description`; `Cargo.toml`'s `[workspace.package]` gained `repository` and
`license = "Proprietary"` for the others to inherit. First attempt used
`license-file.workspace = true` pointing at the root `LICENSE` (an
all-rights-reserved file, not an SPDX id) — `cargo metadata` correctly
reports this as `license_file: "../../LICENSE"` (relative to the package's
manifest dir, per Cargo's own rule), but dist 0.32.0's asset-copy step
resolves that relative path against the wrong base directory and fails
with `failed to copy asset from ../../LICENSE to
target/distrib/.../LICENSE: No such file or directory`. Switched to a
plain `license = "Proprietary"` string (a package.metadata.dist "note"
field, not something dist needs to copy) and the build passed; dist's own
LICENSE/README auto-discovery (log line `Found LICENSE at
/home/james/dev/chronicle/LICENSE`, absolute path) still bundles both into
every tarball's `[misc]` assets, confirmed present in the built archive.

`dist generate` wrote `.github/workflows/release.yml` only —
`.github/workflows/ci.yml` untouched (verified with `git diff --stat`).
`cargo tree -e normal -p chronicle | grep -i "gtk\|xkb"` shows only
`xkbcommon-dl` (the `-dl` suffix means dlopen at runtime, no build-time
link) and no `gtk` anywhere in the tree — egui's glow/winit backend needs
no apt packages on the `ubuntu-22.04` build runner, so the generated
workflow was left as-is with no `[dist.dependencies.apt]` addition.

`dist plan` (`$TMPDIR/agent-e-plan.log`) succeeds:

```
announcing v0.1.0
  chronicle 0.1.0
    source.tar.gz [checksum]
    chronicle-installer.sh
    chronicle.rb
    sha256.sum
    chronicle-aarch64-apple-darwin.tar.xz   [bin] chronicle  [misc] LICENSE, README.md
    chronicle-x86_64-apple-darwin.tar.xz    [bin] chronicle  [misc] LICENSE, README.md
    chronicle-x86_64-unknown-linux-gnu.tar.xz [bin] chronicle [misc] LICENSE, README.md
```

`dist build --artifacts=local --target x86_64-unknown-linux-gnu` (after the
license fix) finished the `dist` profile in ~2 min and produced
`target/distrib/chronicle-x86_64-unknown-linux-gnu.tar.xz` (12.7 MB), whose
checksum matches `chronicle-x86_64-unknown-linux-gnu.tar.xz.sha256` and
which contains `chronicle` (48,101,680 bytes, stripped ELF64 PIE),
`LICENSE` and `README.md` — verified with `sha256sum -c` and
`tar -tJvf`.

Secrets the release workflow (`.github/workflows/release.yml`) reads from
`${{ secrets.* }}`, beyond the automatic `GITHUB_TOKEN`:
- `HOMEBREW_TAP_TOKEN` — a PAT with write access to
  `james-clarke/homebrew-tap` (that workflow's own `GITHUB_TOKEN` can't
  push to a different repo); the `publish-homebrew-formula` job checks out
  the tap with it and commits/pushes the generated `chronicle.rb`.
- `CODESIGN_CERTIFICATE`, `CODESIGN_CERTIFICATE_PASSWORD`,
  `CODESIGN_IDENTITY` — the `macos-sign = true` secret set dist documents
  for the `build-local-artifacts` job's macOS runners. No notarization
  step is generated by dist 0.32.0 (no `APPLE_ID`/`APPLE_TEAM_ID`/
  app-specific-password secrets appear in the workflow) — notarization
  stays a manual `xcrun notarytool submit` step to add later if Gatekeeper
  rejects the signed-but-unnotarized binary; not needed until James has a
  Developer ID and wants to test that path.

Before the first release: the `james-clarke/homebrew-tap` repo must exist
(the `publish-homebrew-formula` job's `actions/checkout` targets it
directly and will fail if it's missing), and the above secrets must be set
in this repo's Actions secrets. A release itself is cut by pushing a `v*`
tag (`git tag v0.1.0 && git push origin v0.1.0`, or whatever
`--tag`/version dist expects) — that's James's action, not something this
session did or can do.

Unverified: the `aarch64-apple-darwin` and `x86_64-apple-darwin` builds
(cargo-dist's cross-compiled macOS jobs need a real macOS runner/SDK for
llama.cpp's cmake step, per the plan's own "No Mac is attached to this
box" constraint — only `dist plan`'s manifest was checked for those
targets, not `dist build`); whether `macos-sign` actually wires signing
correctly end-to-end (never run without the three `CODESIGN_*` secrets);
the Homebrew formula's `brew style`/`brew audit` pass (no `brew` on this
box); the shell installer script's actual `curl | sh` behavior once a
release exists.
