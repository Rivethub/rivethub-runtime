# Operator CLI: B9, B10, B16

`Config::load` with `Profile::Node`, and `rh config validate`, still require `runtime`, `agents`, and `providers`. `rh local` never invents an agent, a provider, or an endpoint. When no config resolves it writes only `memory.backend: sqlite`, the database path under the state directory, and the gateway bind, then loads that file with `Profile::Standalone`, which does not require those sections. `rh local` runs the task engine on the SQLite store unless `tasks.enabled` is false; it serves `/api/tasks` and `/api/catalog/agents` and backs MCP delegation with the same engine. On Postgres the gateway runs its node's tasks; `rh agent` is retired (CONTRACT note 74). Its directory holds the configured agents and the presets in `agents.json`, read at start.

`rh` dispatches through the `cli` composition crate. Bare invocation, `rh help` and `rh --help` print the derived clap
command list, and `rh <command> --help` prints one command's arguments. One typed `cli::Exit` owns exit codes, including
the existing service startup codes. Every visible command:

| Command | Arguments | Does |
|---|---|---|
| `rh version` | | Print the RivetHub version |
| `rh config path` | | Print the resolved configuration path |
| `rh config show` | | Show a configuration summary without secrets |
| `rh config validate` | `[PATH]` | Validate configuration and print findings |
| `rh doctor` | `[--json]` | Check this node without applying migrations |
| `rh status` | | Report gateway health |
| `rh stop` | | Signal the running instance to stop |
| `rh gateway` | `[--check] [-c, --config FILE]` | Run the composed gateway service |
| `rh worker` | `--role embedder\|summarizer\|categorizer\|wiki-builder\|all [--check] [-c, --config FILE]` | Run a memory worker for one role |
| `rh mcp` | `[--check] [-c, --config FILE]` | Run the MCP sidecar |
| `rh local` | `[--port N] [--no-lan] [--ensure [--wait SECONDS]]` | Run a standalone node on SQLite, or start one in the background with `--ensure` |
| `rh update` | `[--channel stable\|beta\|nightly] [--version TAG] [--no-restart] [--mesh] [--ssh-user USER]` | Update this node or the mesh |
| `rh db migrate` | `[-c, --config FILE] [--url URL \| --path FILE]` | Apply database migrations |
| `rh db status` | `[-c, --config FILE] [--url URL \| --path FILE]` | Report schema status read-only |
| `rh service init` | | Write the systemd unit (system unit as root, user unit otherwise) |
| `rh service start`, `rh service stop`, `rh service restart` | | Control the systemd unit |
| `rh service status` | | Show the systemd unit status |
| `rh service logs` | | Print the unit's journal |
| `rh ca token node` | `NAME` | Mint a step-ca token for a node in the operator roster |
| `rh ca token agent` | `NAME` | Mint a step-ca token for an agent in the operator roster |
| `rh ca token device` | `NAME` | Mint a step-ca token for one device name |
| `rh ca revoke` | `SERIAL` | Revoke one certificate by decimal serial |
| `rh ca crl` | | Print the certificate revocation list |
| `rh ca status` | | Show the certificate authority roots |
| `rh cert renew` | `--principal P --ca-url URL --trust-root FILE [--token-file FILE] [--state DIR] [--once]` | Renew this holder's certificate |
| `rh mesh list` | | List mesh nodes |
| `rh mesh ping` | `[--timeout MS]` | Probe mesh nodes (default timeout 5000 ms) |
| `rh mesh enroll` | `[TARGET] [--name NAME] [--advertise ADDR] [--hub-cmd CMD] [--state DIR]` | Enroll this node through step-ca and record it |
| `rh mesh sync` | `[TARGET] [--hub-cmd CMD]` | Merge a hub mesh registry |
| `rh mesh renew` | `[TARGET] [--name NAME] [--hub-cmd CMD] [--state DIR]` | Renew this node's holder certificate |
| `rh mesh join` | `[HOST] [--manual] [--port N] [--name NAME] [--advertise ADDR]` | Join a mesh |
| `rh mesh status` | | Show the mesh registry |
| `rh mesh forget` | `[TARGET] [--force]` | Remove a mesh node |
| `rh memory` | `queue-status`, `retry-failed`, `requeue`, `backfill-tool-synth`, `export`, `import`, `backfill-titles`, `reembed`, `consolidate-wiki`, `recompile-wiki`, `cleanup`, `merge-store` | Administer memory; `rh memory --help` lists each command's flags |
| `rh capture hook` | `[--harness claude-code\|grok-build\|opencode\|codex\|dsh]` | Capture one harness hook payload read from stdin |
| `rh logs` | `[agent] [-f, --follow \| --no-follow] [-n, --lines N] [--since TIME] [--level NAME] [-g, --grep TEXT] [--json] [--bare-metal]` | Read the service journal |

