# M40 — Windows: the last port, on the platform half of developers use

Status: written 2026-09-11 on James's queue ("M40 Windows, the last port.
No plan doc yet"); rationale in `dev-tools-direction.md` ("Platforms and
distribution", the M40 line). Runs after M41, whose registry and probes are
what make a Windows row a data edit rather than another panel. Written on a
Linux box with no Windows machine, the way M38 was written without a Mac.
Every "today" claim carries a `file:line`.

## The short version

Chronicle captures on Linux (X11, wlroots, KWin) and macOS. Windows is
~50 % of professional developers (WSL ~17 % of all respondents) and the
direction doc puts it last only because its two honest limits — elevated
windows are invisible to an unelevated process, and a WSL2 shell's title
carries no working directory — needed the shell hook (M37) to exist first.
This milestone gives every collector with a Linux or macOS body a Windows
arm behind the same traits, moves the single-instance socket and the
service manager off their Unix assumptions, adds the tray on the daemon's
own message pump, and ships a signed-or-disclosed `x86_64-pc-windows-msvc`
build through cargo-dist. The README's platform matrix already names the
route per row (README.md:56–79); this plan is that column made real.

## What the code does today

- **Traits are platform-neutral, arms are not.** `FocusProvider`,
  `LockSignal`, `PresenceProvider`, `AfkProvider` (capture `lib.rs:45–65`)
  and `MicSource` (`mic.rs:25`) are chosen in `crates/app/src/capture.rs`
  by `cfg(target_os = "linux")` (`:9`, `:20`, `:62`) and `"macos"` (`:112`);
  the only non-Linux fallback is `focus_route_name` returning `None`
  (`:70`). No file in the tree mentions `target_os = "windows"`.
- **The macOS port is the template.** `crates/capture/src/macos/` holds
  `focus`, `input`, `lock`, `ax`, `proc`, `lsof`, `mic`, `ffi` behind
  per-target dependencies (`capture/Cargo.toml:30`); the app crate does the
  same for `tray-icon` (`app/Cargo.toml:41`) and moves the daemon loop to a
  `daemon-main` thread so the main thread can host the tray
  (`daemon.rs:375–408`).
- **Unix in the plumbing.** The single-instance socket is a
  `std::os::unix::net::UnixListener` at `$XDG_RUNTIME_DIR` or the data dir
  (`daemon.rs:41`, `:95`); the control protocol over it is text lines
  (`daemon.rs:84–92`). `chronicle service` has systemd and launchd arms and
  a third that answers "not available" (`service.rs:220–248`). Git hooks are
  written as `#!/bin/sh` scripts (`hooks.rs:81`). Listening ports walk
  `/proc/net/tcp` (`ports.rs:62`); the terminal cwd reads `/proc/<pid>/cwd`
  or libproc (`cwd.rs`); presence counts XI2 raw events (`presence.rs`);
  the lock edge comes from logind (`lock.rs`).
- **Two unconditional X11 dependencies.** `x11rb` is a plain dependency of
  the app crate (`app/Cargo.toml:36`) for `compositor_active()`
  (`ui/mod.rs:59`), and `WINIT_X11_SCALE_FACTOR` is pinned before eframe
  starts (`ui/mod.rs:80–84`). Both compile on Windows and are wrong there.
