# Dev workflow

Working on a machine where Chronicle is also the daily driver. For build,
test and PR expectations see [CONTRIBUTING.md](CONTRIBUTING.md).

The daemon runs under `systemctl --user` from the installed release binary
(`~/.cargo/bin/chronicle`), not `target/debug`. It spawns its own binary for
`ui` / `derive` children, so never replace the exe it is running from.

## Everyday dev

`cargo build` / `test` / `check` / `clippy` are safe anytime — they don't touch
`~/.cargo/bin`.

## Visual UI iteration

`./target/debug/chronicle ui` runs the window standalone against the live DB —
no daemon needed, no systemd stop/start. Daemon-only actions ("derive now")
just report "daemon not reachable" if the unit is stopped.

## Test a debug daemon live

```sh
systemctl --user stop chronicle     # clean SIGTERM path
./target/debug/chronicle run        # test
# Ctrl+C when done
systemctl --user start chronicle
```

Skip the stop and the debug run hits the single-instance gate: it toggles the
release daemon's UI and exits. No crash, but you'd be testing the wrong binary.

## Ship a new version

```sh
systemctl --user stop chronicle
cargo install --path crates/app
systemctl --user start chronicle
chronicle status
```

Stop before install — installing over the running exe breaks its derive/UI
child spawns (`own_exe` path).

## Health & logs

- `chronicle status` (`--json` for scripts; exit 1 when not healthy)
- `journalctl --user -u chronicle -f`
- `~/.local/share/chronicle/logs/chronicle.log` (5 MB rotation, one `.1` backup)
- Crash → auto-restart in 2 s (`Restart=on-failure`)

## The site

`site/` is static HTML + CSS, no scripts, no fonts fetched; keep it that way
(the footer says so). Hosted on Render as a static site from `render.yaml`
(publish path `./site`; CI ignores `site/**`). Every push to `main` rebuilds:
`sh site/build.sh` bakes the git pulse (last push, commits today / this
week, 30-day strip), the footer numbers and the sharing URLs
into `index.html` between `<!-- pulse -->`, `<!-- numbers -->` and
`<!-- og -->` markers, then fails the build if the page breaks its budget
(0 third-party requests, ≤ 2 KB inline JS, ≤ 250 KB above the fold, ≤ 900 KB
total). The committed `index.html` keeps placeholders; preview a baked copy
with `sh site/build.sh $TMPDIR/preview` rather than running it in place.

Render was set up once from the dashboard (New → Blueprint → this repo); it
reads `render.yaml` and owns the custom domain and HTTPS. Every `git push` of
`site/**` to `main` redeploys; nothing else does.

Local check before pushing site changes: render the baked preview headless
at 1280, 820 and 400 wide and look at it (`google-chrome --headless=new
--screenshot=… --window-size=W,H --user-data-dir=<scratch>
file://<preview>/index.html`; Chrome needs write access under
`~/.config`, so run it outside any sandboxed shell; strip `loading="lazy"` into a temp copy first or
offscreen images render as alt text).
