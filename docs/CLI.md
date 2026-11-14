# Operator CLI: B9, B10, B16

`Config::load` with `Profile::Node` still requires `runtime`, `agents`, and `providers`; `rh config validate`, `rh status` and `rh doctor` also accept a SQLite config that passes with `Profile::Standalone`. `rh local` never invents an agent, a provider, or an endpoint. When no config resolves it writes only `memory.backend: sqlite`, the database path under the state directory, and the gateway bind, then loads that file with `Profile::Standalone`, which does not require those sections. `rh local` runs no task engine; delegation, `/api/tasks` and `/api/catalog/agents` were removed (CONTRACT note 132), and `rh agent` was retired earlier (CONTRACT note 74).

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
| `rh auth token-path` | `[--json]` | Print where the local owner token file lives |
| `rh auth rotate` | | Replace the local owner token and end every browser session |
| `rh open` | `[--no-browser]` | Open the web app in your browser, signed in with a one-time code |
| `rh status` | | Report gateway health |
| `rh stop` | | Signal the running instance to stop |
| `rh gateway` | `[--check] [-c, --config FILE]` | Run the composed gateway service |
| `rh worker` | `--role embedder\|summarizer\|categorizer\|wiki-builder\|all [--check] [-c, --config FILE]` | Run a memory worker for one role |
| `rh mcp` | `[--check] [-c, --config FILE]` | Run the MCP sidecar |
| `rh local` | `[--port N] [--no-lan] [--no-cloud] [--ensure [--wait SECONDS] [--json]]` | Run a standalone node on SQLite, or start one in the background with `--ensure` |
| `rh update` | `[--channel stable\|beta\|nightly] [--source rivethub-cloud\|github] [--version TAG] [--no-restart]` | Update this node |
| `rh db migrate` | `[-c, --config FILE] [--url URL \| --path FILE]` | Apply database migrations |
| `rh db status` | `[-c, --config FILE] [--url URL \| --path FILE]` | Report schema status read-only |
| `rh service init` | | Write and enable the systemd unit (system unit as root, user unit otherwise); it runs `rh start` for a node config and `rh local` for a standalone SQLite config or when no config exists yet |
| `rh service start`, `rh service stop`, `rh service restart` | | Control the systemd unit |
| `rh service status` | | Show the systemd unit status |
| `rh service logs` | | Print the unit's journal |
| `rh memory` | `queue-status`, `retry-failed`, `requeue`, `backfill-tool-synth`, `export`, `import`, `backfill-titles`, `reembed`, `consolidate-wiki`, `recompile-wiki`, `cleanup`, `merge-store`, `share`, `receive`, `key show`, `trust add\|list\|remove` | Administer memory; `rh memory --help` lists each command's flags |
| `rh share login`, `rh share logout` | `[<server>]` | Sign this machine in to a share server (rivethub.cloud by default, or a team server) with a device code in the browser, or revoke and forget that login; `rh memory share` uploads with it. A hub policy (`cloud.disabled`, `memory.share.allowed_servers`) limits which servers these reach; `rh share logout` always forgets the login, and revokes it only at a server the policy admits (CONTRACT note 145) |
| `rh share status` | | List the share servers this machine is signed in to |
| `rh capture hook` | `[--harness claude-code\|grok-build\|opencode\|codex\|dsh]` | Capture one harness hook payload read from stdin |
| `rh capture backfill` | `[--harness claude-code\|grok-build\|opencode\|codex\|dsh]... [--dry-run] [--json]` | Capture every stored session of the harness stores, Claude Code subagents included, through the writer the hooks use |
| `rh capture spool` | `[--requeue-dead]` | Prints how many captured batches wait in the spool and how many are dead-lettered; `--requeue-dead` moves every dead-lettered batch back to the spool for another try |
| `rh harness status` | `[--json]` | Show which harnesses are found, how they capture and whether recall is wired |
| `rh harness connect` | `<id> [--capture \| --recall] [--dry-run]` | Add rh's hook or plug-in and its MCP entry to a harness, printing the diff and backing the file up first; for `opencode --capture` rh writes the plug-in embedded in it, or the file named by `RIVETHUB_OPENCODE_PLUGIN`. For `dsh`, rh writes `$DSH_HOME/plugins/rivethub-capture.mjs` and a home-level `$DSH_HOME/cordis.patch.yml` that covers every profile, plus a recall row for `@deepseek-ai/dsh-mcp-client` when that package sits next to `dsh`. Without `--capture` or `--recall`, a part rh cannot write is skipped and named on stderr, and the closing line says which part was connected; when no part can be written it exits 1 |
| `rh harness disconnect` | `<id> [--capture \| --recall] [--dry-run]` | Remove rh's hook and MCP entry from a harness |
| `rh logs` | `[agent] [-f, --follow \| --no-follow] [-n, --lines N] [--since TIME] [--level NAME] [-g, --grep TEXT] [--json] [--bare-metal]` | Read the service journal |

