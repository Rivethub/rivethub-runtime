# Service composition

`Config::load` with `Profile::Node` still requires the `runtime`, `agents`, and `providers` sections; `rh config validate` also accepts a SQLite config that passes with `Profile::Standalone`. `rh local` does not invent an agent, a provider, or an endpoint. When no config file resolves it writes only the SQLite database path and the gateway bind, then loads that file with `Profile::Standalone`, which does not require those sections. A present section is still validated. `rh local` composes the gateway, configured workers, and MCP sidecar in one process over SQLite; `memory.sqlite.wiki_dir` enables the wiki builder. Standalone and service roots resolve the node identity through the same resolver.

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

The gateway composition supplies Postgres memory, the stored-session reads,
the users-file identity, the Claude transcript reader, the configured providers
(including harnesses run headless as providers), the plug-in host, and the
plug-in notice registry. HTTP routes use the gateway router: capture, memory and
tags, the stored sessions and their transcripts, topics, entities, review, Today,
Tidy, the wiki and plug-ins. Every request to the composed router requires a
resolved identity: a users-file bearer token, a plug-in, or a loopback caller as
the node's owner. A bearer token that names no user is refused, even on loopback.

Under the `gateway` configuration section (`den` is its deprecated alias):

- `tls_cert` and `tls_key` select the server identity. The listener asks for no
  client certificate.
- `users_file` (overridden by `RIVETHUB_USERS_FILE`, with the deprecated
  `RIVETOS_USERS_FILE` alias) selects the B11 JSON registry: `ownerUserId`, `unmappedIsOwner`,
  and `users`, a map of user IDs to records with optional `tokens` (hex SHA-256
  digests of bearer tokens), `admin`, `pgUrl`, `envFile` and `persona`. An
  explicit missing or invalid file, or a token digest that is not hex SHA-256 or
  repeats, refuses startup. Without a registry, the local identity is `owner`.

The removed keys (`terminal`, `commands_file`, `files_root`, `files_open`,
`allowed_harnesses`, `advertise_mdns`, the client-certificate keys, every `mesh`
key except `node_name`, and `runtime.profile`) load with one warning each and
are ignored.

Before moving the compatibility state directory, startup acquires existing
root-level `.pid`/`.lock` files and checks their PID owners. A live PID, held lock,
unreadable file or malformed PID refuses the move. Successful stale-file checks
retain the locks through rename. `--check` never performs the move.

## Current checkout limits

The SQLite store implements the kernel memory ports, and `rh local` composes it. The
gateway and worker roots open Postgres only; `rh mcp` opens the SQLite file of a
SQLite config and serves stdio. An explicit SQLite or PGlite
selection fails with a typed startup error and is never redirected to Postgres.

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
is set, and MCP when `transports.mcp` sets a host or a socket. Stdio MCP is not attached.
SQLite embeddings run with chunking disabled. A config or migrate error exits 1.
SIGTERM and SIGINT cancel the process. It exits 0 when the gateway, workers, and
sidecar finish inside the same 10 second grace, and exits non-zero, naming the
work still entered, when they do not. The default bind is
`127.0.0.1:5174` when `rh local` writes the config, or `0.0.0.0:5174` when TLS
for that bind already resolves; `--no-lan` forces `127.0.0.1`. An off-loopback
bind without TLS stops before the node starts, naming the config file. `--port` and `--no-lan` override an existing file in memory and
do not rewrite it. Migrate runs in the CLI before the store opens, because the
runtime does not depend on the migrate crate.

`rh mcp` serves the memory and wiki tools on stdio, or on streamable HTTP and
a Unix socket when `transports.mcp` selects them. After those it lists the plug-in creator's tools and the running plug-ins' tools,
read from the gateway over `<state>/plugins/tools.sock`. The sidecar does not
claim jobs. It does not serve web, echo, or skills.

Recording notes are at `../testkit/tests/fixtures/roots/README.md` and
`../testkit/tests/fixtures/gateway/README.md`. Binary integration tests cover TLS,
identity refusal, memory, configuration checking and signal shutdown. Existing route recordings remain
covered by their workspace replay suites; they are not all replayed through the
binary. Real installed ACP execution remains in the adapter smoke suites.
