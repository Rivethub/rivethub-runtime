# `rh` command-line contract (B9), first slice

The behaviour operators, scripts and systemd units rely on: command names, flags, exit codes, and the outputs a script
reads. The wording of human-oriented text is RivetHub's own. Names follow the rename: binary `rh` (with `rivetos` as an
alias), state in `~/.rivethub`, environment `RIVETHUB_*` (the `RIVETOS_*` aliases are read by `config`).

## Dispatch
- `rh` with no arguments, `rh help`, `rh --help` and `rh -h` print the command list and exit 0.
- An unknown command prints `Unknown command: <name>` and the command list to stderr, then exits 1.
- A failed command prints `Error: <message>` to stderr and exits 1, unless the command defines another code below.
- `--help` after a command is that command's help (exit 0).

## Commands in this slice
| Command | Behaviour | Exit |
|---|---|---|
| `rh version` | prints `RivetHub v<crate version>`, plus ` (<short git sha>)` when built from a git checkout | 0 |
| `rh config path` | prints the resolved config path (the order `config` already defines) | 0 found; 1 none, with `No config found. Write ~/.rivethub/config.yaml (rh local writes a standalone one) or set RIVETHUB_CONFIG` |
| `rh config show` | a summary of the resolved config: agents (name → provider), providers (name → model), the default agent, the workspace path. No secrets are printed | 0; 1 on a parse error, with `Failed to parse config: <message>` |
| `rh config validate [path]` | validates the file at `path` (default: the resolved path) with `config`'s validation and prints its findings in `config`'s format | 0 valid; 1 unreadable, unparsable or invalid |
| `rh doctor [--json]` | runs checks and reports each as pass, warn or fail: config valid; the state directory exists and is writable; disk space under the state directory (fail below 256 MB free, warn below 1 GB); the memory backend is reachable and its schema version is current (through the store's schema verification, never running migrations); `herdr` on PATH when the terminal is enabled. `--json` prints `{version, timestamp, checks:[{name, status, message}], summary:{pass, warn, fail}}` | 1 if any check fails, else 0 |
| `rh status` | reads the gateway's `/healthz` on the configured host and port (3 s timeout) and prints running or not running with the version and uptime when available; falls back to the pid file under the state directory | always 0 |
| `rh stop` | sends SIGTERM to the pid in the pid file under the state directory and prints `Sent SIGTERM to PID <pid>`. With no pid file: `No running instance found (no PID file).` If the process is gone: `PID file exists but process is not running.` | 0 for those three; 1 on any other error |

The CA and holder commands are now implemented; their approved new B14 contract and acceptance coverage are documented in `crates/cli/README.md`. They have no historical recordings.

Out of this slice (later slices): `start`, `update --mesh`, `init`, `pair`,
`user`, `test`, `agent`, `model`, `keys`, `gateway token`.

## `rh service` (second slice)
Manages the systemd unit that runs the node. Run as root, it manages `/etc/systemd/system/rivethub.service`; otherwise the
user unit `~/.config/systemd/user/rivethub.service`.

| Command | Behaviour | Exit |
|---|---|---|
| `rh service init` | writes the unit: `Type=simple`, `ExecStart=<absolute path of this rh binary> start`, `EnvironmentFile=-<state dir>/.env`, `Environment=RIVETHUB_LOG_LEVEL=info`, `Restart=on-failure`, `RestartSec=5`. It refuses to overwrite an existing unit (`Service file already exists: <path>` and `Delete it first if you want to regenerate.`, exit 1). In user mode it reports whether lingering is enabled for the user (`loginctl show-user`), and prints `sudo loginctl enable-linger <user>` when it is not | 0 written; 1 exists or write error |
| `rh service start` / `restart` | `systemctl [--user] daemon-reload`, then the verb, then prints `systemctl [--user] status rivethub --no-pager` | the `systemctl` verb's failure is exit 1 with its stderr; it is never swallowed |
| `rh service stop` | `systemctl [--user] stop rivethub` | 0 / 1 as above |
| `rh service status` | `systemctl [--user] status rivethub --no-pager`, passed through | `systemctl`'s own exit code |
| `rh service logs` | `journalctl [--user] -u rivethub -f --no-pager`, passed through until interrupted | 0 on interrupt |
| `rh service <other>` | `Unknown action: <name>` and the usage | 1 |

The unit body is generated from one typed struct and serialised in systemd's format. Tests check the generated unit,
the root/user path choice and the refusal to overwrite, and use a fake `systemctl`/`journalctl` on PATH for the verbs.
They never touch the real systemd.

## `rh start` and `rh stop` (third slice)

`rh local` runs the node in one process. It is not a supervised child of `rh start`. Its contract is below.

`rh start` is the node's supervisor. It runs in the foreground and is what the systemd unit's `ExecStart` calls.

| Command | Behaviour | Exit |
|---|---|---|
| `rh start [-c/--config <path>]` | Resolves the config like every other command, then refuses to run if another node already owns the state directory (`rh` state lock held, or a live pid in the pid file), with the same refusal `config` uses for a live `~/.rivetos`. Writes its pid to the pid file under the state directory and holds the state lock. Spawns one child per enabled service root, `rh gateway`, `rh agent` when Postgres memory would start, `rh worker --role <role>` for each configured worker role and `rh mcp` when the MCP transports are enabled, as subcommands of the same binary, passing `--config`. Restarts a child that exits unexpectedly with exponential backoff (1 s doubling to 60 s, reset after 60 s of uptime), logging each restart. On SIGTERM or SIGINT it forwards SIGTERM to every child, waits up to 10 s for them, sends SIGKILL to any that remain, removes the pid file and exits. | 0 after a signalled shutdown; 1 if no config is found (`No config found. Write ~/.rivethub/config.yaml (rh local writes a standalone one) or use --config <path>`) or the state directory is owned by another node |
| `rh stop` | as in the first slice: SIGTERM to the supervisor's pid, which shuts its children down | as above |

Tests drive `rh start` with fake roots: a test binary that sleeps, crashes or ignores SIGTERM. They check restart and backoff with tokio's paused clock, the forwarded shutdown, the SIGKILL after the grace period, pid-file cleanup and the ownership refusal.

## `rh local`

`Config::load` with `Profile::Node`, and `rh config validate`, still require `runtime`, `agents`, and `providers`. `rh local` does not invent those values. When no config exists it writes `memory.backend`, `memory.sqlite.path`, and the gateway bind, then loads that file through `Config::load` with `Profile::Standalone`, which does not require those sections.

`rh local [--port <n>] [--no-lan]` runs one process: the gateway, the SQLite worker roles, and MCP when `transports.mcp` has a host or socket. It claims the state directory, runs `migrate::sqlite` once, and opens one SQLite store. SIGTERM and SIGINT drain that process and exit 0. A config or migrate error exits 1. `--port` defaults to 5174. `--no-lan` binds `127.0.0.1`; otherwise the written bind is `0.0.0.0`. Flags on an existing file apply in memory and do not rewrite it.

`rh local` runs the wiki builder when `memory.sqlite.wiki_dir` is set, and its MCP server exposes `delegate_task` on the SQLite task queue. The supervised `rh agent` process remains the Postgres task service.

## `rh mesh`

`rh mesh` registers and inspects mesh nodes through `gateway::mesh::Registry` and the typed `mesh.json` document. `enroll` mints a roster token with the same `pki` minter as `rh ca token`, writes the holder key under the state directory, and records the node. `renew` calls `rh cert renew` over step-ca mTLS and does not rewrite `mesh.json`. `sync` reads `http://<hub>:<port>/mesh.json` and merges it. `join --manual` reads that same document from the seed and posts `/api/mesh/join`. SSH to `rivethub-hub`, `rivet-ca.sh`, and the legacy YAML seed are not implemented. The roster is `/rivet-shared/rivet-ca/mesh-nodes.conf`, never `mesh.json`. Private keys are not written under `/rivet-shared`.

## Memory and logs scenarios left out

`docker log detection` is not implemented: `rh logs` always reads the `rivethub` journal. `bare-metal tee instructions` are not implemented: `RIVETHUB_BARE_METAL` still reads that journal. `embedded postgres` is not started: memory commands never launch a database. `rh cloud import` is not implemented: a cloud-host import exits 2 and names that command.