`rh memory receive <code>` and `rh memory ingest-bundle <path>` check a bundle for secrets and prompt injection before they write anything (CONTRACT note 149). A bundle with findings imports nothing: the command prints `held: ...` and the escaped report, keeps the ciphertext, and exits 3 (`cli::Exit::Held`; `rh gateway` uses 3 for a failed bind). Rerun with `--session <n>=import|skip|hold-spans` (repeatable; `n` counts sessions from 1) for each flagged session, or `--all=import|skip|hold-spans`. `hold-spans` imports the session with each finding's span replaced by `[HELD:<rule id>]`. A session with a secret finding allows only `skip` or `hold-spans` unless `--force` is given. `--dry-run` runs the same checks and writes nothing.

`--check` on a service root validates its configuration and exits without serving. `rh mesh`, `rh ca`, `rh cert` and
`rh update --mesh` were removed with the mesh and client certificates (CONTRACT note 132).

`rh local --ensure` starts a standalone node on demand for launchers and capture hooks that run without a service manager. It adopts legacy state as `rh local` does, resolves the address `rh local` would serve with the same `--port` and `--no-lan` (the resolved config, or the bind it would generate), and exits 0 when a RivetHub node answers that address's `/healthz`, a degraded 503 included: only a 200 or 503 answered at that address itself (a redirect is refused) whose body names the node with non-blank `name` and `node` counts; every other health field is ignored, so a newer node still counts. When something else answers there it starts nothing and exits 1 with `port N is in use by something that is not a RivetHub node (<origin>/healthz answered <status> without a RivetHub health body); stop it or use --port`. When nothing answers it takes a non-blocking lock at `~/.rivethub/run/local-start.lock`, starts `rh local` with the same flags in its own process group with stdin closed and output appended to `~/.rivethub/logs/local.log`, and waits up to `--wait` seconds (default 2, 1 to 300) for health; a caller that finds the lock held only waits. When nothing answers in time it exits 1 naming the address and the log, so a capture hook can spool its batch. The node still refuses a second instance through its pid-file claim. With `--json` it prints one JSON object on one line to stdout instead of the sentence, for launchers such as the desktop app: `{"outcome":"started"|"already_running","origin":"http://127.0.0.1:5174","port":5174,"pid":1234,"node":"<node name>","version":"0.8.1"}` on exit 0, where `pid` is read from the node's pid file and is `null` when the answering node keeps none in this state directory, and `version` is `null` when the node does not report one; and `{"outcome":"failed","reason":"port_in_use"|"not_ready"|"error","error":"<message>","origin":<string or null>,"port":<number or null>}` on exit 1, with the same `Error: <message>` line on stderr. `origin` and `port` are `null` only for `error`, which covers every other failure, such as an unreadable config or a node that could not be spawned.

`rh local --no-cloud`, or `RIVETHUB_CLOUD=off` in the environment, is the install-time switch for the hub cloud policy (CONTRACT note 145). When no config resolves, the config `rh local` writes also holds `cloud: {disabled: true}`. When a config already exists and does not set `cloud.disabled: true`, `rh local` exits 1 and names the file to add `cloud: {disabled: true}` to; it never edits that file and never starts a node with the cloud on. `--ensure` checks the same before it probes or starts a node, and passes `--no-cloud` to the node it starts. It then reads the node's `/healthz` `policy`, and exits 1 when that node is not running with the cloud disabled, for example one started before the policy, telling the operator to restart it; it never restarts a node itself. `RIVETHUB_CLOUD=on` or unset changes nothing; any other value exits 1.