The `rh ca` commands also take `--ca-url URL`, `--root FILE`, `--provisioner-password-file FILE`, `--roster FILE` and
`--crl-out FILE`; `rh mesh enroll` and `rh mesh renew` take `--ca-url URL` and `--root FILE`, `rh mesh enroll` also takes
`--provisioner-password-file FILE` and `--roster FILE`, and every `rh mesh`
command takes `--json` and `--ssh-user USER`. `--check` on a service root validates its configuration and exits
without serving.

`rh local --ensure` starts a standalone node on demand for launchers and capture hooks that run without a service manager. It adopts legacy state as `rh local` does, resolves the address `rh local` would serve with the same `--port` and `--no-lan` (the resolved config, or the bind it would generate), and exits 0 when anything answers that address's `/healthz`, a degraded 503 included. Otherwise it takes a non-blocking lock at `~/.rivethub/run/local-start.lock`, starts `rh local` with the same flags in its own process group with stdin closed and output appended to `~/.rivethub/logs/local.log`, and waits up to `--wait` seconds (default 2, 1 to 300) for health; a caller that finds the lock held only waits. When nothing answers in time it exits 1 naming the address and the log, so a capture hook can spool its batch. The node still refuses a second instance through its pid-file claim.

`rh capture hook` is the entry point for a Claude Code hook command. It reads one hook payload from stdin and captures it through the `capture` library's hook: it resolves the local gateway from the configuration the same way `rh status` does (a standalone `rh local` configuration is enough), posts the batch to `/api/capture`, honours `memory.capture.redaction` and `RIVETHUB_CAPTURE_REDACTION`, spools a batch the gateway could not take under `<state>/capture-spool` for the next run to replay, and appends a line to `<state>/claude-capture.log` after each capture. A `UserPromptSubmit` prompt is held under `<state>/claude-pending/` and posted with the next `Stop`, `SubagentStop` or `SessionEnd` only when the transcript does not hold it; those events capture the whole transcript (CONTRACT note 84). Claude Code writes a turn's last reply to the transcript just after `Stop` starts, so a `Stop` whose `last_assistant_message` is not yet the turn's last reply in the transcript re-reads it every 100 ms for up to 5 seconds before it captures. It never fails the harness: it always exits 0, and a missing configuration or a failed capture is reported on stderr only. A hook run under `RIVETHUB_MODEL_RUN` captures nothing: rh sets it on the harness a model provider such as `claude-cli` runs, so a worker's model calls never become conversations, and that provider's Claude Code sessions are also started with `persistSession` off, so they leave no transcript in the session list. With `--harness grok-build` it is the entry point for a Grok Build hook command instead (`Stop`, `PreCompact` and `SessionEnd`; other events are ignored): it reads the session's whole `updates.jsonl` under `$GROK_HOME` (default `~/.grok`), captures every message as a `rivet-grok` conversation keyed `grok-build:<session>` with the title the session list shows and the payload's `cwd`, finalizes it on `SessionEnd`, posts nothing for a session with no messages yet, and logs to `<state>/grok-capture.log` (CONTRACT note 76). With `--harness opencode` it takes `{session_id, hook_event_name, cwd}` from an opencode plug-in on `session.idle` or `session.error`, reads the session's parts from `opencode.db` under `$XDG_DATA_HOME/opencode` (default `~/.local/share/opencode`), captures them as a `rivet-glm` conversation keyed `opencode:<session>` and logs to `<state>/opencode-capture.log` (CONTRACT note 82). With `--harness codex` it is the entry point for a Codex hook command (`Stop`, and `SessionEnd`, which finalizes): it reads the session's rollout under `$CODEX_HOME/sessions` (default `~/.codex/sessions`), captures it as a `rivet-gpt` conversation keyed `codex:<session>` and logs to `<state>/codex-capture.log` (CONTRACT note 83). With `--harness dsh` it takes `{session_id, hook_event_name, cwd}` from a DeepSeek Harness plug-in on `turn/end` (other events are ignored), reads the session's `session.<version>.jsonl.zstd` (or plain `session.jsonl`) under `$DSH_HOME/sessions/<workspace>/<session>/` (default `~/.dsh`), captures it as a `rivet-deepseek` conversation keyed `dsh:<session>`, finalizes it and logs to `<state>/dsh-capture.log` (CONTRACT note 92).

