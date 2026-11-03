# Service composition

`Config::load` with `Profile::Node`, and `rh config validate`, still require the `runtime`, `agents`, and `providers` sections. `rh local` does not invent an agent, a provider, or an endpoint. When no config file resolves it writes only the SQLite database path and the gateway bind, then loads that file with `Profile::Standalone`, which does not require those sections. A present section is still validated. `rh local` composes the task engine, gateway, configured workers, and MCP sidecar in one process over SQLite. With an agent and provider configured, MCP `delegate_task` runs through the gateway session API; `memory.sqlite.wiki_dir` enables the wiki builder. Standalone and service roots resolve the node identity through the same resolver.

`rh gateway`, `rh worker`, and `rh mcp` are the composition roots. Each one
runs on the multithreaded Tokio runtime built in `rh`'s `main`, with one
cancellation tree, and stops on SIGTERM or SIGINT. In-flight provider calls,
embedding calls, queue listens, gateway connections, and claimed jobs observe
that token. A claimed job is released back to the queue with the attempt added
by the claim removed; it is not failed. Each root exits 0 when that work finishes
inside a 10 second grace (`kernel::shutdown::SHUTDOWN_GRACE`). If anything is
still entered when the grace elapses, the process logs
`shutdown grace of 10s elapsed; still running: …` and exits non-zero.
`rh start` supervises those roots as children of this binary and stays hidden
from help. Invoking `rh` without arguments prints help.

`rh gateway --check`, `rh worker --role <role> --check`, and `rh mcp --check`
validate configuration without connecting the memory database, binding
listeners, or moving legacy state. Configuration uses `~/.rivethub/config.yaml`
and the config crate's `RIVETHUB_*` environment resolution and deprecated aliases.

The gateway composition supplies Postgres memory, the shared session registry,
local owner identity, verified device names, the Claude transcript reader,
configured providers and agent loops, the agent presets kept on the memory
database, the hub state, the plug-in host, the task engine, the device runtime
with the node's certificate authority, and optional terminal and mesh services. HTTP routes use the gateway router, including capture,
memory/tags, sessions, terminal/harness sessions and wiki. Every request to
the composed router requires a resolved identity. A verified device leaf is
looked up before loopback ownership, so an unknown device cannot inherit the
owner identity merely by connecting over loopback.

Under the `gateway` configuration section (`den` is its deprecated alias):

- `tls_cert` and `tls_key` select the server identity.
- `tls_ca` selects the client CA chain; when present, client certificates are
  required and verified by rustls.
- `tls_crl` selects a PEM CRL bundle. Revocation and CRL expiration fail closed.
- `users_file` (overridden by `RIVETHUB_USERS_FILE`, with the deprecated
  `RIVETOS_USERS_FILE` alias) selects the B11 JSON registry: `ownerUserId`, `unmappedIsOwner`,
  and `users`, a map of user IDs to records with optional `devices`, `pgUrl`,
  `envFile` and `persona`. Device names accept the compatibility `device:` prefix.
  SPIFFE device identities must match both the registry user and device. Legacy
  device leaves use the registry mapping and emit a deprecation warning. An
  explicit missing or invalid file refuses startup. Without a registry, the local
  identity is `owner` and no device identities are accepted.
- `terminal.enabled: true` enables the herdr-backed terminal service. The roster path is
  `commands_file` when set, otherwise `RIVETHUB_TERM_CONFIG`, otherwise the
  deprecated `RIVETOS_DEN_TERM_CONFIG` alias (one warning while the new name is
  unset), otherwise `<state dir>/den-term.json`. The file maps command ids to
  `label`, `cmd`, and `room`, with optional top-level and per-command `cwd` and
  `env`. `room` is the session flag. A missing file starts the built-in roster.
  Malformed JSON, an empty argv, a relative cwd, an invalid key, or a default
  that names no command refuses startup. Direct PTY fallback is not selected by
  the composition. Missing herdr refuses terminal startup.
- `terminal.roots` lists the folders a session may start in; a session working
  directory must resolve inside one of them, otherwise the start is refused with
  403. Every entry must be an absolute path. Without the key the roots are
  `files_root` (else `runtime.workspace`), the service user's home directory,
  and the shared directory (`/rivet-shared`, or `RIVETHUB_SHARED_DIR`). Roots that
  do not exist are skipped with a warning, and terminal startup is refused when
  none exists. A relative directory from a delegated turn is joined to the first
  root. These roots do not change what the Files tab browses, which stays
  `files_root`.

