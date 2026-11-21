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

## Postgres logins on a shared hub

A shared hub keeps three kinds of database login apart:

| Login | Role memberships | Used by |
|---|---|---|
| Owner | owns the schema and every table; `NOLOGIN`, or used only by an administrator | `rivethub-migrate`, `rh db migrate` |
| Serving | `rh_gateway` and `rh_maintenance`; not the owner, `NOSUPERUSER NOBYPASSRLS` | the gateway |
| Worker | its own login with `rh_gateway` and `rh_maintenance`; not the owner, `NOSUPERUSER NOBYPASSRLS` | `rh worker` |

A login pinned to a person in `rh_role_principals` needs neither role and stays pinned even when it holds them. The
database believes the person and spaces a connection names only when its login is a member of `rh_gateway`, and turns
maintenance on only when the login itself is a member of `rh_maintenance`; `SET ROLE` does not change either answer.
The gateway and workers refuse to start under a login that owns the memory schema or tables, bypasses row-level
security, or is neither pinned nor a member of `rh_gateway`. They warn about every unpinned login that can act as a
person or turn on maintenance.

The same checks run before anything is migrated: `rh doctor` fails its memory check, `rh update` stops before
migrating, and `rivethub-migrate` refuses to apply this change while no login that could serve afterwards is a member
of `rh_gateway`. A hub that serves as its owner, including a single-person hub, has to create a serving login first.
Admin commands that open memory directly (`rh memory ...`, `rh db ...`) run as whatever login they are given and do not
apply the serving checks.

Upgrading a hub from schema 80 or 81:

1. As a role administrator, create the groups: `CREATE ROLE rh_gateway NOLOGIN; CREATE ROLE rh_maintenance NOLOGIN;`.
   The migration creates them itself when the migrating login has `CREATEROLE`, and otherwise stops with this hint.
2. Create a serving login for the gateway and a separate login for each worker
   (`CREATE ROLE <name> LOGIN NOSUPERUSER NOBYPASSRLS PASSWORD '...'`). Neither may own the schema or its tables.
3. Grant both groups to the serving and worker logins: `GRANT rh_gateway, rh_maintenance TO <login>;`. Grant nothing
   to any other login.
4. Run `rh db migrate` as the owner. It grants the groups the table and sequence privileges they need, `EXECUTE` on
   the schema's ordinary functions (never on `SECURITY DEFINER` ones), and default table and sequence privileges for
   later migrations.
5. Point the gateway and workers at their own logins and restart them.

Always migrate as the same owner login. Default privileges only follow tables created by the login that set them, so a
table created by a different login is not readable by the serving logins. Once the configured URL is the serving
login, set `RIVETHUB_MIGRATION_URL` to the owner's URL when running `rh update` so the update migrates as the owner;
a serving login can confirm a schema that is already current but cannot apply migrations.

Until step 3 is done, a login that is neither pinned nor in the groups sees no rows once the migration has run.

A hub that serves as the owner login keeps working on the migrated schema with the previous `rh` release, because the
owner still passes every check. Migrate and switch the logins first, then install this release: it refuses to serve
as the owner and names the login it refused.

What this does not cover: any member of `rh_gateway` can still read and write every person's rows by naming that
person, so the serving and worker logins must be treated as trusted with the whole hub. The serving login also holds
`rh_maintenance` because the gateway runs upkeep in process. The planned next step is a gateway-only serving login with
that upkeep moved behind `SECURITY DEFINER` functions; the full answer is a login per person or a signed assertion
of who is asking, so the database does not have to trust the gateway's word.

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