- **The registry already knows Windows.** `Platform::Windows` is in `ALL`
  (`connectors.rs:28`); Chrome's history probe has a `{data}/Google/Chrome/
  User Data` arm (`:338`) and `health::expand` maps `{config}`/`{data}` to
  `AppData/Roaming` and `AppData/Local` (`health.rs:70`). Listening ports
  carry a `netstat` presence probe (`:522`) and the note "Windows
  GetExtendedTcpTable" (`:516`); mic capture's note says the Windows
  capability registry is not written (`:534`). The browser reader itself
  only looks under `~/.config` and `~/Library/Application Support`
  (`browser.rs:36`, `:328`), so the probe would say `found` for a database
  the collector never opens — the M41 rule ("probes mirror what the
  collector reads") makes that a bug to fix in chunk 2, not a feature.
- **The shell hook speaks PowerShell already** (`shell_hook.rs:250`,
  `chronicle shell-init pwsh`), posting cwd, program and duration to the
  local endpoint; the test at `:348` pins that the raw command never goes.
- **Distribution stops at three targets** (`dist-workspace.toml:15`); the
  cargo-dist workflow already carries its "enable windows longpaths" step
  for a fourth (`release.yml:116`). No `windows`, `windows-sys` or `winreg`
  crate is in the workspace. The site says "Not yet. The capture layer is
  being written now" (`site/index.html:339`).
- **Data dir** is `directories::ProjectDirs` (`core/lib.rs:36`,
  `Cargo.toml:58`): `%APPDATA%\chronicle\data` on Windows, honouring the
  same `XDG_DATA_HOME` override the sandboxes use.

## Decisions

- **One capture thread with a message pump.** `SetWinEventHook`
  (`EVENT_SYSTEM_FOREGROUND`, `EVENT_OBJECT_NAMECHANGE`,
  `WINEVENT_OUTOFCONTEXT`) needs a thread that pumps `GetMessage`; the
  same thread owns a message-only window for `WM_WTSSESSION_CHANGE`
  (`WTSRegisterSessionNotification`) and the low-level hooks. The four
  providers become channel readers off that one thread, so the app crate's
  `spawn_capture` keeps the Linux shape: four `Box<dyn …>` values, no
  shared state. Titles debounce 1 s as on X11 (`x11.rs:32`).
- **App name is the process image, not the class.** `GetWindowThreadProcessId`
  then `QueryFullProcessImageNameW`, basename without `.exe`, lowercased —
  `code`, `windowsterminal`, `chrome` — which is what `extract::family`
  (`extract.rs:141`) already matches. The window class (`Chrome_WidgetWin_1`)
  goes nowhere.
- **Idle is `GetLastInputInfo`; presence is counts from `WH_KEYBOARD_LL` /
  `WH_MOUSE_LL`** on the pump thread — key counts, button counts, motion and
  wheel, never which key (the `presence` table's contract, README.md:64 and the m32 note).
  Hooks are installed only when `capture_presence` is on, the same switch
  as X11.
- **Lock is a WTS edge, suspend is the tick.** `WTS_SESSION_LOCK`/`_UNLOCK`
  drive `LockSignal`; the daemon's late-tick rule already marks suspends
  (`daemon.rs:487`), so `WM_POWERBROADCAST` is not wired.
- **Terminal cwd is the shell hook's job on Windows.** Reading another
  process's cwd needs `NtQueryInformationProcess` and a PEB walk that
  breaks across architectures and fails on elevated shells; the pwsh hook
  already reports cwd per command, and WSL shells report through the bash
  or zsh hook. `cwd.rs` gets no Windows body; the Setup view's "one step
  each" already puts the hook line first.
- **Ports through `GetExtendedTcpTable`** (`iphlpapi`, `TCP_TABLE_OWNER_PID_LISTENER`)
  for `(port, pid)`, then the image path per pid. Without a cwd the row is
  keyed by process rather than repo; the place resolver gets the image
  basename, which is enough for `localhost:5173 → node` and no more. The
  `netstat` probe (`connectors.rs:522`) is replaced by nothing — the API
  needs no binary — and the row's health comes from `DbRows`.
- **Mic through the capability registry**:
  `HKCU\Software\Microsoft\Windows\CurrentVersion\CapabilityAccessManager\
  ConsentStore\microphone\**\LastUsedTimeStop == 0` names the packages
  and (under `NonPackaged`) the executables using the microphone right
  now — the one place Windows writes this without a driver. A `MicSource`
  polling it every 2 s; the app is the executable's basename.
- **Tray through `tray-icon`, on the main thread**, mirroring macOS: the
  loop moves to `daemon-main` (`daemon.rs:398`) and the main thread pumps
  messages for the icon; menu items map to the same `CtrlMsg`s. No GUI
  session (a service, an RDP-less session 0) skips the tray and runs the
  loop on the main thread, as `gui_session()` does on macOS.
- **Single instance through a named pipe**, `\\.\pipe\chronicle-<hash of
  the data dir>`, behind the existing text-line protocol; `socket_path`
  and `send_ctrl` grow a Windows arm and the Unix one is untouched. The UI
  stays a child process spawned with a stdin pipe exactly as today.
- **Autostart is the `Run` key**, `HKCU\Software\Microsoft\Windows\
  CurrentVersion\Run\chronicle = "<exe>" run`, as the platform matrix promises (README.md:67). It
  has no restart contract; that is disclosed in `service status` ("starts
  at login; not supervised") rather than papered over with a Task Scheduler
  entry that would need elevation for the restart trigger. `chronicle
  service install|remove|status` gains its third arm; the onboarding card
  calls the same code.
- **Git hooks stay POSIX scripts.** Git for Windows runs hooks through its
  own `sh`, so `#!/bin/sh` files work unchanged; `hooks::install` only has
  to stop setting the 0755 mode bit there. `.cmd` shims are not written.