`rh gateway` serves the gateway and runs its node's tasks. `rh worker --role <role>` runs one worker role, claiming up to four jobs at a time, or `RIVETHUB_WORKER_CONCURRENCY` (a positive count) when set. `RIVETHUB_SUMMARY_LEAF_TOKENS`, `RIVETHUB_SUMMARY_BRANCH_TOKENS` and `RIVETHUB_SUMMARY_ROOT_TOKENS` cap each summary request (defaults 16384, 24576, 32768, room for a reasoning model's thinking and its answer), and `RIVETHUB_EMBED_TIMEOUT_MS` bounds each embedding request (default 30000). `rh mcp` runs the MCP sidecar.
`rh start` stays hidden from help and is the systemd unit's `ExecStart`. It supervises one child of this binary per
configured service root (`gateway`, each sqlite worker role, and `mcp` when that transport has a host or socket),
restarts an unexpected exit with backoff, and forwards SIGTERM. Deployments previously invoking bare `rh` to launch the gateway must use `rh gateway`. The
one-release compatibility binary dispatches through the same entry point.

Browser origins: the gateway answers a request that carries an `Origin` header only when that origin is the
gateway's own origin, the desktop app's built-in origin, or is listed in the gateway's `allowed_origins` setting (the `gateway` section of `config.yaml`, including the file
`rh local` writes). Origins compare by scheme, host and port, so `localhost` and `127.0.0.1` are separate entries. A
web client served by a Vite dev server on port 5273 that talks to the gateway needs that origin listed:

```yaml
gateway:
  allowed_origins:
    - http://127.0.0.1:5273
    - http://localhost:5273
```

An unlisted origin is refused with `origin "<origin>" is not allowed (add it to the gateway allowed_origins setting)`.

A Claude Code terminal opened in a folder it has not seen before waits at its "do you trust the files in this folder"
dialog. `rh` leaves that dialog alone unless the operator opts in:

```yaml
gateway:
  terminal:
    trust_claude_launch_dirs: true
```

With the flag set, before the gateway launches a `claude-code` terminal command it marks the launch directory trusted in
the Claude Code config file (`$CLAUDE_CONFIG_DIR/.claude.json`, or `~/.claude.json`) by setting
`projects.<dir>.hasTrustDialogAccepted` and `hasCompletedProjectOnboarding`. Only a directory that resolves inside the
terminal roots (`gateway.terminal.roots`, see `crates/rivethub/README.md`) or inside a configured `tasks.harnesses.<id>.cwd` is trusted;
any other directory launches normally and Claude Code asks as usual. Every other setting and project in the file is kept,
the file is written through a temporary file and a rename, and a file that cannot be parsed is left untouched and the
launch is refused with the parse error. The flag is off by default because trusting a folder lets that folder's own
`.claude` settings, hooks and MCP servers run without asking. Chat sessions and other terminal commands never touch the
file.

Platforms: `rh` targets Linux and macOS (Apple Silicon). CI builds `rh` and runs the
`cli` tests on `macos-latest`. `rh service` and the restart step of `rh update` manage systemd units only. On macOS
running `rh start` under launchd (a LaunchAgent or LaunchDaemon plist) is the operator's job, so `rh update` on macOS
installs the universal archive and needs `--no-restart`; it refuses platforms without a published archive. The PKI
commands refuse a state, holder or authority path that passes through a symlink; on macOS `/tmp` and `/var` are symlinks into
`/private`, so keep those paths under the home directory (the default `~/.rivethub` is fine).

Doctor calls `migrate`'s read-only schema status library APIs in-process. Postgres uses the async status API;
SQLite runs on a blocking task. The typed status stays in `migrate`. Doctor never runs migrations or opens a store
for writing. An unreachable backend and an old or unsupported schema fail the memory check explicitly.

Configuration discovery, parsing, validation, defaults, and compatibility notices remain in `config`. Show prints only
agent/provider mappings, default agent, and workspace. Status uses the gateway binding configuration and the shared
health response, with a three-second HTTP timeout and a diagnostic before PID fallback. Stop reads the configured
state directory's PID file, rejects group/system PIDs and out-of-directory symlinks, and sends SIGTERM.

The supplied `testkit` CLI fixture directory contains only its README, with no executable transcripts. Tests exercise
that README's scenarios and check its complete deferred-command list against named reasons. The fixture inventory
check fails if transcripts are added, requiring their integration instead of silently skipping them. Tests also cover
real binary dispatch, the compatibility alias, the existing gateway's health route and graceful shutdown, invalid
PID files and symlink boundaries, real signal delivery, and read-only doctor checks against both backends. Schema
tests assert unchanged database state. Timing tests await events with deadlines.

`rh update [--channel stable|beta|nightly] [--version TAG] [--no-restart] [--mesh] [--ssh-user USER]`
updates Linux x86_64 and aarch64 and macOS installations. Stable is the default and resolves the latest published
stable GitHub release, or the exact tag supplied with `--version`; it refuses prereleases. Beta resolves the highest
semantic version among published releases, prereleases included, or the exact tag supplied with `--version`. The
updater downloads the release archive for the running platform, checks its SHA-256 against the release's
`SHA256SUMS`, and only then unpacks `rh` and `rivethub-migrate` from it; an archive missing either binary is refused.
HTTPS redirects are supported; HTTPS downloads cannot downgrade to HTTP. Version tags are URL path components, never
shell expressions.

