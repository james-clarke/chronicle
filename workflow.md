# Dev workflow (post-m11: daemon is systemd-managed)

The daemon runs under `systemctl --user` from the installed release binary
(`~/.cargo/bin/chronicle`), not `target/debug`. It spawns its own binary for
`ui` / `derive` children, so never replace the exe it is running from.

## Everyday dev

`cargo build` / `test` / `check` / `clippy` are safe anytime — they don't touch
`~/.cargo/bin`.

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