`rh capture hook` is the entry point for a Claude Code hook command. It reads one hook payload from stdin and captures it through the `capture` library's hook: it resolves the local gateway from the configuration the same way `rh status` does (a standalone `rh local` configuration is enough), posts the batch to `/api/capture`, honours `memory.capture.redaction` and `RIVETHUB_CAPTURE_REDACTION`, spools a batch the gateway could not take under `<state>/capture-spool` for the next run to replay, and appends a line to `<state>/claude-capture.log` after each capture. When `memory.capture.gateway` is set (`url` and `ca` required, `token` optional), it posts to that gateway instead, over HTTPS only, trusting only `ca` and sending `token` as a bearer token, so a machine that runs no node captures as the user whose users-file entry lists that token's SHA-256 (CONTRACT notes 120 and 132). A `UserPromptSubmit` prompt is held under `<state>/claude-pending/` and posted with the next `Stop`, `SubagentStop` or `SessionEnd` only when the transcript does not hold it; those events capture the whole transcript (CONTRACT note 84). Each of `SubagentStop`, `Stop` and `SessionEnd` also captures the session's subagent transcripts as conversations of their own (CONTRACT note 148). Claude Code writes a turn's last reply to the transcript just after `Stop` starts, so a `Stop` whose `last_assistant_message` is not yet the turn's last reply in the transcript re-reads it every 100 ms for up to 5 seconds before it captures. It persists each batch to the spool before it posts or replays anything, so no later failure loses it, and it never evicts a spooled or dead-lettered batch: the spool has no count or size cap, and disk space is the only limit. A permanent 4xx moves a batch to `dead/`, but 408 and 429 stay in the spool and retry, a `Retry-After` in seconds (up to an hour) setting when. `rh doctor` and `rh status` count dead-lettered batches, and `rh capture spool --requeue-dead` moves them back. A spool entry that cannot be read moves to `<state>/capture-spool/dead/` and the rest still replay. The hook logs one warning each time the spool passes 1,000 entries or 100 MB, and `rh status` and `rh doctor` warn while it is past either. It does not block the harness: a missing configuration or a failed delivery is reported on stderr and exits 0, but a batch that cannot be written to the spool (a full disk, say) exits 1 with `capture batch was NOT saved` on stderr, never 2, which Claude Code treats as a block. A hook run under `RIVETHUB_MODEL_RUN` captures nothing: rh sets it on the harness a model provider such as `claude-cli` runs, so a worker's model calls never become conversations, and that provider's Claude Code sessions are also started with `persistSession` off, so they leave no transcript in the session list. With `--harness grok-build` it is the entry point for a Grok Build hook command instead (`Stop`, `PreCompact` and `SessionEnd`; other events are ignored): it reads the session's whole `updates.jsonl` under `$GROK_HOME` (default `~/.grok`), captures every message as a `rivet-grok` conversation keyed `grok-build:<session>` with the title the session list shows and the payload's `cwd`, finalizes it on `SessionEnd`, posts nothing for a session with no messages yet, and logs to `<state>/grok-capture.log` (CONTRACT note 76). With `--harness opencode` it takes `{session_id, hook_event_name, cwd}` from an opencode plug-in on `session.idle` or `session.error`, reads the session's parts from `opencode.db` under `$XDG_DATA_HOME/opencode` (default `~/.local/share/opencode`), captures them as a `rivet-glm` conversation keyed `opencode:<session>` and logs to `<state>/opencode-capture.log` (CONTRACT note 82). With `--harness codex` it is the entry point for a Codex hook command (`Stop`, and `SessionEnd`, which finalizes): it reads the session's rollout under `$CODEX_HOME/sessions` (default `~/.codex/sessions`), captures it as a `rivet-gpt` conversation keyed `codex:<session>` and logs to `<state>/codex-capture.log` (CONTRACT note 83). With `--harness dsh` it takes `{session_id, hook_event_name, cwd}` from a DeepSeek Harness plug-in on `turn/end` (other events are ignored), reads the session's `session.<version>.jsonl.zstd` (or plain `session.jsonl`) under `$DSH_HOME/sessions/<workspace>/<session>/` (default `~/.dsh`), captures it as a `rivet-deepseek` conversation keyed `dsh:<session>`, finalizes it and logs to `<state>/dsh-capture.log` (CONTRACT note 92).
`rh capture backfill` posts every session a harness store holds, so memory can reach 100% of what the stores keep. It resolves the gateway and the writer exactly as `rh capture hook` does, so a local node gets the owner token the hook sends and a spooled batch replays the same way, and walks the stores under `$CLAUDE_CONFIG_DIR` (default `~/.claude`), `$GROK_HOME`, `$XDG_DATA_HOME/opencode`, `$CODEX_HOME` and `$DSH_HOME` (default `~/.dsh`). Each `--harness` limits the run to one of them, and none means all five. For `claude-code` it posts each top-level transcript and each subagent transcript (`subagents/agent-*.jsonl` and `subagents/workflows/<run>/agent-*.jsonl`) with the batch and event ids the hook writes (CONTRACT note 148), so a session the hooks already captured adds nothing; the other harnesses go through the reader and the title their hooks use, and `dsh` posts each `sessions/<workspace>/<session>/` transcript under the ids its `turn/end` hook writes. A backfilled conversation is never finalized, so Postgres keeps its recorded `updated_at`. It posts one session at a time, prints a line per harness with the sessions it read and the messages the gateway added, and a second run adds nothing. `--dry-run` reads and counts the sessions and posts nothing, and `--json` prints `{"dry_run":false,"harnesses":[{"harness","sessions","messages","added","failures":[{"session","error"}]}]}` on one line. A session that cannot be read or that the gateway rejects is listed on stderr as `<harness> <session>: <error>` and the run goes on. A gateway that cannot be reached, or that refuses the owner token with 401, stops the run after it spools that one batch, so a down node does not fill the spool. It exits 1 when any session failed and 0 otherwise. `cursor`, `kimi-code`, `hermes`, `omp` and `cowork` are not backfilled: `cursor`'s reader does not parse the transcripts Cursor writes (CONTRACT note 148), and the rest have no capture writer yet.

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

