# RivetHub runtime (`rh`)

`rh` is the node runtime of RivetHub. One binary runs a node's gateway (the HTTP and WebSocket API every client uses),
its harness sessions (Chat over ACP, Terminal over a PTY), memory (capture, search, summaries, topics and the wiki),
delegated tasks, the plug-in host, the MCP sidecar, the memory workers, and the mesh and certificate commands. A second
binary, `rivethub-migrate`, applies every schema change. Memory lives in Postgres on a shared datahub, or in one SQLite
file on a standalone machine.

Related repositories: [rivethub-desktop](https://github.com/philbert440/rivethub-desktop) (the native client) and
[rivethub-plugins](https://github.com/philbert440/rivethub-plugins) (plug-ins, their SDKs and templates).

## Install

Every release `v<version>` publishes one archive per target and a `SHA256SUMS` file. Each archive holds `rh`,
`rivethub-migrate` and `LICENSE`. There is no install script; unpack both binaries into a directory on your `PATH`:

```sh
version=0.7.0                      # a release tag without its v
target=x86_64-unknown-linux-gnu    # or aarch64-unknown-linux-gnu, universal-apple-darwin
base=https://github.com/philbert440/rivethub-runtime/releases/download/v$version
curl -fL -O "$base/rh-$version-$target.tar.gz" -O "$base/SHA256SUMS"
sha256sum --ignore-missing -c SHA256SUMS
tar -xzf "rh-$version-$target.tar.gz" -C ~/.local/bin rh rivethub-migrate
rh version
```

Linux builds need glibc 2.35 or newer; on Windows, run the Linux archive under WSL. After that, `rh update` keeps the
installation current:

- `rh update` installs the latest stable release; `rh update --channel beta` the newest release, prereleases included.
  The channel is not remembered: pass `--channel beta` every time you want beta. `--version <tag>` pins one release.
- It verifies the archive against `SHA256SUMS`, runs the new `rivethub-migrate` against the configured memory backend,
  swaps both binaries in place (keeping `rh.previous` and `rivethub-migrate.previous`) and restarts the systemd unit
  unless `--no-restart` is given. On macOS restarting is up to you, so pass `--no-restart`.
- `--mesh` then runs `rh update` over SSH on every node in the mesh registry, at the same release tag.
- `--channel nightly` builds from the clean `main` checkout named by `RIVETHUB_INSTALL_ROOT` instead.

Every merge to `main` publishes the next patch version as a beta prerelease; the Promote workflow marks a beta stable.
[docs/CLI.md](docs/CLI.md) has the release layout and every command.

## Run a node

### Standalone on SQLite: `rh local`

```sh
rh local                   # serve on 127.0.0.1:5174
rh local --ensure          # start one in the background unless one already answers
```

`rh local` is one process on one SQLite file (`~/.rivethub/memory.sqlite`). With no configuration it writes a minimal
`~/.rivethub/config.yaml` (the database path and the gateway bind) and never invents agents, providers or endpoints. It
runs the gateway, the plug-in host and the task engine, and adds, from `memory.sqlite`:

| Setting | Adds |
|---|---|
| `workers: true` | the embedder and the summarizer |
| `tagging: true` | the categorizer (filing and Tidy proposals) |
| `wiki_dir: <dir>` | the wiki builder, writing pages under that directory |
| `embed_endpoint`, `embed_model` | vector and hybrid search; without them search answers keyword results, marked degraded |

MCP is served in the same process when `transports.mcp` sets a `host` or a `socket`. A first run binds `127.0.0.1:5174`,
or `0.0.0.0:5174` when TLS for a LAN bind is already set up (`RIVETHUB_TLS_CERT` and `RIVETHUB_TLS_KEY`). To serve the
LAN later, set `gateway.host: 0.0.0.0` with `gateway.tls_cert` and `gateway.tls_key`; an off-loopback bind without TLS
stops with the file and the ways out. `--no-lan` forces `127.0.0.1` and `--port` picks another port. A SQLite node
serves one person: it stores visibility marks but does not filter reads by them.

### A node on Postgres

1. Prepare the database and run `rivethub-migrate postgres` (see [docs/OPERATIONS.md](docs/OPERATIONS.md)). No service
   migrates on start; each refuses to run behind the schema it needs.
2. Write `~/.rivethub/config.yaml` (below) and check it: `rh config validate`, `rh gateway --check`, `rh doctor`.
3. `rh service init` writes a systemd unit (a system unit as root, a user unit otherwise) that runs `rh start`;
   `rh service start|stop|restart|status|logs` control it. `rh start` supervises `rh gateway`, and `rh mcp` when
   `transports.mcp` names a host or socket, restarting either with backoff.

`rh gateway` serves the API, sessions and terminals, the plug-in host, devices, the certificate routes and the mesh,
and runs the tasks queued for its node. `rh status` reports its health and `rh logs` reads its journal.

### Memory workers on the datahub

```sh
RIVETHUB_WIKI_EXTRACTION=1 rh worker --role wiki-builder
```

`rh worker --role <role>` claims one role's jobs from the Postgres queue and schedules that role's cron entries. Run
one process per role, usually as separate systemd units next to Postgres:

| Role | Jobs |
|---|---|
| `embedder` | `embed-target`, and the sweeps for unembedded rows and unchunked messages |
| `summarizer` | `compact-conversation` (the summary tree), `synthesize-tool-call`, the idle and stale sweeps, `sync-topic-page`, `reap-dead-jobs`, `purge-deleted-sessions` |
| `categorizer` | `file-conversation` (files a conversation into a topic), `propose-tidy` |
| `wiki-builder` | `extract-wiki`, the wiki backfill, `consolidate-wiki`, `recompile-wiki`, the stale-wiki sweep; refuses to start without `RIVETHUB_WIKI_EXTRACTION=1` |
| `all` | every role in one process |

The summarizer, categorizer and wiki builder call the default agent's model. Tuning variables, unit files and memory
limits are in [docs/OPERATIONS.md](docs/OPERATIONS.md).

## Configuration

`rh` reads `RIVETHUB_CONFIG`, else `~/.rivethub/config.yaml` (or `.yml`), else the legacy `~/.rivetos/config.yaml`,
else `./config.yaml`; every service command also takes `-c FILE`. `${NAME}` in a value is replaced from the environment.
State lives in `~/.rivethub/`. `rh config path`, `rh config show` and `rh config validate` inspect the file; validation
requires `runtime`, `agents` and `providers` (only `rh local` runs without them).

```yaml
runtime:
  workspace: ~/work
  default_agent: coder
agents:
  coder: { provider: local-llm, model: example-model }
providers:
  local-llm: { type: openai-compatible, base_url: http://llm.example:8000/v1, model: example-model }
memory:
  backend: postgres
  postgres:
    connection_string: ${RIVETHUB_PG_URL}
    embed_endpoint: http://embed.example:8001/v1
    embed_model: example-embedding-model
gateway:
  host: 127.0.0.1
  port: 5174
  terminal: { enabled: true }
```

| Section | Holds |
|---|---|
| `providers` | model endpoints: `openai-compatible` or `vllm` (`base_url`, `model`, `api_key` or `token_command`), or a harness used headless (`claude-cli`, `codex-cli`, `grok-cli`, `hermes-cli`, `opencode-cli`, `kimi-code`) |
| `agents`, `runtime` | named agents on those providers; `runtime.default_agent` is the model the workers use |
| `memory` | `backend` (`postgres` or `sqlite`), connection and embedding settings, `capture.redaction` (off by default) |
| `gateway` | `host`, `port`, TLS (`tls_cert`, `tls_key`, `tls_ca`, `tls_crl`, `tls_require_client`), `users_file`, `terminal` (`enabled`, `roots`, `trust_claude_launch_dirs`), `allowed_origins`, `certificate_authority`, `devices.gateway_url` |
| `tasks` | `enabled`, and `harnesses.<id>` (`binary`, `model`, `effort`, `cwd`) for the harnesses this node runs |
| `transports.mcp` | network MCP: `host` and `port` (default 5700, needs `auth_token`), or `socket` (an absolute path) |
| `mesh` | `enabled`, `node_name`, `advertise_host`, `agent_channel_port`, TLS paths |

The gateway binds `127.0.0.1:5174` by default. Off loopback it refuses to start without TLS, and with TLS it requires a
client certificate unless `tls_require_client: false`. A caller on loopback is the node's owner. Browser origins other
than the gateway's own must be listed in `gateway.allowed_origins`.

## Harnesses and capture

A session runs one harness on one node. Chat speaks ACP to it: natively for `grok-build` (`grok agent stdio`),
`opencode`, `hermes`, `kimi-code`, `omp` and `dsh` (each `<binary> acp`), and through the `claude-agent-acp` and
`codex-acp` adapters for `claude-code` and `codex`. `cursor` is terminal-only and `cowork` is captured only. Terminal runs
the harness's own TUI under herdr (`gateway.terminal.enabled: true`; the node needs `herdr` installed), and Chat and
Terminal take turns holding a session. Override a binary with `tasks.harnesses.<id>.binary`.

Sessions started outside RivetHub reach memory through `rh capture hook`, run from the harness's hook command. It reads
one hook payload on stdin, posts it to the local node's `/api/capture` (a running `rh local` or `rh gateway`), spools
what the node could not take under `~/.rivethub/capture-spool/` for the next run, and always exits 0:

| `--harness` | Called on | Reads | Captured as |
|---|---|---|---|
| `claude-code` (default) | `UserPromptSubmit`, `PostToolUse`, `Stop`, `SubagentStop`, `SessionEnd` | the payload's `transcript_path` | the session id |
| `grok-build` | `Stop`, `PreCompact`, `SessionEnd` | `$GROK_HOME` (`~/.grok`) | `grok-build:<session>` |
| `codex` | `Stop`, `SessionEnd` | `$CODEX_HOME/sessions` (`~/.codex/sessions`) | `codex:<session>` |
| `opencode` | `session.idle`, `session.error` from an opencode plug-in | `opencode.db` under `$XDG_DATA_HOME/opencode` | `opencode:<session>` |
| `dsh` | `turn/end` from a dsh plug-in | `$DSH_HOME/sessions` (`~/.dsh`) | `dsh:<session>` |

For Claude Code, each of its events gets the same entry in `settings.json`:

```json
{ "hooks": { "Stop": [{ "hooks": [{ "type": "command", "command": "rh capture hook" }] }] } }
```

The per-harness kits that install these hooks live with the clients. A hook run with `RIVETHUB_MODEL_RUN` set captures
nothing, so the workers' own model calls never become conversations. `memory.capture.redaction` (or
`RIVETHUB_CAPTURE_REDACTION`) redacts secrets before they are stored.

### MCP

`rh mcp` gives any MCP client this node's memory: `memory_search` (hybrid, fts, trigram, regex or vector),
`memory_browse`, `memory_get_full`, `memory_stats`, `memory_tags`, `wiki_search`, `wiki_read`, `list_agents`, and on stdio
`delegate_task`; then the plug-in creator's four tools and every running plug-in's tools, named `<plugin>-<tool>`. It
serves stdio by default and streamable HTTP at `/mcp` when `transports.mcp` names a host (bearer `auth_token` required) or
a socket. On a SQLite node `rh mcp` opens the node's SQLite file and serves stdio; the network transports there are
`rh local`'s own.

```json
{ "mcpServers": { "rivethub": { "command": "rh", "args": ["mcp"] } } }
```

## Plug-ins

A plug-in is a program in its own process, described by `rivethub-plugin.json`, that reaches `rh` only through a
per-start token on the node's plug-in API socket. `rh gateway` and `rh local` host them; no plug-in code is loaded into
`rh`.

- **Install:** `POST /api/plugins` with `{"path": "<absolute directory>"}` registers it in place, disabled. Its data,
  socket and logs live in `~/.rivethub/plugins/<id>/`.
- **Permissions:** the manifest asks for `memory.read`, `memory.write`, `sessions.read`, `sessions.control`,
  `topics.read`, `topics.write`, `tasks.read`, `tasks.delegate`, `agents.read`, `notifications.send`, and the host
  permissions `host.files.read`, `host.files.write`, `host.network` and `host.exec`. The owner approves them by enabling
  the plug-in from a paired device (Settings › Plug-ins in the app); a reload that asks for more stops it until they
  are approved again. Every API call is checked against the grant when it arrives.
- **Confinement:** on Linux each plug-in runs under Landlock, limited to its own folders, its program and the system
  directories unless a `host.*` permission widens that. A kernel without full Landlock cannot enable a `host.*` plug-in.
- **What it adds:** views each client draws natively (pages, topic cards, session actions, canvases), web panels,
  agent tools in MCP, event subscriptions (`session.*`, `summary.written`, `task.done`), cron or manual jobs, and a
  settings form.
- **Creator mode:** an agent builds one with the `plugins-templates`, `plugins-scaffold`, `plugins-install` and
  `plugins-status` tools: it scaffolds from a template in `~/.rivethub/plugin-templates/` into `~/.rivethub/workshop/<id>/`,
  installs it and reads its log. It never enables one; a person does.

`GET /api/plugins` lists them; `…/{id}/enable`, `/disable`, `/reload`, `/logs`, `/settings` and `DELETE` manage them
(owner only). The manifest, socket protocol and bodies are CONTRACT row B17 and notes 50 to 60, 71, 90 and 91.

## Memory

Every capture is stored as a conversation of messages, owned by the caller. The workers build on it:

- **Search:** hybrid ranking over full text, trigram, vectors (messages, message chunks and summaries) and recency.
- **Summaries:** each conversation gets a tree of leaf, branch and root summaries, written as it goes idle.
- **Topics:** sessions are filed into work topics (nested up to four levels) by you or by the categorizer, which never
  overrides your placement. Topics carry decisions, a startup context handed to new sessions in that topic, and one
  wiki page per root topic. Entities (people, hosts, models, services, repos and the like) are topics of kind
  `entity` under `type` topics, kept apart from the work tree.
- **Wiki:** the wiki builder extracts each summary into topic and entity pages. On Postgres the pages are a git
  repository at `WIKI_DIR` (default `/rivet-shared/wiki`, or `wiki/` under `RIVETHUB_SHARED_DIR`). A page is rewritten,
  not appended to, within fixed budgets (article 16 KiB, sections 4 KiB each, 48 History lines); what a rewrite drops
  is archived under `archive/<slug>/`.
- **Tidy:** changes to topics, entities and types that need a person (merge, move, rename, close, delete, new topic,
  new type) wait as proposals at `/api/tidy/proposals` until approved or rejected.
- **Flagged jobs:** a job that keeps failing (5 times by default) leaves the queue as a flagged record so the jobs
  behind it run; `/api/memory/flagged` lists them, and each can be retried or dismissed.
- **Backfill:** with `RIVETHUB_WIKI_BACKFILL=1` the wiki builder works through older summaries below all new work.

`rh memory` administers a store: `queue-status`, `retry-failed`, `requeue`, `export` and `import`, `backfill-titles`,
`backfill-tool-synth`, `reembed` (SQLite), `consolidate-wiki`, `recompile-wiki`, `cleanup` (a model-planned topic
cleanup that can be written as one Tidy batch), and `merge-store`. `rh memory --help` lists the flags.

## Tenancy

`gateway.users_file` (or `RIVETHUB_USERS_FILE`) names the node's users:

```json
{
  "ownerUserId": "alice",
  "unmappedIsOwner": false,
  "users": { "alice": { "devices": ["alice-laptop"] }, "bob": { "devices": ["bob-phone"] } },
  "spaces": { "home": { "kind": "household", "name": "Home", "members": ["alice", "bob"] } }
}
```

- Every captured row records its owner: a bearer token's user, a device certificate's user, or the node's owner for a
  loopback caller, a plug-in or `rh capture hook`. Without a users file the owner is `owner`. `"admin": true` lets a
  user pass the node-owner checks too.
- Everything is private to its owner until shared with a space (`household`, `team` or `compartment`; without
  `spaces` the node has one household of every listed user). A session's visibility is its own mark, else its topic's,
  else its owner's default for sessions started after choosing it, else private. Only an item's owner marks it.
- Postgres enforces this with forced row-level security on every owned table. `rh mcp`, plug-ins and tasks act as the
  node's owner; workers and operator commands run as maintenance, except that filing runs as the conversation's owner.
- Wiki pages live in scopes: the owner's pages at the root of `WIKI_DIR`, another person's in `users/<user>/`, a
  space's in `spaces/<space>/`, each its own git repository. A private session's notes never reach a shared page.
- `rh memory merge-store --from <url> --owner <user>` folds a second person's memory database into this one, private
  to them.

## Tasks and delegation

A task is a harness session started by another session. `POST /api/tasks` takes a `goal`, an `agentId` (`agent@node`
pins the node), and optional acceptance criteria and a budget (`maxWallClockMs`, `maxTurns`). The gateway that takes it
picks the node, queues `run-task:<node>`, and that node's gateway starts the child session with the parent's topic
memory and sends the goal as one turn. The task ends with the reply text when the turn ends, `awaiting-input` while an
approval waits, `timeout` or `killed` past its budget, and `POST /api/tasks/{id}/kill` stops it. The child session
stays open and resumable. Agents delegate through MCP `delegate_task` and see targets with `list_agents`.
`tasks.enabled: false` turns tasks off on a node.

## Mesh and certificates

Nodes, agents and devices hold short-lived certificates from a step-ca the operator runs: SPIFFE identities, keys made
on the holder, renewed at two thirds of their life by `rh cert renew`. `rh mesh enroll` enrolls a node, `rh mesh
list|ping|status` inspect the registry, and `rh ca token|revoke|crl|status` mint enrollment tokens and manage
revocation. The gateway proxies signing at `/ca/v1/` when `gateway.certificate_authority` is set, and pairs phones when
`gateway.devices.gateway_url` is set too. See [docs/PKI.md](docs/PKI.md).

## Documentation

- [docs/OPERATIONS.md](docs/OPERATIONS.md): running Postgres, workers and the wiki for a datahub.
- [docs/CLI.md](docs/CLI.md): every command, releases and updates.
- [docs/CONTRACT.md](docs/CONTRACT.md): the boundaries (HTTP API, events, schema, CLI, config, plug-ins) and every
  numbered note that changed them; the payload schemas are in `contract/`.
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): crates, ports, process shape.
- [docs/PKI.md](docs/PKI.md): certificates and step-ca.
- [docs/BUILDER-RULES.md](docs/BUILDER-RULES.md) and [docs/PATTERNS.md](docs/PATTERNS.md): the rules every change is held to.

