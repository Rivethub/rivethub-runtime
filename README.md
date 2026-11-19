# RivetHub runtime (`rh`)

`rh` is RivetHub's memory runtime. It captures the sessions you run in coding tools, keeps them in a store you own,
and lets every tool recall them.

One binary runs a node: the gateway (the HTTP API clients read memory through), capture, memory (search, summaries,
topics, entities and the wiki), the plug-in host, the MCP server tools use for recall, and the memory workers. A second
binary, `rivethub-migrate`, applies schema changes. Memory lives in one SQLite file on a standalone machine, or in
Postgres on a shared hub.

Documentation is at [rivethub.dev](https://rivethub.dev).

## Install

On Linux or macOS:

```sh
curl -fsSL https://get.rivethub.io/local.sh | bash
```

The installer picks the archive for your machine, verifies it against the release's `SHA256SUMS`, puts `rh` and
`rivethub-migrate` in `~/.local/bin` and starts a local node. Linux builds need glibc 2.35 or newer. On Windows, run
the Linux build under WSL.

Each release also publishes one archive per target (`x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`,
`universal-apple-darwin`) on the [releases page](https://github.com/Rivethub/rivethub-runtime/releases), with `rh`,
`rivethub-migrate` and the license inside.

## Use it

```sh
rh local --ensure                  # start a node on 127.0.0.1:5174 unless one is running
rh harness status                  # which tools are found, and whether capture and recall are wired
rh harness connect claude-code     # add the capture hook and the recall entry for a tool
rh status                          # is the node healthy
rh update                          # install the latest release
```

`rh harness connect` works for Claude Code, Codex, Grok Build, OpenCode, Cursor, DeepSeek Harness, Kimi Code, Oh My Pi
and Hermes. After that, sessions are captured as you work, and each connected tool can search them through the
`memory_search`, `memory_browse` and related MCP tools.

For `memory_search`, use a few specific keywords instead of a full sentence. Include names, identifiers, error text, or the subject, such as `build cache timeout`. If nothing matches, try a second set of keywords.

`rh --help` lists every command.

## Layout

| Path | What it holds |
|---|---|
| `crates/values`, `crates/wire`, `crates/config` | shared types, wire formats and configuration |
| `crates/kernel` | the domain: ports, memory, topics, the wiki, review |
| `crates/memory-sqlite`, `crates/memory-postgres` | the two stores |
| `crates/capture` | reading sessions from each tool and sending them to a node |
| `crates/providers`, `crates/harness-acp` | model providers |
| `crates/mcp` | the MCP server |
| `crates/gateway`, `crates/workers` | the HTTP API and the memory workers |
| `crates/migrate` | schema migrations |
| `crates/cli`, `crates/rivethub` | the commands, and the `rh` binary that composes everything |
| `crates/testkit` | test support and recorded fixtures |
| `xtask` | the checks CI runs |
| `lints` | the project's own lints, a Dylint library outside the workspace |
| `contract` | JSON schemas for shared payloads |

Crates depend only downward: foundation, then kernel, then adapters and services, then composition. `deny.toml`
enforces it.

## Build and check

Rust 1.99, from `rust-toolchain.toml`.

```sh
cargo build --release -p rivethub --bin rh -p cli --bin rivethub-migrate
cargo test --workspace
cargo xtask check          # what CI runs: fmt, clippy, the project lints, cargo-deny and the repository checks
cargo xtask install-hooks  # pre-commit and commit-msg hooks
```

The repository is Rust only and holds code, not commentary: no comments, and no Markdown other than this file. What a
change does and why belongs in its commit message and pull request.

## License

Apache-2.0.