- **UIPI is disclosed, not worked around.** An unelevated daemon cannot read
  titles from elevated windows; those spans carry the app name and an empty
  title, logged once per process, and the README platform note says so.
  Running the daemon elevated is not offered.
- **WSL2 is the shell hook plus one address.** Inside WSL, `127.0.0.1` is
  the VM unless Windows 11's mirrored networking is on; `chronicle
  shell-init bash|zsh` learns to detect WSL (`/proc/version`) and post to
  the default gateway when `localhost` does not answer. The hook line in
  Setup shows the same text on every platform.
- **`x86_64-pc-windows-msvc` only**, CPU inference; Vulkan stays an
  opt-in feature as on Linux. cargo-dist's PowerShell installer
  (`irm … | iex`) and a `.zip` are the download; winget and Scoop
  manifests are owed after the first release, since both need a released
  URL. Trusted Signing is documented as the fix for the SmartScreen warning
  and not bought here.

## Chunks and gates

### 0. Scaffolding

- `crates/capture/src/windows/` behind `cfg(target_os = "windows")` with
  the names the app crate will call: `focus::WinFocusProvider`,
  `input::{WinAfkProvider, WinPresenceProvider}`, `lock::WinLock`,
  `ports`, `mic::CapabilityMic`, `pump` (the thread that owns the hooks
  and the message-only window), `ffi` where `windows-sys` lacks a binding.
- Per-target dependencies: `windows-sys` (Win32_UI_WindowsAndMessaging,
  Win32_UI_Accessibility, Win32_UI_Input_KeyboardAndMouse,
  Win32_System_RemoteDesktop, Win32_NetworkManagement_IpHelper,
  Win32_System_Registry, Win32_System_Threading) for capture; `tray-icon`
  for the app on Windows as on macOS.
- `x11rb` becomes a Linux-only dependency of the app crate
  (`app/Cargo.toml:36`); `compositor_active()` returns `true` off Linux
  (DWM composites always) and the `WINIT_X11_SCALE_FACTOR` pin gets a
  `cfg(target_os = "linux")`.
- `spawn_capture` gains a Windows body mirroring the Linux one; lock,
  presence and mic spawns get Windows arms; `xdg-open`/`open` becomes
  `cmd /c start`.
- `scripts/win-check.sh`: `rustup target add x86_64-pc-windows-msvc`, then
  `cargo check --target x86_64-pc-windows-msvc` on capture, core, server and
  mcp with the fake-compiler trick from `scripts/mac-check.sh` so
  `libsqlite3-sys`, `ring` and `aws-lc-sys` build scripts pass. The app and
  derive crates need llama.cpp's cmake and the MSVC toolchain: the
  `windows-latest` CI job is their gate.
- CI: a `windows-latest` job running clippy and the tests.
- Gate: Linux and macOS builds unchanged (tests, clippy, `mac-check.sh`);
  `win-check.sh` passes once chunk 1 lands.

### 1. Focus, idle, presence, lock

- `pump`: one thread, `SetWinEventHook` for foreground and name changes,
  a message-only `HWND` registered with `WTSRegisterSessionNotification`,
  the two low-level hooks when presence is on, `GetMessage` loop; every
  event becomes a message on a crossbeam channel. Foreground and title
  changes resolve pid and image name on the pump thread and emit
  `(app, title, pid)`; title changes debounce 1 s.
- `WinFocusProvider` reads that channel; `WinAfkProvider` polls
  `GetLastInputInfo` every second (the `AfkProvider` contract, lib.rs:65);
  `WinPresenceProvider` drains the hook counters per minute; `WinLock`
  turns `WTS_SESSION_LOCK`/`UNLOCK` into edges.
- Elevated windows: `GetWindowTextW` returns empty and
  `QueryFullProcessImageNameW` fails with access denied; the provider
  emits the app from the window's process id where it can and an empty
  title, and logs "elevated window, title unreadable" once per pid.
- Gate: `win-check.sh` clean; the pure parts (image basename → app,
  debounce, counter folding) unit-tested on Linux; on a Windows box,
  alt-tabbing between two apps produces two focus spans with titles, and
  Win+L flips the lock flag in `chronicle status`.

### 2. Ports, mic, browser paths, ports probe

- `ports`: `GetExtendedTcpTable` listeners with owning pid, image path per
  pid, one call a minute, emitting the same `cwd`-style event the Linux
  poller does with the process basename as the place. The registry row
  loses its `netstat` probe.
- `CapabilityMic` behind `MicSource` (mic.rs:25): the consent-store walk
  every 2 s; the Linux `PwDump` and macOS CoreAudio sources untouched.
- The browser reader and editor-workspace reader gain the Windows
  directories (`%LOCALAPPDATA%\Google\Chrome\User Data`, `…\Microsoft\Edge`,
  `…\BraveSoftware`, `%APPDATA%\Mozilla\Firefox\Profiles`, `%APPDATA%\Code`,
  JetBrains under `%APPDATA%\JetBrains`) so the M41 probes describe what is
  read; the M38 lesson ("without this the gate would have had no browser or
  editor evidence") applies verbatim.
- Gate: the parsers pure and tested on Linux; on a Windows box a `vite`
  dev server shows as a listening-port row, a Teams call as a `call` span,
  and Chrome history lands as `browse` rows.

### 3. Tray, single instance, UI

- Named pipe server in `daemon.rs` behind the text protocol; `send_ctrl`
  connects to it; `chronicle toggle` and a second `chronicle` work as on
  Linux.
- `tray_windows.rs` with `tray-icon` on the main thread; `run` gets a
  Windows arm shaped like the macOS one (`daemon.rs:375–408`), including
  the panic hook that exits the process.
- The UI child: `compositor_active` true, no scale pin, the window parks
  bottom-right of the primary monitor's work area (winit reports it), and
  `chronicle ui` opens on Setup on a fresh profile as everywhere.
- Gate: on a Windows box, `chronicle run` twice shows the window once;
  the tray click toggles it; closing the daemon from the tray exits both
  processes.

### 4. Service and autostart

- `service.rs` Windows arm: `install` writes the `Run` value with the
  running binary's path, `remove` deletes it, `status` reports
  `starts at login` / `not installed`, plus the disclosure that nothing
  supervises it. The onboarding "run at login" card calls it.
- Logs under `%APPDATA%\chronicle\data\logs\chronicle.log` through the
  existing rotation; no Event Log integration.
- Gate: `chronicle service install`, sign out and in, `chronicle status`
  reports a daemon with an uptime under a minute.

### 5. Distribution and disclosure

- `dist-workspace.toml` gains `x86_64-pc-windows-msvc`; `dist plan` shows
  the PowerShell installer and the `.zip`; the release workflow's Windows
  job builds llama.cpp with MSVC (CPU only).
- README platform matrix and notes: UIPI and WSL2 stated as limits with
  their answers; the site's Windows download row replaces "Not yet" with
  the `irm` line and a SmartScreen sentence.
- `scripts/requests.sh` and the registry gain nothing; `docs/connectors.json`
  and `site/tools.html` regenerate because two descriptors changed.
- Gate: `dist plan` resolves four targets; the tools page shows the
  Windows column truthfully (ports and mic `supported`, cwd via the hook).

## Owed on a Windows machine

- Every gate above marked "on a Windows box": focus spans with titles, the
  lock flag, the ports row, a call span, the tray toggle, the login start.
- WSL2: the bash hook posting to the host on both networking modes.
- An elevated window (an admin PowerShell) producing an app-only span and
  one log line, not a crash.
- The installer under SmartScreen, and whether the unsigned warning is
  survivable for the first users or Trusted Signing has to come first.
- Presence hooks under a game or a remote-desktop session: the hook
  callback budget (300 ms before Windows drops the hook) must never be
  exceeded; the counters are atomics for that reason.
- winget and Scoop manifests, after the first tagged release.