Every release tag `v<version>` carries one archive per target and one `SHA256SUMS` listing the archives:

| Target | Archive | Built on |
|---|---|---|
| `x86_64-unknown-linux-gnu` | `rh-<version>-x86_64-unknown-linux-gnu.tar.gz` | `ubuntu-22.04` (glibc 2.35 or newer) |
| `aarch64-unknown-linux-gnu` | `rh-<version>-aarch64-unknown-linux-gnu.tar.gz` | `ubuntu-22.04-arm` (glibc 2.35 or newer) |
| `universal-apple-darwin` | `rh-<version>-universal-apple-darwin.tar.gz` | `macos-latest`, arm64 and x86_64 joined with `lipo` |

`<version>` is the tag without its leading `v`: tag `v0.5.3` publishes `rh-0.5.3-x86_64-unknown-linux-gnu.tar.gz`
at `https://github.com/philbert440/rivethub-runtime/releases/download/v0.5.3/rh-0.5.3-x86_64-unknown-linux-gnu.tar.gz`.
Each archive holds `rh`, `rivethub-migrate` (which `rh update` runs to migrate before the swap) and `LICENSE` at its top
level. Windows users run the Linux archive under WSL. Before upload, every build job rejects a Linux binary that needs
a glibc newer than 2.35, unpacks the archive, and checks that `rh version` prints the release version and commit, that
`rh db migrate` creates a fresh SQLite store, and that `rh doctor` then passes with a current schema.

Every merge to `main` tags the next patch version (one past the highest `vX.Y.Z` tag, or the workspace package version
if that is higher) and publishes it as a beta prerelease with all three archives. The Promote workflow turns a beta into
the latest stable release without rebuilding. A pushed `v<version>` tag must match the workspace package version and
publishes that release. Published archives are never rebuilt or replaced, so a pinned URL and SHA-256 stay valid.

Nightly requires `RIVETHUB_INSTALL_ROOT` to name an absolute, clean source checkout on `main`. The deprecated
install-root alias remains in `config` and emits its existing warning. The updater runs
`git pull --ff-only origin <branch>` then `cargo build --release --locked` for both binaries into an isolated staging
directory. With nightly, `--version` asserts that the updated checkout resolves to that commit or tag;
a different revision is refused before building. The updater never changes the checkout's branch or discards edits.

Updates require a configured memory backend. The staged migration binary applies that backend's migrations before
replacement or restart, including with `--no-restart`. A download, checksum, build, or migration failure leaves the
installed executables untouched. A lock serializes updates to the installation. Each replacement uses a rename on
the installation filesystem; `rh.previous` and `rivethub-migrate.previous` retain the previous executables for manual
rollback. If the second rename fails, the first is restored. Schema changes remain forward-compatible: binary rollback
does not undo migrations. Restart uses the installed system or user systemd service through the existing service CLI
implementation. A restart failure exits 1 and retains both backups; it does not undo the database migration.

Mesh mode reads the existing typed registry at `<RIVETHUB_SHARED_DIR>/mesh.json` through the gateway's registry reader.
It resolves the local node name with the same config, environment, and operating-system hostname precedence as the runtime. It validates every SSH destination and concurrently preflights SSH
before updating locally, then visits every reachable peer in registry order. Each peer executes its own `rh update`;
`--mesh` is never forwarded. Stable and beta peers receive the exact release tag installed locally. The explicit SSH user overrides the registry user; absent both, SSH selects its normal user.
Batch mode and strict host-key checking use the operator's existing SSH trust. Paths and secrets are not interpolated
into a remote shell. Unreachable peers and failed peer updates are reported, other reachable peers continue, and the
aggregate exit is 1 if any peer failed. Local failure stops the rollout. Success exits 0; cancellation exits 1.

`rh db migrate|status [--config FILE] [--url URL | --path FILE]` uses the migration library for the selected Postgres
or SQLite backend. Without an explicit database, it resolves the normal config, including minimal standalone configs.
Status is read-only and reports `current`, `required`, and `pending`; migration reports the resulting status and is
idempotent. SQLite operations run on blocking workers. Cancellation drains an in-flight migration before reporting cancellation. Migration is an explicit operator action, never runtime startup
behavior introduced by these commands. Store errors direct operators to `rh db migrate`.

No update/db transcripts or their referenced inventory sections were supplied in the repository's CLI fixture directory.
The new tests cover the behavior above using isolated databases, a local HTTP release server, and fake process executables.
The update/db skip list is empty and checked; the fixture inventory check fails when new recordings arrive. Existing
README contract tests remain unchanged. Historical update/db wording and exit-code parity cannot be established without
those recordings; no transcript or TypeScript artifact was invented to fill that gap.
