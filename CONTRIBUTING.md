# Contributing

Chronicle is a Rust workspace. This page covers building it, running it
against your own machine, finding your way around, and what a pull request
needs to pass.

## Build

You need stable Rust (`rust-toolchain.toml` pins the channel and pulls in
`rustfmt` and `clippy`), plus `cmake` and a C++ compiler for the embedded
llama.cpp.

```sh
git clone https://github.com/james-clarke/chronicle
cd chronicle
cargo build
cargo test --workspace
```

The build produces one binary, `target/debug/chronicle`. GPU acceleration is
opt-in with `--features vulkan` on Linux; on macOS Metal is on by default.

## First run

The daemon runs as `chronicle run` and spawns its own binary for the window
and the model workers, so never install over a binary that is currently
running.

```sh
./target/debug/chronicle run             # in one terminal, leave it running
./target/debug/chronicle ui              # the window, in another
./target/debug/chronicle dump            # what has been stored so far
```

Capture works without a model. To see derivation, `./target/debug/chronicle
model pull` downloads the default Qwen3-4B (about 2.5 GB) into the data
directory, and tasks appear after about five minutes idle. If a release
build is already installed as a service on the machine, stop it first, or the
debug daemon will find the running instance, toggle its window and exit.
[`workflow.md`](workflow.md) covers that setup in more detail.

## Where things live

| Crate | What it owns |
|---|---|
| `crates/core` | Types, config, SQLite storage and migrations, sessionizer, pre-pass rules, digest, evidence, corrections, the connector registry |
| `crates/capture` | The `FocusProvider`, `AfkProvider`, `PresenceProvider` and `LockSignal` traits, one implementation per platform behind `#[cfg]`, and the evidence collectors (git, sessions, browser, shell, calendars, microphone) |
| `crates/server` | The localhost HTTP endpoint that speaks ActivityWatch and WakaTime |
| `crates/derive` | Digest to prompt, the llama.cpp runner, GBNF grammars, embeddings, cloud backends |
| `crates/mcp` | MCP client config and the allowlisted context and fetch calls |
| `crates/app` | The `chronicle` binary: CLI, the daemon loop, the egui UI, the worker subcommands |

Grammars are in `grammars/`, prompts in `prompts/`, recorded event streams
and expected outputs in `fixtures/`. Every name in the fixtures is invented;
[`fixtures/README.md`](fixtures/README.md) describes the cast so a new
fixture can extend the same story. [`docs/`](docs/README.md) holds the
reference pages: configuration, the CLI, what is stored, the sources and
the architecture.

Platform code stays behind the capture traits. Everything downstream only
sees `CaptureEvent`, so adding a platform means adding a `#[cfg]` module and
nothing else changes.

## The gate

Every change has to pass these three commands:

```sh
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same three on Linux, and clippy plus tests again on macOS,
which is the only compile check the macOS port gets. CI skips changes that
only touch Markdown, `docs/`, `site/` or `render.yaml`.

## Tests

Tests are fixture-driven. A mock `FocusProvider` replays a JSONL event
stream from `fixtures/`, the sessionizer and digest are compared against
golden files next to it, and derivation is scored against `*.expect.json` by
`chronicle bench`. For a change to derivation quality, add the failing case
as a fixture first, then make it pass.

Two committed files are generated from the connector registry and checked by
a test, so regenerate them after any registry change:

```sh
./target/debug/chronicle connections --json > docs/connectors.json
sh scripts/site-tools.sh       # rewrites the table in site/tools.html
```

## Pull requests

- One change per pull request, and a diff that matches the title.
- Explain why you made the change as well as what changed.
- For larger work, open an issue first describing the change and why, so
  the approach is agreed before the code is written.
- Commit messages follow Conventional Commits, `type(scope): subject`, one
  line, in the style of the existing `git log`.
- Use whatever tools you like to write the code, AI included. Patches are
  reviewed on what they do and how they read. If the code is good it gets
  merged.

## Bugs and requests

Open an issue. For a tool Chronicle cannot read yet, use the
[integration request template](https://github.com/james-clarke/chronicle/issues/new?template=integration-request.yml).
The app can fill it in for you from Settings › Connections › *What you work
with*.

## License

Chronicle is AGPL-3.0-only. Contributions are accepted under the same
licence. There is no contributor agreement.