Mesh requires `enabled`, `node_name`, `advertise_host`, `agent_channel_port`,
and TLS with a client CA. It writes `mesh.json` beneath the configured shared
root. Every configured agent receives a bounded mailbox and independent turns.
All agents use the common audit pipeline.
Registry heartbeats, listeners, turns and terminal maintenance belong to the
shutdown tree. SIGTERM/SIGINT cancel these services under the same 10 second grace.

Before moving the compatibility state directory, startup acquires existing
root-level `.pid`/`.lock` files and checks their PID owners. A live PID, held lock,
unreadable file or malformed PID refuses the move. Successful stale-file checks
retain the locks through rename. `--check` never performs the move.

## Current checkout limits

The SQLite store implements the kernel memory ports, and `rh local` composes it. The
gateway and worker roots open Postgres only; `rh mcp` opens the SQLite file of a
SQLite config and serves stdio. An explicit SQLite or PGlite
selection fails with a typed startup error and is never redirected to Postgres.

`rh gateway` connects the step-ca client (`pki::Authority`) when
`gateway.certificate_authority` is set and runs without a certificate authority
otherwise. A phone issuer is enabled only when `gateway.devices.gateway_url` is set,
and pairs only while the authority answers. Device enrollment
(`/api/devices/enroll`) stays off. The users file remains a composition
configuration boundary.

`rh worker --role` runs the workers runner for embedder, summarizer,
categorizer, wiki-builder, and `all`. The runner uses the Postgres job queue,
embedding store, summary store, and wiki store, plus the default agent's
provider. An absent queue (no `graphile_worker` schema) fails closed. A role that
claims `extract-wiki` refuses to start without `RIVETHUB_WIKI_EXTRACTION=1`.

`rh start` is the supervisor. It spawns `rh gateway` unless `gateway.enabled` is
false, `rh worker --role` for
sqlite `workers`, `tagging`, and `wiki_dir`, and `rh mcp` when `transports.mcp`
sets `host` or `socket`. Postgres has no worker-role list, so those roles are
not inferred. Stdio MCP is not a child. On SIGTERM or SIGINT, `rh start` forwards
SIGTERM to each child and waits the same 10 second grace. A child that exits
inside the grace is not killed. A child still running after the grace is sent
SIGKILL; if it has not exited after that same grace, start logs
`shutdown grace of 10s elapsed; still running: child …` and returns an error. `rh local` is one process on one
`SqliteStore`: the gateway (always, including when `gateway.enabled` is false), the
embedder and summarizer when `memory.sqlite.workers` is true, the categorizer
when `memory.sqlite.tagging` is true, the wiki builder when `memory.sqlite.wiki_dir`
is set, and MCP when `transports.mcp` sets a host or a socket. The task engine
and MCP delegation run unless `tasks.enabled` is false. Stdio MCP is not attached.
SQLite embeddings run with chunking disabled. A config or migrate error exits 1.
SIGTERM and SIGINT cancel the process. It exits 0 when the gateway, workers, and
sidecar finish inside the same 10 second grace, and exits non-zero, naming the
work still entered, when they do not. The default bind is
`127.0.0.1:5174` when `rh local` writes the config, or `0.0.0.0:5174` when TLS
for that bind already resolves; `--no-lan` forces `127.0.0.1`. An off-loopback
bind without TLS stops before the node starts, naming the config file. `--port` and `--no-lan` override an existing file in memory and
do not rewrite it. Migrate runs in the CLI before the store opens, because the
runtime does not depend on the migrate crate.

The gateway runs the task engine (`rh agent` is retired, CONTRACT note 74): it
claims its node's `run-task:<node>` jobs, starts each task's child session through
its own session registry, and finishes under the process shutdown token. It also
serves `/api/tasks` and `GET /api/catalog/agents`. `tasks.enabled: false` turns
the engine off.

`rh mcp` serves the memory and wiki tools on stdio, or on streamable HTTP and
a Unix socket when `transports.mcp` selects them. Stdio also serves
`delegate_task` and `list_agents`; a network listener serves `list_agents`.
After those it lists the plug-in creator's tools and the running plug-ins' tools,
read from the gateway over `<state>/plugins/tools.sock`. The sidecar does not
claim jobs. It does not serve web, echo, or skills.

The task engine uses the kernel task ports. The gateway runs the job loop and
serves the task HTTP API, and `rh mcp` exposes delegation through it.

Recording notes are at `../testkit/tests/fixtures/roots/README.md` and
`../testkit/tests/fixtures/gateway/README.md`. Binary integration tests cover TLS,
revocation, identity refusal, websocket snapshots, memory, mesh turns,
configuration checking and signal shutdown. Existing route recordings remain
covered by their workspace replay suites; they are not all replayed through the
binary. Real installed herdr/ACP execution remains in the adapter smoke suites.