## Working here

```sh
cargo xtask install-hooks        # installs the xtask binary into target/git-hooks (pre-commit, commit-msg)
cargo xtask check                # what CI runs first: fmt, clippy, dylint, cargo-deny, languages, privacy, secrets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace
cargo build --release -p rivethub --bin rh -p cli --bin rivethub-migrate
```

The repository is Rust only. The git hooks are the `xtask` binary itself: `cargo xtask install-hooks` copies it to
`target/git-hooks/pre-commit` and `target/git-hooks/commit-msg` and points `core.hooksPath` there; the binary dispatches on
the name it is invoked as (pre-commit: staged privacy + fmt; commit-msg: house identity, no attribution trailers).

Every crate depends only downward (foundation ← kernel ← adapters/services ← composition); `deny.toml` fails the build the
moment a `Cargo.toml` breaks that (each internal crate is a ban whose wrappers are exactly the crates allowed to use it).
Production code has no comments, no `allow`, no `unwrap`/`expect`, no `unsafe`; `clippy.toml` disallows std locks, untyped
JSON, runtime construction and environment reads outside their one home, and `lints/` (a Dylint library outside the
workspace, pinned to the nightly its `dylint_linting` version requires) carries the rest of `docs/PATTERNS.md`.
`deny.toml` keeps TLS on rustls and bans `anyhow`, OpenSSL and a second async runtime.

Licensed under Apache-2.0.