Platforms: `rh` targets Linux and macOS (Apple Silicon). CI builds `rh` and runs the
`cli` tests on `macos-latest`. `rh service` manages systemd units only. On macOS running `rh start` under launchd (a
LaunchAgent or LaunchDaemon plist) is the operator's job; `rh update` on macOS installs the universal archive and
restarts only a node `rh local --ensure` started. It refuses platforms without a published archive.

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

`rh update [--channel stable|beta|nightly] [--source rivethub-cloud|github] [--version TAG] [--no-restart]`
updates Linux x86_64 and aarch64 and macOS installations. Stable is the default and resolves the latest published
stable GitHub release; Beta resolves the highest semantic version among published releases, prereleases included.
Either way, a resolved release that is not newer than the running `rh` installs nothing: the updater prints
`rh X is already newer than the latest <channel> release Y; nothing to do` or `rh X is already up to date` and exits 0.
`--version TAG` pins exactly that published release on any channel, prerelease or not, older or not; a tag with no
release, or a draft, is refused by name. The
updater downloads the release archive for the running platform, checks its SHA-256 against the release's
`SHA256SUMS`, and only then unpacks `rh` and `rivethub-migrate` from it; an archive missing either binary is refused.
HTTPS redirects are supported; HTTPS downloads cannot downgrade to HTTP. Version tags are URL path components, never
shell expressions.

After migrating and swapping the binaries the updater restarts the node unless `--no-restart` is given. With a systemd
unit (`rh service init`) it runs `systemctl restart`. A directory or other non-file at the unit path is refused by name.
Without a unit it reads the node's pid file: no node running means nothing to restart; a node whose command line is
`rh local` gets SIGTERM, and once it has exited (within 30 seconds) the new binary runs `rh local --ensure --wait 30`
with the node's own `--port`, `--no-lan` and `--no-cloud`, which starts it in the background and confirms its health; a
node that `rh local` did not start (such as `rh start` in a terminal) is left running with a note to restart it.

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

Stable and beta come from one of two sources. Without `--source`, the updater uses the install's update source:
`rivethub-cloud` once this install is signed in to rivethub.cloud (Settings, Account, updates and privacy), unless
the owner chose `github` there, and `github` while it is signed out. `--source` overrides that choice for one run.
The `github` source is the release flow above. The `rivethub-cloud` source asks
`GET <cloud.url>/v1/updates/latest?channel=<channel>&os=<os>` with the install token from
`<state>/account/install.json`, where `<os>` is `linux-x86_64`, `linux-aarch64`, `macos-x86_64` or `macos-aarch64`.
It verifies the manifest's Ed25519 signature over the raw SHA-256 of the archive before it downloads anything, and
then checks the archive against that SHA-256. Only then does it unpack `rh` and `rivethub-migrate`, migrate, swap and
restart as above. An archive the gateway already downloaded and verified under `<state>/account/updates/` is reused
after the same SHA-256 check. The release public key is compiled in. `cloud.update_key` in config.yaml (standard
base64 of the 32-byte key) replaces it, and `cloud.url` (default `https://rivethub.cloud/`) names the cloud. The cloud
source installs the latest release of the channel, so `--version` with it is refused. A signed-out install, a missing
release, a signature or checksum failure exits 1 and leaves the installed executables untouched. Nightly ignores the
source. With `cloud: {disabled: true}` in config.yaml the source is always `github`, and `--source rivethub-cloud`
exits 1 with the hub policy message (CONTRACT note 145).

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

`rh auth token-path` prints the path of `<state>/local-owner.token` (`--json` prints `{"path": "..."}`) so a script can read the token for `Authorization: Bearer $(cat $(rh auth token-path))`. `rh auth rotate` posts to `/api/auth/rotate` with the current token; the gateway writes the new file and ends every browser session. With the gateway down it deletes the token file and the next start makes a new one, but never on a mere connect error: if the pid file names a live process it refuses and asks you to run `rh stop`. A command that sends the token refuses to when the port's listener is not owned by your uid. Every command that calls the local gateway reads that file through one constructor and sends no `Authorization` header when it is missing, so a new `rh` still works against a running 0.8.0 gateway. `rh open` reads the token the same way, posts to `/api/auth/login-codes`, prints `<origin>/login#code=<code>` (the code rides in the fragment, which browsers never send) and opens it with the system opener. `--no-browser`, or a Linux session with neither `DISPLAY` nor `WAYLAND_DISPLAY`, only prints the URL. The code lives 60 seconds and works once. `rh status` points at `rh open` when the gateway answers 401.
