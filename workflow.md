# Dev workflow (post-m11: daemon is systemd-managed)

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
(publish path `./site`, `buildFilter` so only `site/**` changes redeploy;
CI ignores `site/**` too).

First-time setup, once a GitHub remote exists (repo is proprietary, keep it
private):

```sh
gh auth login -h github.com
gh repo create chronicle --private --source=. --remote=origin --push
```

Then Render dashboard → New → Blueprint → pick the repo; it reads
`render.yaml` and creates `chronicle-site`. Custom domain and HTTPS are set on
the service afterwards. Every later `git push` of `site/**` to `main`
redeploys; nothing else does.

Local check before pushing site changes: render the page headless at 1280,
820 and 400 wide and look at it (`google-chrome --headless=new
--screenshot=… --window-size=W,H file://…/site/index.html`; strip
`loading="lazy"` into a temp copy first or offscreen images render as alt
text).
