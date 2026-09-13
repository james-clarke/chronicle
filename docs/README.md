# Reference

The [README](../README.md) is the overview. These pages go deeper.

- [Configuration](configuration.md): every field of `config.toml` with its default, the other files in the data directory, and the environment variables.
- [CLI](cli.md): every subcommand and flag.
- [Data and privacy](data.md): what each table holds, what leaves the machine and when, retention, export and deletion.
- [Sources](sources.md): every evidence source, what it reads and does not read, and how to connect it.
- [Architecture](architecture.md): the process model, the crates, the capture traits, and the rules the pipeline runs on.

`connectors.json` is generated from the connector registry and checked by a test. Do not edit it by hand; the regeneration command is in [CONTRIBUTING.md](../CONTRIBUTING.md).
