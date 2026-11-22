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

Historical summary conversion waits five minutes before a second pass after exhausting its transient model retries.
This gives model contention time to clear on a short queue. Set
`RIVETHUB_SUMMARY_CONVERSION_SECOND_PASS_DELAY_MINUTES` on the worker to choose a delay from 1 to 1440 minutes.

## Run rh inside another application

A host application that starts its own node and shows the web app reads the node's state from these commands. Each
prints one JSON document on stdout. The field names are stable: a release may add fields and does not rename or remove
one.

| Command | Prints | Exit |
|---|---|---|
| `rh local --ensure --json` | `outcome` (`started`, `already_running` or `failed`), `origin`, `port`, `pid`, `node`, `version` | 0, or 1 with `reason` `port_in_use`, `not_ready` or `error` |
| `rh status --json` | `running`, `version`, `cli_version`, `origin`, `health_error`, `web`, `tls` | 0 when the document was printed, running or not; 1 when the configuration cannot be read |
| `rh open --json` | `outcome` `minted` with `origin`, `url`, `code`, `expires_at`; or `outcome` `failed` with `reason` and `error` | 0; 12 when the node runs and has no web app (`reason` `no_web_app`); 1 otherwise (`not_running` or `error`) |
| `rh tls fingerprint --json` | `cert`, `ca`, `sha256`, `spki_sha256`, `not_after` | 0; 1 when the node serves plain HTTP |
| `rh auth token-path --json` | `path` of the local owner token | 0 |

`rh open` without `--json` uses the same exit codes.

**Home and configuration.** Set `RIVETHUB_HOME` to an absolute directory to give the node a home of its own. The
state directory is `RIVETHUB_STATE_DIR` when set, then `RIVETHUB_HOME`, then `~/.rivethub`. It holds the database, the
owner token, the logs and the web app. The configuration file is the first of these that exists: the path in
`RIVETHUB_CONFIG`, then `config.yaml` or `config.yml` in the home, then `config.yaml` in the working directory.
`rh local` writes a standalone `config.yaml` into the home when there is none. Every command reads the same home, so
run them all with the same `RIVETHUB_HOME`.

`health_error` is `null` when the node answered its health check, and otherwise says why it did not.

**Origin.** The origin is the scheme, `gateway.host` and `gateway.port` of the configuration, and `rh status --json`
prints it whether or not the node runs. `rh local --port <n>` sets the port. The node never moves to another port:
when the port is taken, `rh local --ensure` fails with `port_in_use`. The origin changes only when the configuration
or `--port` changes, so compare `origin` with the one you stored before you reuse a browser session.

**Web app.** The node serves the directory in `RIVETHUB_STATIC_DIR` or `gateway.static_dir` when either is set, and
the environment variable wins. Otherwise it serves `web/current` in the state directory, a link to `web/<version>`,
which holds `index.html` and the `web-manifest.json` of that build. Only `rh update` writes that directory.
`rh update --web-only` finds a build on GitHub. `rh update --web-only --from <archive> --sha256 <hex>` installs an
archive you already hold and makes no network request. It reads `web-manifest.json` from beside the archive, or from
`--manifest`, and refuses an archive whose SHA-256 differs from `--sha256` or a build that needs a newer `rh`. In
`rh status --json`, `web.source` is `managed`, `explicit` or `none`, and `web.compatible` says whether this `rh` is at
least the build's `minRhVersion`. It is `null` for an explicit directory.

**Signing in.** `url` from `rh open --json` signs a browser in once, within `expires_at`. To land on a page other
than the start page, build the URL as `<origin>/login?next=<path>#code=<code>` with the path percent-encoded. The login
page follows `next` only when it is a path on the same origin.

**TLS.** A node bound to loopback serves plain HTTP unless `gateway.tls_cert` and `gateway.tls_key` (or
`RIVETHUB_TLS_CERT` and `RIVETHUB_TLS_KEY`) name a certificate, and `tls` in `rh status --json` is then `null`. With a
certificate, `sha256` is the SHA-256 of the leaf certificate and `spki_sha256` is the SHA-256 of its public key. The
node reads the certificate file when it starts. A replaced file changes what these commands print at once and what
the node serves after its next restart. Read the fingerprint again after each restart of the node and after
`not_after`. A renewal that keeps the key changes `sha256` and keeps `spki_sha256`.

## SQLite and Postgres memory transfer

```sh
rh memory merge-store --from-sqlite memory.sqlite --into <hub-url> --owner alice --sqlite-owner owner
rh memory merge-store --from <hub-url> --into-sqlite memory.sqlite --owner alice --sqlite-owner owner
```

Use the local principal configured for the SQLite runtime. It defaults to the hub principal on the command line. Both directions support `--dry-run` and `--json`. The transfer takes a snapshot of the source, copies rows in a destination transaction, and leaves the source in place. When copying into a SQLite path that does not exist, the command initializes it. An unsuccessful copy can leave that initialized database on disk.

Use an administrative Postgres connection and migrate both databases to the current schema first.
The hub must already have matching spaces and memberships for the selected owner. Global type
references resolve by slug against the destination's catalog. If any required global type is
missing, the command lists its slug before inserting any destination rows. Provision those
types on the destination and retry. Conflicting rows abort the destination transaction.

The copy includes claims on the person's own topics and on global types. It reports how
many of their claims refer to topics another person owns. A hub wiki redirect travels
when its target page travels and its owner is the selected hub person or is unknown.
The SQLite copy assigns the selected SQLite principal to legacy redirects with unknown
owners. The report counts redirects to copied pages left behind because someone else owns them.
The copy preserves each tag's stored display and includes shared taxonomy entries for its
key and value. Without a taxonomy entry, the tag still has its own display text or the raw
key and value. Unused shared vocabulary and other people's private vocabulary stay behind.

The copy assigns ownerless personal rows in a standalone SQLite file to the chosen
principal. Global type and shared taxonomy rows retain their shared status. Legacy
`work`, `entity`, and `type` topic tombstones without evidence of ownership stay behind.
The report counts those unattributed tombstones.

SQLite stores entity vectors as f32 blobs. Postgres stores them at half precision and
retains the original SQLite vector when the conversion rounds it. Summary passages travel
with their owning summaries. Pass the destination worker's reported index key as
`--destination-index-key <hex>` to reuse matching passage vectors. With a different or
unknown key, the destination worker rebuilds the passage index. The destination also
rebuilds the other search embeddings and indexes. The command reports excluded data,
including Git-backed wiki files and history, which require a separate repository backup.

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
