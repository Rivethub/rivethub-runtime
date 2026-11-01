# RivetHub Rust runtime — the boundary contract

Decided 2026-10-12 by Phil. This document is the whole definition of "drop-in replacement": the Rust runtime (RivetHub) must match the
TypeScript runtime (RivetOS) (`origin/main`, commit c72e851f and later) at every boundary listed here, and nowhere else. Internals are
designed fresh in idiomatic Rust (see ARCHITECTURE, the design artifact). Nothing in this document describes how the
TypeScript is built; it describes only what other software, people and files observe.

Reference material that survives from the first attempt (read-only; `/rivet-shared/plans/rust-rewrite/`):
- `inventory/*.md` — what the TypeScript does at each boundary, with file:line evidence (3,535 lines, 8 documents).
- `golden/` — recordings of the TypeScript's real inputs and outputs, produced by the recorders in `tools/`.
- `PLAN.md` TypeScript-bugs table — behaviour the owner decided to fix rather than reproduce.

## 1. Boundaries

| # | Boundary | Who observes it | Spec (inventory) | Pinned by (recording) | Acceptance test |
|---|---|---|---|---|---|
| B1 | Gateway HTTP + WS API (`/api/*`, `/files/*`, `/term`, `/event(s)`, `/sessions`, `/state`, `/healthz`, `/mesh.json`, static SPA, `/ws`; the Topics routes `GET/POST /api/topics`, `GET /api/topics/search?q=`, `GET/PATCH/DELETE /api/topics/{id}` (note 29), `POST /api/topics/{id}/merge`, `GET /api/topics/{id}/sessions`, `GET /api/topics/filing` and `GET /api/sessions/suggestions` (note 70), `GET/PUT /api/topics/{id}/page` (note 63), `GET/POST /api/topics/{id}/decisions`, `PATCH /api/topics/{id}/decisions/{decision}`, `POST /api/topics/{id}/decisions/{decision}/supersede` (note 27) and `GET/PUT/DELETE /api/sessions/{session}/placement`; `POST /api/harness-sessions` with optional `topicId` and `prompt`, answering with optional `startupContext`, and `GET /api/harness-sessions/{session}/startup-context` (note 24); `POST /term` and `POST /api/terminal` with optional `topicId` (note 25); the tool output route `GET /api/harness-sessions/{session}/tools/{tool}/output`; the hub state route `GET/PUT /api/hub/state` and its `hub.state` notice on `/api/events/ws`; the `topic.changed` notice (note 110); session deletion `DELETE /api/sessions/{session}`, `POST /api/sessions/delete` and `GET /api/sessions/trash` (note 75) and `PUT /api/sessions/{session}/keep` (note 85); the task routes `GET/POST /api/tasks`, `GET /api/tasks/{id}`, `GET /api/tasks/{id}/wait`, `POST /api/tasks/{id}/kill` and `GET /api/catalog/agents` (note 74); the device update routes `GET /api/updates/manifest` and `GET /api/updates/{file}` (note 94)) | RivetHub web/Android/Electron, hooks, mesh peers, scripts | gateway-api.md §2–§17 (route table §4, aliases §5) | `golden/gateway/` (28 files, recorded traffic incl. startup refusals) + `golden/term/` (17) | replay every recorded exchange: same status, headers (names case-insensitive), body; WS frames in order |
| B2 | Session event stream v1 (`AgentEvent`, `v: 1`) and the room/session snapshot; a `POST /events` batch skips the elements of a kind this node doesn't know, keeps an unknown activity as `thinking`, and keeps the rest (note 73); `approval.wait` (note 81) | RivetHub clients `/ws` clients, `POST /event(s)` producers | gateway-api.md §6, core-domain.md §1.10 | gateway + term recordings (events and snapshots) | accept/reject decisions and emitted snapshots identical on recorded inputs |
| B3 | Capture ingest: hook payloads from every harness → `POST /api/capture` batch body | 10 harness integration kits (claude, codex, grok, kimi, qwen, opencode, hermes, pi, cursor, cowork) | data-layer.md §3.1–§3.6; harness-control-plane.md §3 | `golden/capture/` (22: pg arms), `golden/capture-zod/` (batch validation) | same rows written for the same hook input; same validation errors |
| B4 | Postgres schema and data (`rh_*` tables, migrations 0001–0020 including `0020_tag_removals`, then 0021 renames `ros_*` to `rh_*` with `ros_*` views over them, through 0028 `rh_session_startup` (note 24), 0029 `rh_session_startup.delivered_via` (note 25) and 0040 `rh_tasks_archive` and the new `rh_tasks` (note 74), and 0052 the `graphile_worker` queue schema when it is absent (note 105), views, triggers, graphile_worker jobs). Migrations are applied only by `rivethub-migrate postgres`, which then requires every migration file name in `_rivetos_migrations`; the runtime verifies one schema version (`memory_postgres::SCHEMA_VERSION`) at connect — equal ok, newer ok with one warning, no `_sqlx_migrations` table a `NotAdopted` error (one `rivethub-migrate postgres` adopts it, including a database whose history is only `_rivetos_migrations`), behind a `SchemaBehind` error naming the migrate command — and runs no DDL | the live datahub, the TypeScript workers during cutover, operators | data-layer.md §1–§2 | `golden/capture/pg/` (30 scenarios), `golden/memory/` | Rust migrates a 0001-era DB to the same schema; writes rows the TypeScript reads and vice versa; enqueues the same jobs with the same keys |
| B5 | SQLite memory file (`memory.sqlite`, schema v7 and its upgrade ladder through cutover; schema v8 adds a sqlite-vec index afterwards). Creation and upgrades run only through `rivethub-migrate sqlite`, which copies the original file to `<file>.v<N>.bak` before any read-write connection and returns `BackupExists` when that path is already there; the runtime reads `user_version` on a read-only connection (0 → `NotInitialised`, below 7 → `SchemaBehind` naming the file, above 7 → `UnsupportedSchema`), a refused open leaves the file unchanged, and it never creates the file | standalone nodes, TypeScript nodes during cutover | data-layer.md §3; sqlite plugin | `golden/memory-sqlite/` (20 files incl. fixture DBs, upgrades, search results) | same `sqlite_master`, same rows, same search ranks; files open both ways |
| B6 | Memory read API: search / browse / stats / get_full / tags (HTTP and MCP); the per-session and per-conversation summary DAG (note 61) and the gzip memory export (HTTP) | RivetHub clients, MCP clients (Claude Code etc.), agents | data-layer.md §3.7–§3.11; gateway-api.md §11 | `golden/memory-sqlite/` search cases; gateway recordings | same results and ranking on the same data |
| B7 | MCP sidecar: RivetHub's own tools only, exactly nine (`memory_search`, `memory_browse`, `memory_get_full`, `memory_stats`, `memory_tags`, `wiki_search`, `wiki_read`, `delegate_task`, `list_agents`); `echo`, `skill_list`, `skill_manage`, `internet_search` and `web_fetch` are removed (Phil, 2026-10-13). Their schemas, transports (stdio, streamable HTTP, unix socket) | Claude Code, codex, every MCP client | mcp-tools-providers.md §2–§3 | `golden/mcp/` (`tools-list.json`, `calls.json`) | `tools/list` for the nine equals the recording after the RivetOS→RivetHub name translation, compared as parsed values; recorded calls give the same results |
| B8 | Provider wire traffic: the OpenAI-compatible chat (streamed) and embeddings API as spoken to vLLM, SGLang and hosted OpenAI-compatible endpoints through one client (no Ollama, no llama-server type) | the model servers | mcp-tools-providers.md §5.4–§5.5 | the vllm scenarios in `golden/providers/` (request+response) | byte-equal request bodies (headers filtered) except the intentional wire deltas of one typed table (testkit `B8_OMITTED_REQUEST_FIELDS`): the runtime sends NO sampling fields where the TypeScript sent hard-coded `temperature: 0.7`/`top_p: 0.95`, never sends `tool_choice` (the server's own `auto` default applies when `tools` are present), and sends `max_tokens` only when the provider config sets it (the TypeScript's 4096 default is gone); same parsed responses and stream chunks |
| B9 | CLI: command tree, flags, output text, exit codes, prompts (`rh agent` is gone, note 74) | operators, systemd units, scripts, `rh update --mesh` | cli.md §0–§19 | `tests` listed in cli.md §21; (recording to add: CLI golden transcripts) | same stdout/stderr/exit for the same argv on the same state |
| B10 | `config.yaml` keys, defaults, validation messages; environment variables (`tasks.pricing`, `tasks.eval` and `runtime.heartbeats` are retired, note 74) | operators, deploy tooling | config-and-boot.md §1–§2 | `golden/diff/config/` (realistic cases) | same accept/reject and same messages for the same file |
| B11 | On-disk state: `~/.rivethub/` layout, pid and lock files, `mesh.json`, agent presets, device/pairing files, uploads, layouts, wiki pages (markdown + frontmatter), skills `_meta.json`, `codex-threads.json`. `mesh.json` node metadata stores `denUrl` as a URL and `denPort` as a `u16`. `parse_cutover_gateway_port` is the one cutover reader for a TypeScript node that wrote `denPort` as a decimal string; every other port type is a parse error. Rust writes `denPort` as a number | other nodes, backups, humans, TypeScript during cutover | config-and-boot.md §3.9; core-domain.md §1.11; gateway-api.md §8 | fixtures under `golden/` and the inventories' documented formats | files written by Rust are read by TypeScript and vice versa |
| B12 | Harness protocol: ACP (`session/*`) natively or through the codex-acp and claude-code-acp adapter binaries; herdr control; terminal WS framing | the harness binaries, herdr, RivetHub clients terminal | harness-control-plane.md §1–§2; standardize/SESSION-MODEL.md | `golden/term/` (PTY, WS, roster) ; ACP conformance via the `agent-client-protocol` crate's schema | real binaries: new/resume/prompt/cancel work on every installed harness; term replays pass |
| B13 | Workers: graphile job names, payloads, keys, priorities (sweeps ahead of work, recent conversations ahead of old backlog, note 78), cron entries (tasks use only `run-task:<node>`, note 74); worker role split (the summarizer also claims `sync-topic-page` and runs its hourly `topic-page-sweep`, note 22; `extract-wiki` writes a placed session's summary into its topic page and files people, hosts and tools under category topics, note 23) | the shared Postgres queue, other nodes' workers | data-layer.md §2.3–§2.6 | `golden/capture/pg/` job rows | same jobs enqueued; same job handled with the same result rows |
| B14 | Mesh registry, re-registration, heartbeat and agent channel; `/ca/v1/sign`, `/ca/v1/chain`, `/ca/v1/crl`; SPIFFE URI SAN identity, holder-generated P-256 keys, 7-day node/agent and 30-day device leaves, one-time enrollment through step-ca, verified-principal renewal by minting a fresh token, step-ca's X.509 CRL; the existing intermediate is bootstrapped into step-ca outside this runtime | peer nodes, holders, operators and legacy CRL readers during cutover | PKI-DESIGN addenda 2026-10-15/15b; mesh inventory unchanged | `crates/testkit/tests/fixtures/stepca/` and `crates/pki/tests` | recorded sign refusals, step-ca chain and CRL through the port, shared `rh ca token` minting, `CaError::Unavailable` when step-ca is unreachable; holder mTLS renew/revoke and local SAN-policy cases listed with reasons |
| B15 | Hook events (names and context fields) seen by skills and the safety fence / audit log | skills, the audit log, operators | core-domain.md §1.6 | existing TypeScript test fixtures | the same hook sequence and fields for the same turn (the workflow journal belongs to the `rivethub-workflows` package, not the runtime) |
| B16 | Logs operators read, health endpoint, `doctor` output | operators, monitoring | config-and-boot.md §3.10–§3.11; cli.md §2.5 | documented lines | same lines for the documented events |
| B17 | Plug-in contract: the manifest `rivethub-plugin.json` (note 51); the plug-in process routes, its socket and environment (note 52); the plug-in API socket and its token (note 53); the permissions (note 54); the management routes `GET`/`POST /api/plugins`, `GET`/`DELETE /api/plugins/{id}`, `POST /api/plugins/{id}/enable`, `/disable` and `/reload`, `GET /api/plugins/{id}/logs`, `GET`/`PUT /api/plugins/{id}/settings` and `POST /api/plugins/{id}/jobs/{job}/run`, and the proxy `/api/plugins/{id}/{views,actions,canvas,tools,panel,http}/…` (note 55); event delivery and jobs (note 56); plug-in tools in `rh mcp` and the node-local tools socket `<state>/plugins/tools.sock` (note 57); views, canvases and web panels (notes 58–60, 91); the plug-in creator's tools, its templates `<state>/plugin-templates/` and its workshop `<state>/workshop/` (note 71); the `plugin.changed` and `plugin.views` notices on `/api/events/ws` and `plugin.notification` on `/api/notifications/ws` | plug-in processes and their TypeScript and Rust SDKs, RivetHub desktop / Android / web, `rh mcp` | notes 50–60, 70, 91 | none: new, with no TypeScript recording; bodies are the `wire::plugin` types in `contract/*.schema.json`, and `crates/wire/tests/fixtures/plugin/` holds a workflows and a file-browser plug-in's bodies | every fixture body parses as its `wire::plugin` type and writes back unchanged; the host's route behaviour is pinned by the gateway's tests against a fixture plug-in process |

## 2. What is explicitly NOT part of the contract

- Internal module structure, class names, file layout of the TypeScript.
- JavaScript runtime behaviour on inputs RivetHub never produces: `Date.parse` legacy forms, NaN/-0, prototype keys,
  implicit coercions, UTF-16 slicing except where a client receives the bytes.
- Key order of JSON objects, number spelling (`1` vs `1.0`), and whitespace — EXCEPT where a stored hash or dedupe key depends
  on them (today: capture event ids, which hash the `JSON.stringify` of tool arguments; keep that one function).
- Hardening, redaction, validation, timeouts and locking the TypeScript does not have. Weaknesses the TypeScript shares are
  logged in the bugs table and fixed after cutover in both.
- Reviewer opinions about internals. A finding against the Rust is valid only if it names a boundary row above and shows the
  observable difference, or shows a defect the Rust itself introduces (crash, hang, data loss, wrong result), or a departure
  from the Rust standards.

## 3. Owner decisions already made (apply, do not re-litigate)

ACP-shaped session model with chat over ACP (see decision 7 for the final per-harness shape; the earlier app-server /
stream-json / headless drivers are superseded) and terminal as a PTY view of the same
session (SESSION-MODEL.md); one multi-thread runtime; no comments anywhere; no den/room naming; standard crates; literal
file-edit replace; delete custom auto-actions; drop the AI-SDK user-agent; one store-timestamp parser; PGlite migration as a
one-shot `pg_dump` at cutover; `en-US` default locale; regex/YAML/JSON engine diagnostic text may differ after the TypeScript
prefix; lone surrogates become U+FFFD except on wire-exact paths.

Decided 2026-10-12 (the design's six questions): (1) switching chat↔terminal is a turn-end handoff with one owner per
session and queued messages, never injection; (2) herdr is the only terminal multiplexer (tmux support is dropped — B12
narrows to herdr control + PTY); (3) CLI-backed model providers stay, as a thin ModelProvider over a headless harness
session; (4) one process per service (`rh gateway`, `rh agent`, `rh worker --role`, `rh mcp`), with
`rh start/stop/status` keeping their CLI contract by supervising those processes; inter-process communication uses the
existing boundaries (B1 HTTP API, B4/B13 Postgres queue) — no new wire; (5) one reviewer, Claude Opus, for Rust idiom and
quality; boundary acceptance is mechanical; (6) the first attempt's branches and PRs are deleted.

Decided 2026-10-19 (after cutover): memory search ranks recency by logarithmic decay from creation,
`1 / (1 + ln(1 + days / 30))` with the same access reinforcement, instead of the 14-day exponential half-life, so B6
no longer ranks like the TypeScript. The `memory-sqlite/search.json` hybrid recording carries the Rust scores, the
older message the fusion gate now keeps, and the access counts that follow from it.

Decided 2026-10-12 (crate review): (7) one harness adapter — every chat-capable harness speaks ACP, natively or through
`codex-acp` / `claude-code-acp`; pi and cursor are terminal-only until they have ACP; no codex app-server, claude stream-json
or headless drivers are built. (8) A crate exists only where a feature is replaced or deployed on its own: hooks live in
agent-loop, skills in mcp, agent presets in gateway, the wiki builder in workers, the certificate authority commands in cli (signing and policy move to the `pki` adapter under the approved 2026-10-14 design), the
harness store readers in capture (behind the `TranscriptReader` port, wired to the gateway's transcript route by the binary).
(9) Standalone runs on SQLite: no embedded Postgres and no local-db crate; existing PGlite installs migrate once at cutover
through the SQLite store's Postgres-shaped import; `rh local` drops `--pg-port` and `--db` (B9 narrows accordingly);
sqlite-vec arrives as schema v8 after cutover while v7 brute-force search holds parity through the transition.
(10) No built-in tool set: the `shell`, `file_*`, `search_*`, `ask_user`, `todo`, `internet_search` and `web_fetch` tools are
deleted (memory shows no RivetHub use of the first five; every harness ships its own shell, file, search and web tools, so the web
tools do not belong in the sidecar either). B7 narrows to the sidecar's RivetHub-owned tools; the `RIVETHUB_MCP_ENABLE_{SHELL,
FILE,SEARCH}` gates and the `plugins/tools/*` plugin entries are accepted and ignored by `config` with a deprecation warning.
(11) One model client: `providers` is a single OpenAI-compatible implementation
(chat with streamed text/reasoning/tool-call deltas, usage, cancellation;
embeddings) plus the cli-backed provider over a headless ACP session. Provider
types are `openai-compatible`, `vllm`, `claude-cli`, `codex-cli`, `grok-cli`,
`hermes-cli`, `opencode-cli` and `kimi-code`; the removed types `ollama`,
`llama-server`, `openai-compat`, `cli`, `pi-cli`, `qwen-code`, `anthropic`,
`google`, `gemini` and `xai` are a config error at `providers.<id>.type` naming
the alternative (a harness, or `openai-compatible` with the server's base URL;
`qwen-code` remains a harness id). Provider auth is the static `api_key` or,
when `token_command` is set, a bearer minted by that argv: stdout has one
trailing newline removed and is cached until `token_ttl_ms` (default 300000)
elapses, the command is killed when `token_command_timeout_ms` (default 5000)
elapses, and `token_command` wins over `api_key`. A minted bearer that is a JWT
whose payload carries an integer `exp` is instead cached until 30 s before that
`exp`, and not cached when that point has passed. Chat-client embeddings use
that same key or token command. `embed_api_key` remains the memory embedder's
static key, and `embed_token_command`, `embed_token_ttl_ms` and
`embed_token_command_timeout_ms` are removed. `memory.postgres.embed_provider`
and `memory.sqlite.embed_provider` name an `openai-compatible` or `vllm`
provider whose `base_url` and `api_key` or `token_command` the memory embedder
then uses with `embed_model`; setting it together with `embed_endpoint` or
`embed_api_key`, or naming an unknown or non-HTTP provider, is a config error
at that key. Sampling and model-server tuning are configured on the model
server, not in RivetHub: the tuning keys `temperature`, `top_p`, `top_k`,
`min_p`, `presence_penalty`, `frequency_penalty`, `seed`, `stop`,
`repetition_penalty`, `min_tokens`, `default_tool_choice`, `mm_processor_kwargs`,
`chat_template_kwargs`, `extra_body`, `models`, `models_ttl_ms`, `models_url`,
`verify_model_on_init`, `num_ctx`, `keep_alive` and `embed_wire_shape` are
removed, a config that still sets one warns
(`<key> is set on the model server, not in RivetHub`), and the runtime sends
no sampling fields, never sends `tool_choice`, and carries no hard-coded
`max_tokens` default on the wire: `max_tokens` goes out only when the
provider config sets it. A cli-backed provider whose harness fails ACP
discovery (bounded by `discovery_timeout_ms`, default 30000; `timeout_ms`
bounds turns only) is degraded, never fatal: the gateway and every other
provider start, a turn on it fails fast naming the provider and the cause,
timeouts and launch I/O retry with exponential backoff (1 s doubling to
60 s) until discovery succeeds, a missing or unverified harness or a
protocol refusal does not retry, and `/healthz` lists it under
`degradedProviders` (`id`, `recovery`, `reason`), which `rh status`
prints. The agent loop stays, minimal, for Discord, the
task engine's API agents and `/v1`.
(12) Chat-channel bots (Discord, Telegram, voice) live in `rivethub-channels`; `channels.discord/telegram/voice` config is
accepted and ignored with a warning. (13) Workflows kept but outside the runtime: the separate `rivethub-workflows` workspace package
(own binary, depends on `wire` + HTTP client only) drives sessions and tasks over the public API; not linked into `rivethub`.
(14) Clients stay in the monorepo under `apps/`; the gateway serves the RivetHub web build from `static_dir`.
(15) Capture redaction is a configuration option, off by default: `memory.capture.redaction.{enabled,builtins,patterns}` and
`RIVETHUB_CAPTURE_REDACTION` are honoured by the hook-side writer; the YAML keys are actually passed to the hook process (the
TypeScript validated them but did not). Nothing is redacted unless enabled.
(16) Memory search (B6, B7) narrows message hits by `role` and `tool_name` on `GET /api/memory/search`, the
`memory_search` tool route and the MCP `memory_search` schema; summaries carry neither and stay in the result unless
`scope` excludes them (Phil, 2026-10-19).
(17) Topics (Phil, 2026-10-25): a topic is a themed group of sessions with a stable UUID, a unique slug folded from
its name, aliases unique across topics (case-insensitive), an optional parent one level deep and an optional directory
(`local` on a node or `shared`). A session has at most one placement, made by `you` or the `categorizer`; the
categorizer never replaces your placement, and unplacing returns the session to Forming. Merging A into B moves A's
placements, sub-topics and aliases to B and keeps A's slug resolving to B. The B1 Topics routes are new, with no
TypeScript recording; `{id}` takes a topic id or slug and `{session}` is the base64url session segment. Their
bodies are the `wire::gateway::topics` types in `contract/*.schema.json`, and `lastActive` is the newest captured
conversation update of the topic's sessions.
(18) Session categorizer (Phil, 2026-10-26; superseded by (42) on 2026-10-29): when a session's first leaf summary is stored, the summarizer skips a
session that is already placed or is a child of another session (it inherits its parent's topic on the client), then
looks for the topics named in the conversation title and the leaf summary (slug, aliases and merge redirects, matched
as whole words of three or more characters, a sub-topic winning over its parent). Exactly one named topic places the
session `by: "categorizer"`; otherwise the session stays in Forming with a suggestion: the title as the suggested
name and, when exactly one topic is named in the title, that topic as `topicId`. No model call is made. `GET
/api/sessions/{session}/placement` carries the suggestion as an optional `suggestion` (`name`, optional `topicId`)
while the session is unplaced; any placement clears it, and merges move its `topicId` like placements.
(19) Tool output (Phil, 2026-10-26): harness transcripts keep tool rows small and carry no tool output except for prompt
tools; a client fetches one call's output on demand from `GET /api/harness-sessions/{session}/tools/{tool}/output`, where
`{session}` is the base64url segment of a canonical session id and `{tool}` is the tool call id. The body is
`wire::gateway::sessions::HarnessToolOutput` in `contract/*.schema.json`: the tool result's text read from the harness's own
session file, capped at 64 KiB by keeping its tail, with `truncated` set when capped. An unknown tool call id is `404`;
Claude Code is the only harness with a tool output source, the others answer `501` `capability-unsupported`. The route is
new, with no TypeScript recording.
(20) Hub state (2026-10-21 call with Phil): RivetHub web / Android / Electron share one document per user — projects,
session names, archive, pins, hidden, chat/terminal modes and the last open session. `GET /api/hub/state` answers
`{version, state}` (version 0 and an empty state before the first save); `PUT` takes `{baseVersion, state}` and saves
only when `baseVersion` is the current version, else answers `409` with the current document. The stored document is
normalised (oversized lists refused with `400`, malformed entries and references to unknown projects dropped), and each
save sends `{type: "hub.state", v: 1, version}` to that user's unfiltered `/api/events/ws` sockets. The document lives on
the memory database (`rh_hub_state`, migration 0026) so every node on one datahub serves the same copy; a node without
Postgres keeps it under `<state>/hub-state/`. Bodies are the `wire::gateway::hub_state` types in
`contract/*.schema.json`. The route matches the TypeScript den route on the unmerged rivetOS `feat/rivethub-hub-state`
branch, with no recording.
(21) Topic pages (Phil, 2026-10-27): a root topic owns at most one wiki page (`rh_wiki_topics.topic_id`, migration 0027,
bound by exact slug, alias or redirect match; SQLite schema v13 keeps the same binding in `rh_topic_pages` so the legacy
`ros_wiki_topics` shape stays as recorded). `GET /api/topics/{id}/page` returns the topic and its page (a sub-topic
answers with its root's page). `/api/wiki` page bodies gain optional `topicId`. The route is new, with no TypeScript
recording.
(22) Topic page sync (Phil, 2026-10-27): every root topic owns a page; create, rename, reparent and merge keep it in step
via the job `sync-topic-page` (`{topicId?, fold?}`), enqueued in the topic store's own transaction on Postgres (only when
the graphile queue is installed) and SQLite, and claimed by the summarizer role. A new root topic gets an empty page whose
slug is the topic slug, or the slug with `-2`, `-3`... when taken; a rename keeps the page slug, retitles the page and
redirects the new topic slug to it; a merge folds the source page into the target page with the wiki fold and redirects
its slug. Sub-topics are `## <name>` sections of the root's page, recorded in frontmatter `sections: [{topic, heading}]`
(SQLite keeps them in `rh_topic_pages.sections`, schema v14) in sub-topic order; a sub-topic that becomes a root takes its
section's text to its own page, and a root that becomes a sub-topic folds its page into the parent's as its section. A sync
is idempotent. Existing topics get their pages from the summarizer's hourly `topic-page-sweep` (an empty payload syncs
every topic), queued once at start by `rh local`. When a root topic becomes a sub-topic, its page folds into the
parent's section without the `#### Merged from <slug>` line that a merge or consolidation fold still writes.
(23) Wiki builder topic routing (Phil, 2026-10-27): `extract-wiki` looks up the summary's session placement
(`rh_session_placements`; a child session with no placement of its own uses its parent's, by the categorizer's lineage
rule) and, when there is one, gives the model the topic as `topic: {pageSlug, name, section?}` beside the summary. A
patch for that page slug lands on the topic's bound page (resolve reason `topic`, title kept as the topic name); for a
sub-topic it lands in the sub-topic's section of the root page instead (merged, not appended, since (100)). The alias, identity, entity, stem and
search resolve steps never land a patch on a page bound to another topic: they fall through, and a patch with no other
match creates an unbound page. The bound page always gets a citation row for the summary, even when the model proposes
nothing. Patches carry an optional `kind` (`person`, `host`, `tool`): a kind patch for a known topic (by title, slug
or alias) goes to that topic's page or section; otherwise the builder ensures the root topic `People`, `Hosts` or
`Tools` and a sub-topic named after the patch title under it, and the text lands in that sub-topic's section of the
category page. Patches without a kind, and summaries of unplaced sessions, resolve as before. Existing unbound pages are
not converted. Consolidation never folds a page bound to a topic into another page; a bound page survives its group.
(24) Startup memory (Phil, 2026-10-27): create takes `topicId` (placed by you at create) and `prompt`; rh assembles a
≤4000-token startup context (topic page, recent sessions in the topic, relevant hits) returned as `startupContext` and kept
at `GET …/startup-context`. Delivery into harnesses is note 25. `POST /api/harness-sessions` resolves `topicId` (an
id or slug, following merge redirects) before anything is spawned, so an unknown topic is `404` with no session; the new
session is then placed `by: "you"`, and a failed placement removes it again and answers the error. The context has
`topicId`, `tokens` (the estimate the memory context uses, four characters a token) and `sections` in the order `topic`
(the bound page's summary and article start, and for a sub-topic its own section first; about 1600 tokens), `recent` (up
to four of the topic's sessions by last activity with a latest leaf summary, each with its title; about 1000) and
`relevant` (a hybrid memory search for `prompt`, hits from the topic's sessions first; about 1400, only with a prompt).
Unused budget rolls forward to the next section; an empty part has no section, so a topic with nothing to say answers
`sections: []`. Each section has `kind`, `title`, `text` and `sources` (`{session}` ids or `{wiki}` page slugs; a hit whose session key is not a canonical session id names no session). A
context that cannot be built is logged and the create answers without it. The context is kept per session
(`rh_session_startup`, migration 0028; SQLite schema v15). `{session}` is the base64url session segment; a session
without a kept context is `404`. The bodies are `wire::harness::HarnessSessionCreateRequest` and
`wire::gateway::topics::StartupContext` in `contract/*.schema.json`. The routes are new, with no TypeScript recording.
(25) Startup memory delivery (Phil, 2026-10-27): the startup context reaches the agent as an appended system prompt,
never as a first-turn message. It is rendered as one plain-text block, `## Memory for this session (topic: <name>)`, with
one `### <title>` per section (`Topic page`, then each section's own title); a context with no sections delivers nothing.
An ACP chat sends it on `session/new` in the `_meta` field the harness's adapter reads: claude-code
`_meta.systemPrompt.append` (claude-agent-acp), grok-build `_meta.rules`, codex `_meta.jetbrains.air.customInstructions`
(codex-acp passes it as the thread's developer instructions). opencode, hermes, kimi-code and qwen-code read no such
field and get nothing. A terminal spawn (`POST /term`, `POST /api/terminal`) takes optional `topicId`, resolved like create (an
unknown topic is `404` before anything is spawned), places the session `by: "you"` and answers with
`harnessSessionId`; on a fresh spawn only, never on a resume, the CLI gets claude-code, qwen-code and pi
`--append-system-prompt <text>`, grok-build `--rules <text>`, codex `-c developer_instructions=<TOML string>`; hermes,
opencode, kimi-code and cursor have no append option and get nothing. The kept context gains `deliveredVia`
(`metaAppend`, `cliFlag` or `none`), set when the session starts and served by `GET …/startup-context`; a terminal
resume keeps no context. Kept on `rh_session_startup.delivered_via` (migration 0029, `none` for earlier rows; SQLite
schema v16). The text is never recorded as a user message.
(26) Node version: `/healthz` answers with optional `version` (the rh package version) and `revision` (the short git sha,
present when the build had one), and each `/mesh.json` node carries the `version` and `revision` its own `/healthz`
reported, absent while it is offline. The recorded healthz bodies match them with the `{{VERSION}}` and `{{REVISION}}`
tokens. `rh status` prints the version.
(27) Topic decisions (Phil, 2026-10-27): a topic owns decisions, each with `text`, `status` (`active` or `superseded`),
`by` (the calling user), `createdAt`, `updatedAt`, optional `sessionId` (the session it came from) and, once superseded,
optional `supersededBy`. `GET /api/topics/{id}/decisions` lists the topic's own decisions oldest first; `POST` takes
`{text, sessionId?}` and answers `201` with the decision; `PATCH …/decisions/{decision}` takes `{text}`; `POST
…/decisions/{decision}/supersede` takes `{text?, sessionId?}`, marks the decision superseded and, with `text`, adds its
replacement, answering `{superseded, replacement?}`. Empty text is `400`, editing or superseding a superseded decision is
`409`, and a decision of another topic is `404`. `{id}` follows merge redirects, and a merge moves the source topic's
decisions to the target. Every write queues `sync-topic-page`, which renders the root page's `## Decisions` section (first,
before the sub-topic sections): the root's decisions as `- <text> (<by>, <date>)` lines, a superseded one struck through
and marked `superseded`, then each sub-topic's under `### <name>`. The section is rewritten on every sync, so edits go
through these routes. Kept in `rh_topic_decisions` (migration 0030; SQLite schema v17). Bodies are the
`wire::gateway::topics` decision types in `contract/*.schema.json`. The routes are new, with no TypeScript recording.
(28) Memory search hits (Phil, 2026-10-27): summary hits on `GET /api/memory/search` carry `sessionId`, the session of
the summarised conversation, as message hits already did. The route takes optional `exclude_roles`, a comma-separated
list of roles (`exclude_roles=tool,system`) whose message hits are left out; summaries are unaffected. NUL characters
in the search text and filters are dropped before the query runs, and legacy rows whose `tool_args` is a JSON string
are read as their decoded object when the string holds one, otherwise without arguments.
(29) Topic delete (Phil, 2026-10-28): `DELETE /api/topics/{id}` (an id or slug, following merge redirects) answers `204`
with no body. The topic's placed sessions are unplaced back to Forming, suggestions, kept startup contexts and decisions that name
it are removed, and its aliases and slug redirects go with it. Its bound page stays with its content and becomes a page
without a topic (`rh_wiki_topics.topic_id` set to null; SQLite schema v18 keys `rh_topic_pages` by page slug so the
binding's `topic_id` can be cleared); a deleted sub-topic's section stays on its root's page as a plain section. A topic
with sub-topics, and the category roots `People`, `Hosts` and `Tools`, answer `409`; an unknown topic is `404`. The
route is new, with no TypeScript recording.
(30) Session summary DAG (Phil, 2026-10-28): `GET /api/memory/summaries/{session}` reads the session's one conversation,
the active row first and then the most recently updated, so an ended session (finalized, `active = false`) still answers
its DAG; it was filtered out before. Each node carries `vector` and the body gains `messages`, a tally of that
conversation's messages: `total`, `user`, `assistant`, `system` by role, `tool` (role `tool` or a tool name, as in
stats), and `embedded`, `pending`, `failed`, `skipped` by vector state. A row with an embedding is `embedded`,
`embed_status` `failed` is `failed`, `unembeddable` is `skipped`, anything else is `pending`. An unknown session answers
`200` with no summaries and a zero tally. Topic startup memory reads ended sessions the same way. Bodies are
`wire::gateway::memory::MemorySummaryDagResponse`, `MemoryMessageTally` and `VectorState` in `contract/*.schema.json`.
(31) Harness set (Phil, 2026-10-29): `HarnessId` drops `pi` and `qwen-code` and gains `omp` (oh-my-pi) and `dsh`
(DeepSeek Harness); `RosterCommand` drops `pi` and `qwen` and gains `omp` and `dsh`. omp chats natively over ACP
(`omp acp`) with launch flags `--model` and `--thinking` and models discovered from its ACP model option; the built-in
terminal command `omp` (label `omp`) resumes with `--resume <id>`, has no pin flag, and a fresh spawn gets
`--append-system-prompt <text>`. Its store is the omp agent directory (`PI_CODING_AGENT_DIR`, else `~/.omp/agent`):
`sessions/<encoded-cwd>/<stamp>_<uuid>.jsonl`, listed with the leading `title` line as the title when it is not blank,
else the first user message, and the last `model_change` as the model. dsh chats natively over ACP (`dsh acp`) and is
offered when `dsh` is installed; it has no terminal command and no session store, so its harness session list holds only
the chats live in this gateway. Its transcript has no source (`capability_unsupported`), so the history socket sends no
transcript for a dsh chat and the client keeps what it streamed; a dsh chat starts in the directory of the node's default
terminal command. A `qwen-code` or `pi` entry
under `tasks.harnesses` is now an unknown harness id. Bodies are `wire::harness::HarnessId` and `RosterCommand` in
`contract/*.schema.json`.
(32) Agent plans (Phil, 2026-10-29): `HarnessEvent` gains `plan`,
`{ "type": "plan", "sessionId", "entries": [{ "content", "status" }] }` with `status` one of `pending`, `in_progress`
and `completed`. A harness session's event stream carries it, whole, each time an ACP agent replaces its plan with a
`session/update` `plan`; it is not stored, so a transcript does not replay it. Bodies are `wire::harness::HarnessEvent`,
`PlanEntry` and `PlanStatus` in `contract/*.schema.json`.
(33) Failed turns (Phil, 2026-10-29): `HarnessCode` gains `turn_failed` (HTTP 502, not retryable). When a chat turn
fails, such as a provider refusing the request, the harness session stream carries an `error` event with code
`turn_failed` and the failure as its message, then `turn-complete`, and the session goes idle and takes the next turn;
before, the failure ended the chat. Bodies are `wire::harness::HarnessCode` in `contract/*.schema.json`.
(34) Steering (Phil, 2026-10-29): `POST /api/harness-sessions/{session}/steer` takes `{ "text", "attachments"? }` (the
`/turn` text and uploads, without model, effort or system prompt) and is authorised like `/turn`. While a chat turn is
running it hands the message to that turn instead of queuing a new one and answers `200` with `{ "outcome" }`:
`steered` (the running turn took it) or `startedNewTurn` (the agent began a turn of its own with it); either way the text
is published on the session stream as the user's message, as a turn's is, and its uploads are journalled like a turn's.
With no turn running nothing is sent and it answers `200` with `{ "outcome": "promptRequired" }`: send the message with
`POST …/turn`. A harness that cannot steer answers `501` with code `capability_unsupported`, empty text without
attachments is `400`, and a steer the agent refuses or fails is `503`. Over ACP the gateway sends the agent's
`_session/steering` request (`{ sessionId, prompt, _meta: { steering: { idleBehavior: "promptRequired" } } }`, the
prompt as `session/prompt` content blocks) and reads its `outcome`: `injected` is `steered`, `startedNewTurn` and
`promptRequired` pass through, and omp's `failed` is the `503` with its `reason`. A harness offers steering when its
ACP `initialize` reply carries `_meta.steering.supported: true` (claude-code through claude-agent-acp, and omp):
`HarnessControlCapabilities` gains optional `steering`, `true` for such a harness once discovered, `false` for one
whose agent does not advertise it, and absent where no agent has answered. Bodies are
`wire::gateway::sessions::SessionSteerRequest` and `SessionSteerResponse` and `wire::harness::SteerOutcome` in
`contract/*.schema.json`. The route is new, with no TypeScript recording.
(35) Unstorable text (Phil, 2026-10-29): a memory write that Postgres refuses for its characters (SQLSTATE `22021`
invalid byte sequence, or `22P05` untranslatable character, such as a NUL inside a capture's `tool_args` or
`metadata`) answers `400` with the database's message instead of `500`, so a capture client sets the batch aside as
a bad batch rather than retrying it ahead of every later one. The write still rolls back whole, as the recorded
TypeScript arms expect.
(36) Done state (Phil, 2026-10-29): a topic can be closed and a session marked as history. `TopicRecord` gains
optional `closedAt` (epoch milliseconds, like its other times; absent while the topic is open) wherever it appears:
list, show, search, create and merge replies. `POST /api/topics/{topic}/close` sets the closed time to now (closing a
closed topic moves it to now) and `POST /api/topics/{topic}/reopen` clears it; both answer the `TopicRecord`, take any
topic, category topics included, and answer `404` for an unknown one. Both set the topic's `updatedAt` to now, so a
client that orders topics by their latest change brings a reopened one back to the front. `GET /api/sessions/history` answers
`{ "sessions": [{ "sessionId", "markedAt" }] }`, every marked session, newest mark first.
`PUT /api/sessions/{session}/history` marks the session at now and answers `{ "sessionId", "markedAt" }`;
`DELETE /api/sessions/{session}/history` removes the mark, is idempotent and answers `204`. The session segment is the
canonical id `/api/sessions/{session}/placement` takes, validated the same way. A mark is independent of placement: an
unplaced session can be marked, and placing or unplacing keeps it. rh never clears either by itself: a client shows a
closed topic as open again once one of its sessions is active after `closedAt`, and a marked session as active again
once it is active after `markedAt`. Postgres migration `0031_done_state.sql` adds `rh_topics.closed_at` and
`rh_session_history`; SQLite schema 19 does the same. Bodies are `wire::gateway::topics::SessionHistoryMark` and
`SessionHistoryList` in `contract/*.schema.json`. The routes are new, with no TypeScript recording.
(37) Deeper topics (Phil, 2026-10-29): topics nest up to 4 levels (a root is level 1), replacing the one-level rule of
B1. Setting a parent, at create or update, is refused when the parent's level plus the height of the moved topic's subtree
would pass 4 (`409`, "Topics nest at most 4 levels deep") and when the parent is the topic itself (`400`, "a topic cannot be
its own parent") or anything under it (`409`, "A topic can't move under its own subtopic"). Merging A into B moves A's
subtopics under B, so it is refused when that would pass 4 levels (`409`, the same message) and when B sits below a
subtopic of A (`409`, the same subtree message); merging A into its own direct subtopic still lifts that subtopic into A's
place. A root topic's wiki page keeps a section for every topic under it at any depth, and startup memory, page binding
and routing find a topic's root by walking its parents. Deleting a topic that has subtopics is still refused. The
categorizer drops a named topic when one of its descendants is also named, not only its direct subtopic.
(38) Session previews (Phil, 2026-10-29): `GET /api/harness-sessions/previews?session=<canonical id>&session=…` (at
most 50, else `400`; a non-canonical id is `400` "canonical session required") answers `{ "previews": [ { "sessionId",
"kind", "text", "at" } ] }`, one entry per session with something to show, in no promised order; sessions the caller
can't see, or that have nothing to show, are left out rather than failing the call. `kind` is `running` (the tool in
progress and the gist of its input, as "Bash: cargo clippy --workspace", taken from a later tool call update when the
harness names the command only there, as claude-agent-acp's terminal calls do; or the last line being written),
`asking` (the question an ask tool put to the person), `approval` (the tool waiting on a permission and its gist),
or `said` (the last line of the last assistant reply). `text` is one line of at most 200 characters; `at` is epoch
milliseconds. A session rh is driving shows its live state; any other session shows `said` from its transcript, read
once per update time.
(39) Topic startup preview (Phil, 2026-10-29): `GET /api/topics/{topic}/startup` answers the `StartupContext` a session
started in that topic right now would receive, built by the same code as a real start, without keeping or delivering
it. An unknown topic is `404`; startup memory not configured is `503`.
(40) Topic kinds (Phil, 2026-10-29): `TopicRecord` gains `kind`: `work` (the workspace's topics, and the default),
`type` (an entity type) or `entity` (a person, agent, host, piece of hardware, model, service, software, repo, domain or
database the wiki knows). `GET /api/topics` answers work topics; `?kind=type|entity|all` answers the others, and
`/api/topics/search` takes the same `kind`. `POST /api/topics` takes an optional `kind`, `work` when absent. Work topics nest
only under work topics and types only under types, both within 4 levels; an entity's `parent` is required and must be a
type, and nothing sits under an entity. A wrong parent is `409` ("Work topics nest only under work topics", "Types nest only
under types", "An entity sits only under a type"); an entity without a parent is `400` ("An entity needs a type"). Merging
joins topics of one kind only (`409`, "Only topics of the same kind merge"). Sessions are filed only under work topics:
placing one under a type or an entity is `409`. Slugs and aliases are unique within a kind, so a work topic and an entity can
share a name; a slug in a topic path names a work topic, so types and entities are addressed by id. Any topic can be
deleted, the former People, Hosts and Tools category topics included, and deleting leaves a tombstone so the wiki builder
never recreates that name. The wiki builder writes only to topic and entity pages: a patch about the session's topic lands
on its page as before, a patch whose `type` names one of the types goes to that entity's own page (a type has no page), and
any other patch becomes a note section on the session's topic page. A `type` with several names (`Software › Apps`)
names the type at that path from the root; a single name names the one type with that name, alias or former name, and a
name more than one type answers to names none, so the entity waits as a candidate. A session that isn't filed yet keeps its notes until it
is filed (a child session until the nearest session above it is, walking up at most 8 parents, so a subagent of a
subagent waits on the filed root and not on its unfiled parent; a conversation or delegated task until that conversation
is), then
they land on its topic page and cite its summary. A session's notes follow it: moved to another topic, by you or
the categorizer, they leave the old topic's page for the new one (and so does the summary's citation, unless the summary
also patched the old page itself), and taken out of a topic they wait until it is
filed again; a child follows its parent, and a delegated task its parent unless you placed it; an entity named by one session waits as a candidate until
a second session names it. A candidate is dropped only after its patch is written to the entity's page and its summary
cited there, so a session that names an existing entity also writes and cites the candidates still held for it. An entity goes by its name, never a description (QA sweep, 2026-11-01): a trailing qualifier in parentheses is dropped, and a title over four words or 40 characters, or holding a file name, a path, a hex id, a code symbol, prose punctuation or a joining word (and, on, for, of, via…), names no entity; a host's name is a hostname, a domain's a domain name and a repository's `name` or `owner/name`. A patch finds its entity by name, alias or former name, ignoring case and punctuation and preferring an entity of its own type; candidates held under any of its names count together, a new entity keeps the other valid names its candidates went by as aliases, and an existing entity learns a patch's other valid names unless another entity goes by them. `WikiPatch.kind` (person, host or tool) is replaced by `type`, and the extraction input lists
the type paths as `types`. Postgres migration `0032_entities.sql` and SQLite schema 20 add the kind, the tombstones and the
candidate and deferred tables; they make People and Hosts types, rename Tools to Software, turn the topics under them into
entities (dropping the filings that pointed at them and unbinding the type pages), and seed the types Agents, Hardware,
Models, Services, Repos, Domains and Databases, with Software's CLIs and tools, Apps, Libraries and frameworks, OS and
platforms, and Plugins and skills.
(41) Wiki edits (Phil, 2026-10-29; the route moved by (63)): `PUT /api/wiki/{slug}` with `{ "currentState"?, "article"? }`
(`wire::gateway::wiki::WikiPageEdit`) replaces what it names on a topic or entity page, commits the page to the wiki
repository as "Edited in Memory" with a `user` source and a dated history entry, and answers the page as
`GET /api/wiki/{slug}` does. A slug that isn't a topic or entity page is `404`; wiki pages not configured is `503`. A page's
title, an entity's name and its type change only through the topic routes, and the builder never renames a topic or an
entity or moves an entity, so what a person sets stays.
(42) Filing (Phil, 2026-10-29): a conversation's first leaf summary enqueues `file-conversation`
(`{conversationId}`), which only the categorizer role runs, on the model of its config's default agent. The tag
suggester (`suggest-tags`) and the folder rule that wrote `project:` tags are retired; tag storage and the tag routes
stay. What is filed: a key that is a session id files that session; a bare uuid on the `claude-code` channel files
`claude-code:<uuid>`; `task:<uuid>` files the conversation under the topic of the conversation that started the task:
the earliest whose tool output names the task id, else the earliest whose messages name it, however long before or after
the task conversation was recorded (as a conversation when none is found or it is unfiled); any other key files as a
conversation. Never filed:
keys starting `grokbot-run-sand-subagent-` or `heartbeat`, and the channels `test-channel`, `env-test-channel` and
`test`. Order: an existing placement is kept; a child session (one whose native id holds `/subagents/`, the parent
being the part before it, or one whose harness store names a parent; the wiki and the cleanup use the same test)
follows its parent on the client and is not filed; a
task follows its parent; a working directory inside a topic's own directory (deepest wins, a tie decides nothing; a
local directory matches by path, since conversations captured before (64) record no node) files it there; exactly one
topic named in the title or first leaf files it there; otherwise the model answers file, suggest a name (with an
optional parent) or nothing. A suggestion that matches a topic's slug or alias files there; one that repeats the title
or reads like a sentence, path, file, ticket, PR, version, branch or id, or names a deleted topic, is refused; the rest is kept for sessions as the
placement `suggestion` with an optional `parentId` (the parent a new topic would go under; `topicId` keeps its meaning).
Conversations keep no suggestion. Filings are `by: "categorizer"` and never replace yours. `GET
/api/conversations/{id}/placement`, `PUT` (`{topicId}`) and `DELETE` read, set and clear a conversation's placement.
When you move or clear something the categorizer filed, the change is kept as a correction and the newest eight are
shown to the model as examples. `GET /api/topics/{topic}/sessions` answers `sessions` (each with its newest `title` and
`lastActive` across the session's conversation keys, so rows need no harness list) and `conversations`
(`conversationId`, `topicId`, `by`, `at`, `channel`, optional `title` and `lastActive`). Merges move conversation
placements and suggestion parents; deleting a topic drops its conversation placements and the corrections that name it.
(43) Tidy proposals (Phil, 2026-10-29): changes to topics, entities and types that someone has to approve wait as proposals.
A change (`wire::gateway::tidy::TidyChange`, tagged by `kind`) is `merge` (`source` into `into`, two topics of one kind),
`move` (`topic` under `parent`, top level when absent), `rename` (`topic` to `name`), `close`, `delete`, `createTopic`
(the new work topic's chosen `topic` id, `name`, optional `parent`, and the `sessions` and `conversations` to file under
it) or `addType` (the new type's `topic` id, `name`, optional parent type). A proposal (`TidyProposal`) carries `id`,
`batchId`, `change`, a `reason`, `evidence` (`{ sessions, samples: [{ id, title? }] }`), `proposedBy` (`categorizer` or
`backfill`), `proposedAt`, `state` (`pending`, `approved`, `rejected`) and `decidedAt`. `GET /api/tidy/proposals`
answers `{ batches: [{ batchId, proposedBy, proposedAt, proposals }] }`, pending proposals only, newest batch first and
each batch in proposal order. `POST /api/tidy/proposals/{proposal}/approve` applies the change through the topic
operations (a merge leaves its redirect, a new topic files its members `by: "categorizer"`, so your placements stay) and
answers the proposal approved; `POST …/reject` answers it rejected. A decided proposal is `409`, an unknown one `404`, an
id that is not a uuid `400`; a change the topic rules refuse answers that topic error and stays pending. Approving again
continues an approval that stopped part way: a new topic or type that already exists under the proposal's id is not
created again and the members not yet filed are filed, and a merge whose source is gone and whose survivor holds a
redirect, or a delete whose topic is gone, is approved as already applied.
`POST /api/tidy/batches/{batch}/approve` and `…/reject` decide every pending proposal of the batch in order and answer
`{ decided, failed: [{ proposalId, error }] }`, where a failed proposal stays pending; an unknown batch is `404`. Every
change has a fingerprint (a merge's unordered pair; a move's topic and parent; a rename's topic and new slug; the topic
of a close or delete; the folded name of a new topic or type; a `fileInto`'s topic and a SHA-256 digest of its sorted
session keys and conversation ids, so filing other members is another change; a `mergePage`'s page then the page it
folds into, in that order, since the opposite fold leaves the other page), and a proposal whose fingerprint is
pending or rejected is dropped when a batch is stored, so a rejected change is never proposed again; approvals are not
remembered. The categorizer role claims `propose-tidy` (cron `tidy-sweep`, hourly; `rh local` also queues it at start), which does
nothing until a week has passed since the categorizer's last batch. It reads the work tree and the types with session
counts and the filings of the last 7 days, the clusters of pending session suggestions that share a folded name (3
sessions or more), the pairs of same-kind topics that share a topic identity key, and the newest eight corrections; asks
the model filing uses for one JSON object per line; drops lines that name nothing listed; and keeps a change only when it
passes the guard: a new topic needs at least 3 sessions, a new or renamed name passes note 42's name rule and is neither
tombstoned nor taken, merges and moves pass the topic rules, a close takes an open work topic, a delete only a topic with
no sessions and no subtopics, and nothing pending or rejected is proposed. Each run stores its batch, empty or not.
Postgres migration `0034_tidy.sql` and SQLite schema 22 add `rh_tidy_batches`, `rh_tidy_proposals` and `rh_worker_runs`
(note 44). The routes and the job are new, with no TypeScript recording.
(44) Memory upkeep (Phil, 2026-10-29): `GET /api/memory/upkeep` answers `wire::gateway::tidy::MemoryUpkeep` for the
Memory Stats tab. `filing` counts every conversation once: `categorizer` and `you` by the placement that files it (its
session's, a bare Claude uuid through its `claude-code:` session, or its own), `leftOut` for note 42's never-filed keys
and channels, `uncategorized` for the rest, so the four add up to `/api/memory/stats` `conversations`; and
`correctionsLastWeek`. `wiki` has `topicPages` (pages bound to work topics), `entityTypes` (`[{ typeId, name, entities }]`,
every type with the entities directly under it) and `loosePages` (pages bound to no topic). `workers` has `embedder`,
`summarizer`, `categorizer` and `wikiBuilder`, each `{ lastFinishedAt?, failing }`: the newest finish of a job in the
role's task list, and how many of its jobs have failed at least once without succeeding since, dead ones included.
Completing a job records its task's finish in `rh_worker_runs`, in the graphile completion statement or the SQLite
completion transaction. `pendingProposals` counts pending proposals. Without the stores the route is `503`.
(45) Topic cleanup (Phil, 2026-10-29): `rh memory cleanup --dry-run --out DIR --model-url URL --model NAME` plans the
cleanup and changes nothing; `--propose --database URL` runs the same stages against the database it names and then
writes them as one tidy batch (`proposed_by: backfill`, note 43) for Phil to approve, refusing the node's live memory
database (`RIVETHUB_PG_URL`): before any stage and again before writing it connects to both and compares the server's
cluster `system_identifier` and the database's oid, so another URL for the same database is refused too. Optional `--database` (dry run: default
`RIVETHUB_PG_URL`), `--batch N` (20), `--concurrency N` (2), `--timeout SECONDS` (900). Memory is read over connections
that set `default_transaction_read_only=on` (checked before reading), on the deployed schema and on 0032/0033 (before
0032 the People, Hosts and Tools roots count as types and the topics under them as entities); the People, Hosts and Tools
wiki pages are read without the writer lock. Child sessions come from this node's harness stores and from claude-code
`/subagents/` keys. Stages are checkpointed in DIR (`snapshot.json`, `tree-chunks.jsonl`, `tree-drafts.jsonl`,
`tree.json`, `filing-answers.jsonl`, `filings.json`, `fold-answers.jsonl`, `folds.json`, `timings.json`,
`progress.log`, `proposed.json`); a rerun resumes after the last finished job, a stage with failed jobs stops the run,
and a new tree drops the later stages.
Automated runs are left out like sandbox runs: task conversations started by a heartbeat or an eval, and conversations
whose opening prompt (its letters, first 80) is shared by at least 20 conversations, at least 80% single-turn, unless
the opening names a project or existing work topic.
Tree: the model proposes topics over batches of conversations (automated runs left out) seeded by the accepted
`project:` tags with their aliases and the existing work topics, then consolidates one tree; a project takes the casing
of an existing topic of the same name. The kernel refuses a new name that is a conversation title, a task, an activity
(review, fix, task, punch list, ops, operations, issue, resolution, triage, audit, smoke, chores), vague (den, program,
initiative, stuff, things, workstream), a path or file name, ticket, version, branch or id, a catch-all, rejected or
deleted, deeper than 4 levels, or held by fewer than 3 conversations (estimated, then again after filing, when a dropped
topic's filings move to its parent). Two names are one area when the word stems of one, leaving out the projects' own
words, lie within the other's; an existing root topic that shares a project's name word, and a new subtopic that merges
such a topic, fold into that project. A review pass then shows the model the whole draft tree (path, estimated
conversations, what each covers) and the topics that share a distinctive word in different places, in two rounds, and
applies its changes in order, checkpointed as `tree-review.jsonl`: merge a topic into another, move it under another or
to the root, rename it, or group topics under a new or existing parent (a group named after one of its topics gathers the
rest under that topic). Projects are never merged, moved or renamed; existing topics keep their names; nothing merges
into a whole project; a merge needs the two names to share a distinctive word; a new or changed name passes the same
refusals and may not be another topic's; nothing passes 4 levels; a change that breaks a rule is declined and listed;
topics left under 3 estimated conversations are dropped again. The report's tree shape counts topics, top-level topics,
topics sharing a distinctive word stem with an unrelated topic, and filings left at a top-level topic.
Filing runs note 42's rules, with this note's changes to them (live too): after the folder rule, a repo rule files work
whose title, summary or directory refers to a repo (a `github.com/<owner>/<repo>` link, a path segment, `<repo> PR #n`,
`<repo>#n`, `PR #n in <repo>`) that is exactly one root topic's name or alias; the name rule ignores names and aliases
that occur in more than 5% of the conversations (measured in the backfill; none live), and when the one named topic has
subtopics the model chooses between it and them (the prompt says which topic the conversation names); a suggestion is
a path only when a word is a file name with a known extension, and a version is `v` with one or two digits or digits
around a dot (so `families.app` and `V100` pass). Model questions go twenty at a time, one JSON object keyed by
conversation ref.
Fold: each loose page and old People, Hosts and Tools section gets one fate: merge into the most cited page of the same
title; an entity of the type its own entity reference names, unless its title is a code name (camelCase, three humps of
PascalCase, `_`, `(`, `:`, or "<Name> tool"); a section on the topic most of its cited conversations are filed in (a
strict plurality); otherwise the model's entity, section, new topic, merge, keep or remove. Remove stands only for clear
junk (a smoke, test, tmp, temp, scratch, dummy, placeholder or untitled word, an 8-digit run, or an empty uncited page);
otherwise the page goes to its most cited topic or is kept. An entity whose name holds another entity's whole name merges
into it (same type first, then the most extended name).
Proposals: a project or new topic becomes `createTopic` with its non-generic aliases and the sessions and conversations
the rules or the model filed there; an existing topic gets `move`, `rename` and `fileInto` (new: `{topic, sessions,
conversations}`); a merged existing topic gets `merge`; a page merge `mergePage` (`{page, into}`, applied as a fold) and
a removal `removePage` (`{page}`, which deletes the page and its file). `createTopic` gains optional `aliases`. A
`createTopic` or `fileInto` from the backfill lists every member it files in `evidence.samples` (`id`, `title`,
`lastActive` in epoch milliseconds, all from the snapshot), so a client shows each session by name without another
request; the weekly sweep keeps three samples, and `lastActive` is optional there. Your
placements, child sessions and automated runs are never proposed. `report.md` and `report.json` hold the left-out runs,
the tree with session counts and samples, filings per rule, a sample of 60, the fold outcomes, the junk names and the
time each stage took.
(50) Plug-in host (Phil, 2026-10-29; notes 51–60 are proposed and await his review): RivetHub's core is the workspace
and memory, plus delegation, notifications and tasks; everything else is a plug-in, a program that runs in its own
process on one node, is described by a manifest and reaches rh only over B17. No plug-in code is loaded into rh, so
ARCHITECTURE's "no runtime plugin loading" still holds. The gateway process (`rh gateway`, and `rh local`) is the host:
it keeps the registry, supervises each enabled plug-in's process, proxies clients to it, delivers its events, runs its
jobs and answers its API calls. A plug-in is installed on one node and runs there. `GET /api/plugins` on a node lists
only that node's plug-ins, each carrying the node's name, so a client connected to several nodes shows every node's
plug-ins labelled by node and reaches each through its own node; a mesh-wide plug-in is not part of this contract. Each
plug-in owns `<state>/plugins/<id>/` (B11): `data/` for its own files and databases (kept across reloads and disables;
what is worth remembering goes into memory through `memory.write`), `run/` for its socket and `logs/`. The registry is
`<state>/plugins/registry.json`, mode 0600, holding each plug-in's path, last valid manifest, enabled flag, granted
permissions and install time. The `plugins:` key of `config.yaml` stays accepted and ignored (decision 10): plug-ins are
managed only through B17. Trust: a plug-in runs as the node's user. Its token and permissions bound what it can do
through rh, and on Linux its process is confined with Landlock to what note 54 lists. Every plug-in process is started
by one launch function in the gateway's `plugins` module, from a thread of its own that confines itself first, so the
process inherits the confinement and rh does not. Where the kernel lacks any Landlock rule rh uses (it needs ABI 9 for
all of them), a plug-in that asks for a `host.*` permission cannot be enabled (`409` `sandbox_unavailable`, saying
whether Landlock is missing or partial), and a start that comes out less than fully confined is stopped before the
process runs; any other plug-in runs as confined as the kernel allows, or unconfined without Landlock. Each start's
`host` log line says whether the process is confined. Bodies are the `wire::plugin` types in `contract/*.schema.json`. The routes are new, with no
TypeScript recording.
(51) Plug-in manifest (proposed 2026-10-29): a plug-in is a directory holding `rivethub-plugin.json`, a
`wire::plugin::manifest::PluginManifest`. `manifest` is 1. `id` is 2 to 24 characters of `a-z`, `0-9` and single
hyphens, starting with a letter and not ending with a hyphen, unique on the node and never `plugins`, which names the
creator's tools (note 71); it names the plug-in's routes, folder and tools. `name`, `version` and `description` are text; `icon` (optional) is a Material Symbols name, as is every icon
in B17, and a client draws its generic plug-in glyph for a name it lacks; `$schema` (optional) is ignored, for editors.
`run` is `command` (an argv, never a shell line: a first element without a slash is looked up on `PATH`, one with a
slash is relative to the plug-in directory, which is also the working directory; an absolute path or one with a `..`
is refused, and a confined start refuses a path that resolves outside the directory through a link), `env` (extra variables; a `RIVETHUB_`
name is refused) and `readyTimeoutMs` (default 10000). `permissions` lists what it asks for (note 54). `settings` is a
list of fields (note 58) other than `file`, keys unique; rh keeps the values and each client draws them as a native form
on the plug-in's entry in Settings. `tools` are agent tools (note 57): `name` (1 to 24 characters of `a-z`, `0-9` and
single underscores, starting with a letter and not ending with an underscore), `title` (optional), `description`,
`inputSchema` (a JSON Schema object) and `hints` (`readOnly`, `destructive`, `idempotent`, `openWorld`). `contributes`
holds `pages` (`{id, title, icon?, view}`: a sidebar entry opening that view as a page), `topicCards` (`{id, title,
view}`: the view drawn as a card in the topic rail with `context.topicId`) and `sessionActions` (`{id, title, icon?,
action}`: a session-menu entry running that action with `context.sessionId`). `jobs` are `{id, title, schedule?,
timeoutMs?}` and `events` the kinds it subscribes to (note 56). Lists default to empty. View, action, job, tab, form,
canvas and contribution names are 1 to 64 characters of `a-z`, `0-9`, `-` and `_`, starting with a letter or digit.
Unknown fields are refused here and in every other B17 body. Install also refuses an empty `command`, a `file` setting,
duplicate tool, setting, job or contribution ids, an event whose permission is not asked for, and a `schedule` that is
not a five-field cron expression, answering `400` with the field's path.
(52) Plug-in process (proposed 2026-10-29): enabling starts `run.command` in its own process group with a scrubbed
environment: `PATH`, `HOME`, `USER`, `LANG`, `LC_ALL` and `TZ` from rh's own, then `run.env`, then
`RIVETHUB_PLUGIN_ID`, `RIVETHUB_PLUGIN_NODE`, `RIVETHUB_PLUGIN_DATA` (the data folder), `RIVETHUB_PLUGIN_SOCKET`,
`RIVETHUB_API_SOCKET`, `RIVETHUB_PLUGIN_TOKEN` and `TMPDIR`, its `run/` folder. rh reaches the plug-in over HTTP/1.1 on the unix socket
`RIVETHUB_PLUGIN_SOCKET` (`<state>/plugins/<id>/run/plugin.sock`, in a 0700 folder rh creates and empties before each
start), where the plug-in listens; a unix socket rather than a localhost port, because nothing has to be allocated, two
plug-ins cannot collide and another local user cannot connect. The plug-in is ready when `GET /health` answers 2xx
within `readyTimeoutMs`. It serves `GET /health`, `POST /views/{view}`, `POST /actions/{action}`, `POST
/canvas/{canvas}/events`, `POST /tools/{tool}`, `POST /events`, `POST /jobs/{job}`, `GET /panel/{path}` and any method
on `/http/{path}`, only those its manifest needs; an unknown route is its own `404`. Disabling, removing, reloading or
stopping rh sends `SIGTERM` to the process group and `SIGKILL` 5 s later. A process that exits while enabled, or is not
ready in time, goes to `backoff` and is started again after 1 s, doubling to 60 s; ten minutes running resets the delay.
Its stdout and stderr lines are kept, with rh's own lines about it (start, ready, exit status, refused answers) as
stream `host`, in `logs/plugin.log`, rotated at 5 MiB with one previous file kept.
(53) Plug-in API (proposed 2026-10-29): rh mints a token of 32 random bytes (64 hex characters) at every start of a
plug-in's process and keeps only its hash; the token stops working when the process stops. It is accepted only on the
plug-in API socket `RIVETHUB_API_SOCKET` (`<state>/plugins/api.sock`, HTTP/1.1, one per node), never on the gateway's
own listeners, and every request there carries `Authorization: Bearer <token>`: a missing or unknown token is `401`, and
the loopback rule that makes a local caller the owner does not apply on this socket. The token names one plug-in; each
request is checked against that plug-in's granted permissions when it arrives, so disabling or narrowing takes effect at
once, and a route outside them is `403` with code `permission_required` and the permission as `detail`; a route no
permission names (the management routes among them) is `403` with the same code and no `detail`. The socket serves the
B1 routes each permission names (note 54) with their B1 bodies, acting as the node's owner, and three routes of its own:
`GET /api/plugin/self` answers `PluginSelf` (id, node, version, granted permissions, settings values including secrets,
and the data folder); `POST /api/plugin/views/changed` takes `{views}` and publishes `{type: "plugin.views", v: 1,
plugin, views}` on every `/api/events/ws` socket, so a client showing one of those views fetches it again (clients poll
nothing); and `POST /api/plugin/notify` (`notifications.send`) takes `{title, text?, tone?, action?}` and sends `{kind:
"plugin.notification", plugin, node, title, text?, tone?, action?, ts}` on every `/api/notifications/ws` socket, a
client running `action` when the person opens it. Memory a plug-in writes through `POST /api/capture` and the
`memory_append` tool route is attributed to agent `plugin:<id>` and kept under session keys that start
`plugin:<id>:`: rh rewrites the body's `agent` and puts that prefix on its `session_key` (`session_id` for the tool)
before the route sees it, so a plug-in cannot write into another conversation.
(54) Plug-in permissions (proposed 2026-10-29): a plug-in asks for permissions in its manifest; a person approves them
at enable (note 55), and any later increase is approved again before the process runs with it. rh checks the B1
permissions on every plug-in API call: `memory.read` (`GET /api/memory/*`, `POST /api/memory/tool/memory_search`,
`memory_browse`, `memory_stats` and `memory_get_full`, `GET /api/wiki*`), `memory.write` (`POST
/api/memory/tool/memory_append`, `POST /api/capture`), `sessions.read` (`GET` on `/api/harness-sessions*` and
`/api/harnesses/*`), `sessions.control` (`POST`, `PATCH` and `DELETE` on `/api/harness-sessions*`:
create, turn, steer, cancel, answer an approval, retitle, delete; and `POST /api/sessions/{session}/messages`, which
sends a turn and can start a session), `topics.read` (`GET /api/topics*`, `GET /api/sessions/history`, `GET
/api/sessions/suggestions` and `GET /api/sessions/{session}/placement`), `topics.write` (the other methods on `/api/topics*`, and `PUT` and `DELETE` on
`/api/sessions/{session}/placement` and `/api/sessions/{session}/history`; no other `/api/sessions` route is open to
a plug-in), `tasks.read` (`GET /api/tasks*`), `tasks.delegate`
(`POST /api/tasks`), `agents.read` (`GET /api/agents*` and `GET /api/catalog/agents`, note 116) and `notifications.send` (`POST /api/plugin/notify`). Events
need the permission of what they reveal (note 56). On Linux the process is confined (note 50): it reads and writes its
own `data/` and `run/` folders; reads its install directory, the program it runs (and the `lib` folder beside the `bin`
folder it sits in, if it does), `/usr`, `/bin`, `/sbin`, `/lib`, `/lib32`, `/lib64`, `/etc`, `/proc`, `/sys`,
`/dev/null`, `/dev/zero`, `/dev/random` and `/dev/urandom`; writes `/dev/null`; connects to the plug-in API socket; and
opens no TCP connection or listener, reaches no abstract unix socket and signals no process outside its own. The host
permissions widen that: `host.files.read` reads every file and `host.files.write` writes every file (files outside the
plug-in's own folder, still within the node user's rights), and `host.network` allows TCP, abstract unix sockets and
reading the resolver configuration, but no TCP connection to the node's own rh listeners (the gateway's port and the
mesh agent channel's), where a loopback caller counts as the owner. `host.exec` (running other programs) is shown for consent and recorded; a program
the plug-in runs inherits its confinement, so it is not checked apart. Landlock does not confine UDP, and confines
connecting to pathname unix sockets only from its ABI 9. The tools and contributions a manifest
declares need no permission; clients show them beside the permissions when asking for approval. The approval is the
owner's `enable` call, and rh cannot tell a person from an agent using the owner's credentials, so an agent that builds
a plug-in installs it and a client asks the person to enable it.
(55) Plug-in routes (proposed 2026-10-29): every route but listing and showing answers only the node's owner (`403`
`owner_required` for any other user); every authenticated user of the node can list plug-ins and use their views,
actions, canvases, panels and tools, and rh names the caller to the plug-in in `x-rivethub-user`. `GET /api/plugins`
answers `PluginList` `{node, plugins}`, and `GET /api/plugins/{id}` one `PluginRecord`: `{id, node, path, manifest,
enabled, state, granted, pending, error?, lastExit? {at, code?, signal?}, restarts, startedAt?, installedAt, jobs: [{id,
lastRun?}]}`, `state` one of `disabled`, `awaitingApproval`, `starting`, `running`, `backoff` and `invalid` (the
manifest at `path` is gone or no longer valid; `error` says why, and `manifest` is the last valid one). `POST
/api/plugins` takes `{path}`, an absolute directory on the node, reads and checks its manifest (note 51) and registers
the plug-in in place, disabled, with every permission it asks for `pending`, answering `201` with the record; it runs
and copies nothing, so an agent building a plug-in edits that directory and reloads. An invalid manifest is `400` naming
the field, and an id already installed is `409` (from the same directory the message says to reload it). `POST
/api/plugins/{id}/enable` takes `{approve}` (`{}` when nothing is pending), which must name exactly the `pending`
permissions (`409` `permissions_unapproved` listing them, `400` `not_pending` for one not pending, `409`
`sandbox_unavailable` for a `host.*` permission on a kernel without full Landlock, note 50); it grants them, starts the
process and answers the record. An enable that grants a permission the plug-in does not hold yet needs a caller
presenting a paired device certificate (a device certificate the node maps to its owner): a node or agent certificate, a
bearer token or a loopback caller is `403` `device_certificate_required`. rh checks it under the lock that orders
enable, reload and the other changes, against exactly the permissions that call grants, so a reload queued ahead of it
cannot swap in permissions the check never saw. The device is read apart from the owner check
(`routes/plugins/approval.rs`) until the approval identity is decided. `POST …/disable` stops it and keeps its grants,
settings and data. `POST …/reload` reads the manifest again: an invalid one is `400` and changes nothing; fewer
permissions narrow the grant at once; more stop the process in `awaitingApproval` with the new ones `pending` until
enable approves them; otherwise an enabled plug-in restarts. `DELETE /api/plugins/{id}` stops and unregisters it,
removes its folder unless `?keepData=true`, never touches the source directory, and answers `204`. `GET …/logs?tail=<n>`
(default 200, at most 2000) answers `{lines: [{at, stream, text}]}`, oldest first, `stream` one of `stdout`, `stderr`
and `host`. `GET …/settings` answers `{values, secretsSet}`: each field's kept value, else the manifest's `value`, a
secret's value left out and its key in `secretsSet`; `PUT` takes `{values}`, where `null` clears a key back to the
manifest's `value`, and refuses with `400` naming the key a key the manifest lacks, a value that does not fit its
field's input (its type, `min` and `max`, a select's options) or a required field left without a value; otherwise it
keeps them in the registry, answers as `GET` does and delivers `settings.changed` to the plug-in when it runs (one that
starts later reads them from `PluginSelf.settings`, which holds the same values with secrets). `POST …/jobs/{job}/run` is note 56. Every change to a record
publishes `{type: "plugin.changed", v: 1, plugin}` on `/api/events/ws`. The proxy forwards
`/api/plugins/{id}/{views,actions,canvas,tools,panel,http}/…` to the plug-in socket with the `/api/plugins/{id}` prefix
removed (a path with a `.` or `..` segment, plain or percent-encoded, or a segment hiding an encoded `/` or `\` is
`400`, so no path climbs out of its route), adds `x-rivethub-user`, `x-rivethub-node` and `x-rivethub-request-id`,
and drops the caller's `Authorization`, cookies and every `x-rivethub-*` header it sent (a name rh cannot put in a
header fails the call with `500` rather than reach the plug-in without it). On `views`, `actions`, `canvas` and `tools` rh parses the request and the answer as their `wire::plugin`
types and sends the client the answer as it parsed it: an answer that does not parse is `502` `plugin_invalid_response`
with the field path as `detail` (the full path to the offending value, such as `blocks[1].items[1].title`, found by
checking the answer against its type's schema, where serde stops at a tagged block) and a `host` log line; an action's `422` reaches the client as it is, and any other
status but `200` is `502` `plugin_error` with a `host` log line naming the status and the answer's first 1 KiB; a
`tools` call reaches only an installed plug-in. `panel` and `http` pass status, headers (less hop-by-hop headers and
`Set-Cookie`) and body through, streamed. An unknown plug-in is `404`, one not running `503` `plugin_unavailable`
(retryable), and no answer in time `504` `plugin_timeout` (30 s; 120 s for tools; 60 s to the first byte on `http`).
Request bodies are capped at 16 MiB and parsed answers at 4 MiB.
(56) Plug-in events and jobs (proposed 2026-10-29): a plug-in receives the event kinds its manifest lists as `POST
/events` with a `PluginEventDelivery` `{seq, at, missed, event}`, one at a time and in order; `seq` grows by one per
delivery to that plug-in, so a repeat can be dropped. The kinds (`event.type`) are `session.started` (`sessionId`,
`topicId?`, the topic it was started in), `session.idle` (a turn ended and the session waits for input), `session.ended` and
`session.placed` (`sessionId`, `topicId`, `by`), which need `sessions.read`; `summary.written` (`summaryId`,
`sessionId?`, the summarized conversation's session key when it is a canonical session id, `kind`, `level`, its depth),
which needs `memory.read` (every summary write announces itself: in Postgres with `NOTIFY rh_summary_written` in the
write's transaction, which the gateway listens for, so summaries from worker processes reach it; in SQLite, where
`rh local` runs its workers, directly); and `task.done` (`taskId`, `status`), which needs `tasks.read`.
`settings.changed` (`values`) goes to every plug-in whether or not it lists it. A 2xx answer acknowledges a delivery;
anything else, or no answer in 10 s, is tried again after 1 s, doubling to 30 s, five tries in all. Each plug-in has a
queue of at most 1000 events, kept while it is starting or in backoff and dropped when it is disabled; events lost to
overflow or after the last try are counted in the next delivery's `missed`. Delivery is at least once within one gateway
process; events queued when rh stops are lost, so a plug-in that sees `missed` reconciles through the API. A job with a
`schedule` (five-field cron, the node's local time) runs at each tick, and any job runs on `POST
/api/plugins/{id}/jobs/{job}/run` (`202` with the run, `409` while one runs). A run is `POST /jobs/{job}` with `{runId,
trigger, scheduledAt?}` (`trigger` `schedule` or `manual`), answered by `{outcome, message?}` (`succeeded` or `failed`)
within `timeoutMs` (default 600000; else the run failed, "timed out"). A job runs once at a time, so a tick while it
runs is skipped, and ticks missed while rh was down are not made up. Each job's last run is on the plug-in record.
(57) Plug-in tools in MCP (proposed 2026-10-29): `rh mcp` lists, after B7's nine, the plug-in creator's four tools (note
71) and then, in manifest order, the tools of every running plug-in on its node, named `<id>-<tool>` with a single
hyphen (Grok refuses a name holding `__`). A tool name holds no hyphen (note 51), so the last hyphen splits a name into
the id and the tool: no two plug-in tools can share a name, and none can take one of B7's, which hold no hyphen. The
name is at most 49 characters of `a-z`, `0-9`, `-` and `_`, inside the `^[a-zA-Z0-9_-]{1,64}$` every harness accepts,
with 15 left for a harness's own prefix (Claude Code's `mcp__<server>__`). Each has the manifest's `title`,
`description` and `inputSchema`, and the `hints` as MCP
`annotations` (`readOnlyHint`, `destructiveHint`, `idempotentHint`, `openWorldHint`, each `true`
when listed and `false` otherwise). B7's nine tools are unchanged. `rh local` serves MCP in the gateway process and
reads the set from the host directly; a separate `rh mcp` reaches the node's gateway on the local socket
`<state>/plugins/tools.sock` (mode 0600, in the mode 0700 `<state>/plugins` folder). Each request carries `Authorization:
Bearer` the token rh writes to `<state>/plugins/tools.token` (mode 0600) at each start, which a confined plug-in cannot
read; any other request is `401`. `GET /tools?after=<version>` answers `PluginToolList` `{version, tools}`,
waiting up to 60 s while the version still equals `after`, and `POST /tools/<id>-<tool>` takes a call. Either way the
server sends `notifications/tools/list_changed` when the version moves. A call carries `{arguments, sessionId?}` (the
calling harness session when rh can tell it: a `tools/call` whose
`_meta` holds `claudecode/toolUseId`, as Claude Code sends, names the session rh is driving whose running tool call has
that id, waited for up to 1 s; a separate `rh mcp` passes that id to the tools socket as `x-rivethub-tool-use`) and goes on to the plug-in as `POST /tools/{tool}`; clients
call the same tool through the proxy as `POST /api/plugins/{id}/tools/{tool}`, answered by `{text, isError}` within 120
s; the MCP result is one text content with that `isError`. A plug-in that is not running, a timeout or a refused answer
is an MCP result with `isError: true` naming the cause. Every harness whose MCP server is the node's `rh mcp` thus gets
a plug-in's tools as it gets the memory tools.
(58) Plug-in views (proposed 2026-10-29): a plug-in's UI is views it answers and each client draws with its own native
widgets and theme, so a plug-in keeps the app's style on desktop, Android and web. A client asks for one with `POST
/api/plugins/{id}/views/{view}` `{params?, context}` (`context` is `{topicId?, sessionId?}`, filled from the
contribution that shows it) and gets a `View`: `title`, `subtitle?`, `toolbar` (buttons beside the title) and `blocks`,
drawn top to bottom. Blocks (`type`): the containers `section` (`title?`, `subtitle?`, `actions`, `blocks`,
`collapsed?`, absent for a section that does not collapse), `split` (`primary` and `secondary` blocks side by side on a
wide window and stacked on a narrow one, `ratio?` the primary share) and `tabs` (`tabs` `[{id, title, badge?, blocks}]`,
switched locally, `selected?`); the content blocks `text` (`text`, `tone?`, `monospace`), `markdown` (CommonMark with
tables, no raw HTML, links opened in the browser), `code` (`text`, `language?`, `follow`: keep the end in view, for
logs), `image` (`path` on the plug-in's `http/` route, `alt`), `properties` (`items` `[{label, value}]`, `value` an
inline), `progress` (`value?` from 0 to 1, absent for indeterminate; `label?`), `notice` (`tone`, `title?`, `text`,
`actions`), `list` (`items` `[{id, title, subtitle?, icon?, trailing, action?, menu}]`, `empty?`), `table` (`columns`
`[{title, align?, sortable}]`, `rows` `[{id, cells, action?, menu}]` with one inline per column, `empty?`; a sortable
column sorts locally by its cells' values) and `breadcrumbs` (`items` `[{label, action?}]`); and the controls `actions`
(`buttons`), `form` (`id`, `fields`, `buttons`), `canvas` (note 59) and `webPanel` (note 60). `empty` is `{title, text?,
button?}`. Inlines (`type`): `text` (`text`, `tone?`, `icon?`, `monospace`), `badge` (`text`, `tone`), `time` (`at` in
epoch milliseconds, `style?` `relative` or `absolute`), `duration` (`ms`) and `bytes` (`value`), each formatted by the
client in its own locale. Tones are `neutral`, `info`, `success`, `warning` and `danger`. A button is `{label, icon?,
style?, action, confirm?, disabled}`, `style` one of `primary`, `secondary` (the default) and `danger`, and `confirm`
`{title, text, confirmLabel?}` asked before the action runs; a menu is a list of buttons. A field is `{key, label,
input, value?, required, help?, placeholder?}`, `input` (`type`) one of `text` (`multiline`), `secret`, `number`
(`min?`, `max?`, `step?`), `toggle`, `select` (`options` `[{value, label}]`, `multiple`) and `file` (`multiple`,
`accept`: media types); a value is a boolean, a number, a string, a list of strings or a list of files `{name, mimeType,
data}` (`data` in base64). Actions (`type`): `call` (`action`, `params?`: `POST /api/plugins/{id}/actions/{action}` with
`{params?, values?, context}`, `values` being the form's when the button belongs to a form), `navigate` (`view`,
`params?`, `replace`: open that view as a page, or in place of the current one), `dialog` (`view`, `params?`: fetch it
and show it modally), `back` (close the dialog, else go back a page), `refresh` (fetch every view of this plug-in on
screen again), `toast` (`text`, `tone?`), `copy` (`text`, to the clipboard), `openSession` (`sessionId`), `openTopic`
(`topicId`), `openUrl` (`url`, `http` or `https`, in the browser) and `download` (`path` on the plug-in's `http/` route,
`filename?`). A call answers `200` with `{actions}`, run in order (a `call` among them is refused as an invalid answer),
or `422` with `{message, fields}`, which the form shows beside the named fields. Pages keep a back stack; a `navigate`
from a topic card or a session action opens the page in the main area.
(59) Plug-in canvas (proposed 2026-10-29): a `canvas` block is `{id, scene, height?, readOnly, menu}`; the client draws
the scene natively, handles pan, zoom, selection and dragging itself, and tells the plug-in only the results. A scene is
`{items, grid?: {size, snap}, viewport?: {x, y, zoom}}` in canvas units (logical pixels at zoom 1, y downwards), its
items drawn in order. An item (`type`) is a `node` (`id`, `x`, `y`, `width`, `height` (its box, used for hits, selection
and connector routing), `shapes`, `anchors`, `tooltip?`, `menu`, `locked`: not draggable, `decorative`: not selectable,
draggable or deletable and has no menu, for lanes and labels) or a `connector` (`id`, `from` and `to` `{node, anchor?}`,
without an anchor meeting the node's box; `route` `straight`, `curved` or `orthogonal`; `stroke?`, `strokeWidth?`,
`dashed`, `arrow` `none`, `end`, `start` or `both`, `label?` at its middle, `tooltip?`, `menu`, `decorative`). Shapes
(`type`) are placed relative to their node's top-left: `rect` (`x`, `y`, `width`, `height`, `radius?`, `fill?`,
`stroke?`, `strokeWidth?`), `ellipse` (the same without `radius`), `text` (`x`, `y`, `text`, `size?` (default 13),
`weight?` `regular`, `medium` or `bold`, `color?`, `align?`, `maxWidth?` past which it ends in an ellipsis,
`monospace`), `path` (`d`, SVG path data; `fill?`, `stroke?`, `strokeWidth?`, `dashed`) and `icon` (`x`, `y`, `name`,
`size?`, `color?`). An anchor is `{id, x, y, side, direction}`: `side` (`top`, `right`, `bottom`, `left`) is where a
connector leaves it and `direction` is `in`, `out` or `both`. Colours are theme roles, never literal values, so a canvas
follows the light and dark themes: `surface`, `surfaceContainer`, `surfaceContainerHigh`, `primary`, `primaryContainer`,
`onPrimary`, `onPrimaryContainer`, `secondaryContainer`, `onSurface`, `onSurfaceVariant`, `outline`, `outlineVariant`,
the tones `info`, `success`, `warning` and `danger`, and `category1` to `category6` to tell kinds apart; an unset fill
is `surfaceContainer`, stroke `outlineVariant`, text `onSurface`, icon `onSurfaceVariant` and connector `outline`.
Locally the client pans (dragging empty space, scrolling), zooms (pinch, or ctrl or cmd with scroll, from 0.1 to 4, with
a fit control), selects (click; shift or cmd click to toggle; shift-drag on empty space for a box; Escape clears), drags
the selected nodes that are not locked, snapping their top-left to the grid when `snap`, with attached connectors
following, draws a connector from an anchor to an anchor of another node (`out` or `both` to `in` or `both`), deletes
the selection on Delete or Backspace, activates an item on double-click or Enter, and opens an item's `menu`, or the
block's `menu` on empty space, on right-click or long press; the selection, hover and provisional connector are drawn in
`primary`. `readOnly` turns off dragging, connecting and deleting. Each result goes to `POST
/api/plugins/{id}/canvas/{canvas}/events` as `{event, params?, context}` (the view's params and context), one at a time
per canvas and in order, consecutive `moved` and `selected` events merged: `selected` (`items`), `moved` (`items` `[{id,
x, y}]`, at the end of a drag), `connected` (`from`, `to`), `deleted` (`items`), `activated` (`item`) and `menu`
(`item?`, absent on empty space, `choice` the menu item's id, and `x`, `y` in canvas units). The plug-in answers `{ops,
actions}`: `ops` (`type`) are `upsert` (`item`, added or replacing the item with its id) and `remove` (`ids`), applied
to the scene before `actions` (note 58) run, so a selection can `navigate` with `replace` to show an inspector beside
the canvas. Until the answer arrives the client shows its own result: moved nodes stay where they were dropped unless an
upsert places them; a new connector is drawn provisionally and dropped when the answer comes, the plug-in's upsert
adding the real one; deleted items are hidden and come back unless the answer removes them. When the view is fetched
again, a canvas with the same id keeps its viewport and selection (less items that are gone); `viewport` is used only
the first time, and without one the client fits the content. A scene holds at most 5000 items.
(60) Plug-in web panel (proposed 2026-10-29): a `webPanel` block (`{id, entry, height?, params?}`) shows the plug-in's
own HTML, `GET /panel/{entry}`, for what blocks and canvases cannot draw. It is specified here and built after the web
panel test build on Wayland. The client loads it in a web view under an opaque origin of its own for each plug-in and
node, serving `/http/…` from the plug-in's `http/` route and every other path from its `panel/` route through the proxy
with the client's own credentials, so the page never holds a token or cookie of the app and reaches nothing but its own
plug-in. Its content security policy allows only that origin (`default-src 'self'`, inline styles, `img-src 'self' data:
blob:`), with no top-level navigation, pop-ups or downloads of its own. The client posts `{type: "init", theme, params?,
context, plugin, node}` once the page has loaded and `{type: "theme", theme}` when the theme changes, `theme` being
`{scheme, colors, fontFamily, fontMono, fontSize, radius}` (`scheme` `light` or `dark`, `colors` a CSS colour per colour
role of note 59), and sets the same theme as CSS variables on the document: `--rh-color-<role>` with the role in kebab
case (`--rh-color-surface-container`), `--rh-font-family`, `--rh-font-mono`, `--rh-font-size`, `--rh-radius` and
`color-scheme`. The page posts `{type: "ready"}`, `{type: "resize", height}` and `{type: "action", action}`, which runs
any action of note 58 as a button would. A panel has no permissions of its own: what it can do is what its plug-in's
routes do.
(61) Conversation summaries (Phil, 2026-10-29): `GET /api/memory/conversations/{id}/summaries` answers one
conversation's summary DAG by its id, as `MemorySummaryDagResponse` with `sessionId` set to the conversation's own key,
so a conversation filed in a topic that is no harness session shows its summaries. A segment that isn't a uuid is `400`
and an unknown conversation is `404`. `GET /api/memory/summaries/{session}` is unchanged.
(62) Page owners and citations (Phil, 2026-10-29): each `WikiTopic` row of the wiki listing (`GET /api/wiki`,
`/api/wiki/topics`, `/api/wiki/search` and `_gaps`' `stalest`) carries an optional `topicId`, the topic or entity that owns
the page, as `GET /api/wiki/{slug}` already did, so a client tells an entity's page from a page without a topic. Each
`WikiCitation` gains what a reader needs to list who cites the page, like Wikipedia's "What links here": optional
`conversationId` (the cited summary's conversation), `sessionId` (that conversation's harness session, when it is one, by
the filing rule of (42)), `title` and `channel` (the conversation's), and `topicId` (where it is filed: the session's
placement, or the conversation's own placement when it is no session). A citation whose summary is gone keeps only its
own fields.
(63) Page edits by topic (Phil, 2026-10-29): `PUT /api/topics/{topic}/page` (an id or slug) with
`{ "currentState"?, "article"? }` (`WikiPageEdit`) replaces `PUT /api/wiki/{slug}` of (41). It edits the page
`GET /api/topics/{topic}/page` shows (an entity's own page, a work topic's root page), writing that page first when the wiki
hasn't, so an entity the builder hasn't written a page for yet can be edited, and answers `TopicPage` as the GET does. The
commit, history entry and user source are as (41) says. A type has no page (`409`, "A type has no page"); an unknown topic
is `404`; wiki pages not configured is `503`.
(64) Conversation nodes (Phil, 2026-10-29): `POST /api/capture` records the receiving node's name (`mesh.node_name`,
as `/healthz` reports it) on a conversation it creates or captures into, the first node to capture it winning.
Postgres migration `0037_conversation_node.sql` adds the nullable `rh_conversations.node`; SQLite schema 23 keeps it in
`rh_conversation_nodes` so the legacy `ros_conversations` table is unchanged. `TopicSession` (the newest node across the
session's conversation keys) and `TopicConversation` on `GET /api/topics/{topic}/sessions` carry an optional `node`.
Conversations captured before 0037, appended through `memory_append`, or imported, have none.
(65) Topic mentions (Phil, 2026-10-29): `TopicPage` (`GET` and `PUT /api/topics/{topic}/page`) gains `mentions`, the
entities whose pages cite summaries of the sessions and conversations filed in the topic or anywhere under it, as
`TopicMention` (`topicId`, `name`), most mentioned first and at most 24. A type or an entity answers none.
(66) Live tool input (Phil, 2026-10-29): `HarnessEvent` gains `tool-update`,
`{ "type": "tool-update", "sessionId", "toolCallId", "input", "turnId"? }`. A harness session's event stream carries it
while a tool call runs, each time the agent names the call's input in an ACP `tool_call_update`, as claude-agent-acp
does once a streamed call's input is known; `input` is the call's whole input as it stands and replaces the one its
`tool-use` gave. A `tool-use` `name` is the ACP tool call's `name` when the agent sends one, else its `title`, so a
claude-agent-acp call is `Bash` or `Read`, as in its transcript, rather than "Terminal" or "Read src/main.rs". Claude
chats run claude-agent-acp with `CLAUDE_CODE_ENABLE_TODO_TOOLS=1`, so Claude keeps its task list with `TaskCreate` and
`TaskUpdate` and the stream carries the list as `plan` (32). Bodies are `wire::harness::HarnessEvent` in
`contract/*.schema.json`.
(67) Forks (Phil, 2026-10-29): `POST /api/harness-sessions/{session}/fork` with `{ "prompts" }` starts a new chat of
the session's harness, on its model and in the directory it ran in (as a resume would, so it need not sit under the
terminal root; a directory that is gone is `400`), that carries the conversation up to and including the reply to its
`prompts`th prompt, and answers the new chat's `HarnessSessionSummary`. The conversation travels as
startup context, the way topic memory does (25): one block, `## Earlier conversation (forked from <title>)`, then a
`### User` or `### Assistant` section per turn with its text and the tools it used, the oldest turns left out past
60,000 characters. The new chat is not filed anywhere and takes no topic memory. It is authorised like reading the
transcript; `prompts` of 0 or more than the session has is `400`, and a harness whose chats take no startup context
(opencode, hermes, kimi-code) is `501` with code `capability_unsupported`. Bodies are
`wire::gateway::sessions::HarnessSessionForkRequest` in `contract/*.schema.json`. The route is new, with no TypeScript
recording.
(68) Waiting approvals on attach (Phil, 2026-10-29): after each `transcript` a harness session socket sends, it sends
again every `approval-request` of that chat still waiting for an answer, so a client that opens the session after the
request went out, as from a needs-you notice, can still answer it. A request leaves the set when it is resolved, the
turn completes or fails, or the session stops being active.
(69) Waiting on a task (Phil, 2026-10-30): a caller that stops waiting never ends the task. When `POST /api/tasks?wait=1`
or `GET /api/tasks/{id}/wait` reaches its `timeoutMs` (default 30 s, clamped to 1 ms–120 s), the task keeps running and
the answer is `504` with `{ "error": "wait deadline exceeded", "task" }`, the task as it stands, so the caller has its
`id` and `status`; before, the `POST` killed the task (`killed`, "Killed by parent") and the `GET` sent no task. MCP
`delegate_task` likewise leaves the task running when its wait ends or the call is aborted, its timeout reply names the
task's id and status, and the wait is no longer the task's wall-clock budget. Only `POST /api/tasks/{id}/kill` ends a
task. Bodies are `wire::task::TaskWaitTimeoutResponse`.
(70) Bulk filing reads (Phil, 2026-10-30): a client reads every filing in one request instead of one per topic and one
per unfiled session. `GET /api/topics/filing` answers `{ sessions, conversations, history }`: `sessions` and
`conversations` are the rows `GET /api/topics/{topic}/sessions` answers (notes 42 and 64) for every topic at once, each
naming its `topicId`, newest `lastActive` first (rows without one last); `history` is what `GET /api/sessions/history`
answers (note 36). `GET /api/sessions/suggestions` answers `{ sessions: [{ sessionId, name, topicId?, parentId? }] }`,
the suggestion of every unplaced session that has one, as `GET /api/sessions/{session}/placement` carries it, in session
id order. Each route is one store query on Postgres and on SQLite. As with `search`, `filing` in that position is the
route, not a topic slug; a topic whose slug is `filing` is still reached by its id. Bodies are
`wire::gateway::topics::TopicFiling` and `SessionSuggestionList`; the routes are new, with no TypeScript recording.
(71) Plug-in creator (proposed 2026-10-30): an agent builds a plug-in on its own node with four tools of the host's,
which `rh mcp` lists as note 57 says under the reserved id `plugins`: `plugins-templates` `{}`, `plugins-scaffold`
`{template, id, name, description}`, `plugins-install` `{id}` and `plugins-status` `{id?, lines?}` (default 40, at most
200). They read templates only from the node's `<state>/plugin-templates/` and build only in its plug-in workshop
`<state>/workshop/` (B11). rh carries no template and fetches none: each real folder in `<state>/plugin-templates/` is
one, a working plug-in whose manifest is checked as note 51 does, written there by rivethub-plugins' `npm run
templates:install` (its `javascript` template: a checklist page with a form, a list and a canvas board, an agent tool,
a `session.placed` subscription, `GUIDE.md` on the build loop and the limits of notes 51–59, and the SDK's sources).
Templates lists them, or says that a person has to install them. Scaffold copies the template's plain files and
folders (a link is refused) into a new `<state>/workshop/<id>/` (an existing one is refused) and sets its manifest's
`id`, `name` and `description`. Install takes the real folder `<state>/workshop/<id>` (a link is refused) whose manifest
names that id, checks the manifest as note 51 does (a refusal names the field and changes nothing), then installs the
folder (note 55) or reloads it; a plug-in of that id installed from elsewhere is refused. It waits for a starting
process up to the manifest's `readyTimeoutMs` plus 2 s and then asks each page of a running plug-in for its view with an
empty context, checking the answer as the proxy does and logging a refused or failed answer as a `host` line. Status
answers the record's state, permissions and last exit with the log's last lines, or without an id the node's plug-ins.
The tools never enable a plug-in or grant a permission: a new plug-in stays off, and an answer that leaves one off or
awaiting approval says that a person approves it in the app (Settings › Plug-ins › the plug-in) on a paired device. A
call from a session rh can tell (note 57) whose owner is not the node's owner is refused. Answers are text results; a
refusal or a failure is `isError`.
(72) Session titles (Phil, 2026-10-30): a conversation title names its session unless it is blank, a harness
placeholder (`<harness> session` for Claude Code, Codex, Grok Build, Kimi Code, dsh, OpenCode, Pi and Qwen Code,
`<name> session (imported)`, or a default such as `Untitled`, `New session`, `New chat` or `New conversation`), or starts with an injected context block (an XML-style `<tag>…</tag>`, or an
`# AGENTS.md instructions` header). `POST /api/capture` keeps a batch's `title` as the words after any leading injected
blocks, and only when they name the session, so a placeholder or an injected block never replaces a stored title and a
new conversation without one stays untitled; a later batch with the person's words replaces a placeholder. When a leaf
summary is stored and the title still does not name the session, the title comes from the earliest leaf that gives one:
a leaf that opens with a heading gives its first heading that names the session (skipping a heading that only frames
the batch, such as `Session metadata` or `Recommended plugins`), any other leaf its first sentence, at most 80
characters. `rh memory backfill-titles [--dry-run]` applies the same to every summarized conversation on Postgres and
SQLite. Tidy proposals (43) answer each evidence sample with its conversation's current title, the stored one only when
no conversation matches. The rule is `kernel::memory::{names_session, spoken_title, summary_title}`.
(73) Tolerant event batches (Phil, 2026-10-30): the hook kits send kinds rh has no v1 event for (`thinking.end`,
`speech.stt`, `tool.end`, `term.line`) and activities it has no value for (`writing_plan`), and refusing their whole
batch dropped the `message.user`, `message.agent` and `turn.end` beside them. A `POST /events` element whose `type` is
not a v1 kind is now skipped, and an `activity` element whose `activity` is not a v1 value is kept as `thinking`, the
working state a session reports with nothing more specific; v1 has no field for the value it named. The rest of the
batch is ingested and `ingested` counts what was kept. Each batch logs its skipped kinds and generic activities with
their counts at debug, and each unknown kind or value is logged once at warn. An element of a known kind that does not
validate otherwise still refuses the whole batch with `422 {"error": "event[i] is not a valid v1 AgentEvent"}`, as
recorded; so does a batch that is not an array of 1–100 elements. `POST /event` is unchanged.
(74) Delegated tasks (Phil, 2026-10-29): a task is a harness session started by another session, its child.
`POST /api/tasks` takes `goal` and `agentId` (required; `agent@node` pins the node, as does `nodeAffinity`), and
optional `acceptanceCriteria`, `budget` (`maxWallClockMs`, `maxTurns`), `parentSessionId`, `model`, `effort` (a thinking
level), `workingDir` and `requestedBy` (`wire::task::TaskCreateRequest`). The gateway that takes the request resolves the
target once: a preset runs on its node with its harness, model ("default" leaves the harness its own) and effort; a runtime
agent runs on the newest online node that hosts it, on its CLI provider's harness. An agent on a model provider and a
preset with no harness are refused with a `400` that names them (an opencode or pi preset is the way to delegate to such a
model), an unknown agent is `404` and a mesh node known to be offline is `503`. The task is queued as `run-task:<node>`
for that node only, never the unscoped `run-task`. That node's gateway claims it, starts the child with the task's
parent, task id and the parent's topic memory, and sends one turn: the goal, an "Acceptance criteria" list and a line
asking for a short summary that goes back to the asker. The child's events drive the task: a tool use counts toward
`maxTurns` (past it the turn is stopped and the task is `killed` with verdict `budget-exceeded`), an approval request
turns it `awaiting-input` until answered, the turn's end completes it with the reply text and usage, and the session
ending first fails it; at `maxWallClockMs` the turn is stopped and the task is `timeout` with the text so far. The
child session is never closed: it stays open, listed and resumable. `POST /api/tasks/{id}/kill`, on any node, marks
the task `killed` and the running node stops the child's turn; nodes coordinate only through the shared database. A
running task's node refreshes its liveness every 30 s; a task whose liveness is 90 s old is failed ("its node stopped
running it"), and a gateway that starts fails its own node's running tasks ("its gateway restarted"). A wait that ends,
`POST ?wait=1` or `GET /wait`, never ends the task (note 69). `TaskWire` adds `node`, `harness`, `model`, `effort`,
`parentSessionId`, `parentTaskId`, `depth` and `childSessionId`, and drops `spec`, `executor`, `executorTarget`,
`origin`, `chainDepth`, `nodeAffinity`, `claimedBy`, `attempt`, `maxAttempts`, `pendingMessage`, `conversationId`,
`sessionKey`, `harnessSessionIds`, `eval`, `evalAttempt`, `lastHeartbeatAt`, `pendingApprovals` and `contextRefs`;
`TaskResult` keeps `verdict`, `summary`, `usage` and `error`; `TaskUsage` and `TaskBudget` drop `costUsd`, `maxUsd` and
`maxTokens`. For one release the retired request fields (`executor`, `executorTarget`, `spec`, `contextRefs`,
`parentTaskId`, `chainDepth`, `budget.maxUsd`, `budget.maxTokens`, and `onApproval` on `/wait`) are accepted and
ignored (a gateway warning names the body fields it ignored); the release after refuses them. Removed: `POST /api/tasks/{id}/steer` and
`/approvals[/{request}]` (the child is steered and its approvals answered as any session), `GET /api/catalog` and
`/api/catalog/{section}` (`GET /api/catalog/agents` stays, now also listing the runtime agents of other online nodes),
`GET /api/outcomes`, and `?wait=1` on `POST /api/sessions/{session}/messages`, which is a `400` pointing at
`POST /api/tasks`. `task.done` on `/api/notifications/ws` gains optional `parentSessionId` and `childSessionId`.
Bodies are `wire::task` in `contract/*.schema.json`.
Lineage and topic: `parentSessionId` names the session that asked. A chat that rh starts
gets `RIVETHUB_SESSION_KEY` in its harness process's environment, as a terminal session already did, so `rh mcp` sends it;
the gateway maps a chat's requested id to its canonical one, and records the parent as `launch` when it knows the
session and `request` otherwise. A parent that is itself a child makes the new task its `parentTaskId` with `depth` one
more; past depth 3 is `409`. The child's `HarnessSessionSummary` carries `parentSessionId` and `taskId`. A child with no
placement of its own is filed by reference under its parent's topic, walking up at most 8 parents: `GET
/api/topics/{topic}/sessions` and `GET /api/topics/filing` list it in that topic with optional `parentSessionId`,
`taskId` and the task's `node` (`TopicSession`), and placing the child yourself takes it out of the parent's topic while
the parent keeps it as a child. The categorizer never files a delegated child.
Permission mode: a child takes its parent's permission mode, the current mode of the
parent's chat on the node that took the request, or a nested child's own; with no parent mode it keeps its agent's
(a CLI provider's `permission_mode`). The mode is set on the child's session when its harness offers it. In
`bypassPermissions` the child's approvals are allowed as they come; in any other mode they wait for you, and the task is
`awaiting-input` meanwhile. Nothing is auto-denied any more.
Late results: when a task finishes and nobody waits on it, and its parent is a live chat on the
node that took the request, that node sends the parent a new turn, "Task <id> finished (<status>) after your
delegate_task call stopped waiting.", the child's summary or error, and the child's session and node, or that no session ran it. A wait that
returns the finished task, or comes back within 3 s of the last one, takes the result instead.
Storage: Postgres migration `0040_tasks_sessions.sql` renames the old `rh_tasks` to
`rh_tasks_archive` (the `ros_tasks` view now reads the archive), kills every archived task that was still open, deletes
the `run-task`, `run-task:*` and `heartbeat-*` jobs, drops `ros_task_outcomes_v`, and creates `rh_tasks` in the
new shape (goal, criteria, budget, status, result, error, usage, target agent, preset, node, harness, model, effort,
working directory and permission mode, parent session, parent task, parent source, depth, child session, requester,
owner, times and liveness), whose trigger sends `rh_task_changed` with the task id on every status change. SQLite schema
25 does the same to the table the adapter kept. Archived `task:<uuid>` conversations keep their filing rule (42).
`delegate_task`: it takes optional `criteria` (strings). `rh mcp` delegates through its own
node's gateway (`POST /api/tasks`, then `GET /api/tasks/{id}/wait` until the task ends or the call's wait does), sending
the `RIVETHUB_SESSION_KEY` of the session that runs it as `parentSessionId`. Its reply ends with
`task <id> · session <child> · node <node>`; a wait that ends answers "[still running]" and the result comes back as a
new turn (see Late results). `list_agents` lists `GET /api/catalog/agents`.
Retired with it: `rh agent` (the gateway runs its node's tasks, and `rh start`
supervises no agent role), the chat-loop executor, `maxUsd` and the pricing table, task evals, scheduled heartbeat tasks,
task steering and the task approval routes. `tasks.pricing`, `tasks.eval` and `runtime.heartbeats` load with a warning that names
them and are ignored.
(75) Session deletion (Phil, 2026-10-27): deleting a session removes its memory and hides it from the session lists;
the harness's own transcript on disk is kept, so resume keeps working. `DELETE /api/sessions/{session}` takes the
base64url session segment (as the placement route does) or the percent-encoded canonical `harness:native` id. A session
the user cannot see, or one with neither a harness store row nor memory rows, is `404 {"error":"not found"}`; a live
session (a chat, a summary that has not ended, or a running PTY) is `409 {"error":"session is live"}` and is never
stopped. Otherwise every agent's conversation for that session key is deleted in one transaction, under the capture
lock: its tags, deferred wiki notes and held entity candidates, its summaries (sources and wiki extractions cascade),
its placement, suggestion, startup context, history mark and session filing corrections, then the conversations
(messages, chunks and conversation filings cascade); wiki provenance, citations and topic decisions are left. The
answer is `{"ok":true,"session","conversations","messages","summaries"}`. A tombstone (`rh_session_tombstones`,
migration 0039; SQLite schema v24) is written in the same transaction and holds only until the session is used again.
While it holds, both session lists omit the session unless its harness store row was updated after `deleted_at` (a
resumed session is listed again; the read does not lift the tombstone), capture and append drop batches whose messages
are all at or before `deleted_at` (a message without a timestamp counts as old; logged at debug, answered `ok` with
nothing inserted), and the summarizer's 15-minute `deleted-session-sweep` (`purge-deleted-sessions`) deletes rows that
writers outside rh add later. A capture or append batch with any message newer than `deleted_at` lifts the tombstone in
its transaction and is captured whole; the sweep lifts the tombstone instead of purging when memory holds a message
created after `deleted_at`. Tombstones that cannot be read leave the lists showing every session, logged at warn.
`POST /api/sessions/delete` takes `{"sessions":[ids]}` (1 to 200 after removing duplicates) and answers
`{"results":[...]}` with one deleted body or `{"session","ok":false,"error":"session is live"|"not found"|"delete
failed"}` per session; one session's failure never fails the batch. `GET /api/sessions/trash` answers
`{"candidates":[{"session","harness","title","updatedAt","reason","userTurns","reply"}]}`. It considers the sessions the
lists show and, because the store listers leave out sessions that never had a user turn (such as the ones model
discovery opens), every session remembered with no messages (at most 500, newest first) whose harness store still
describes it on this node and whose harness this node runs. A candidate is visible, not tombstoned, not live, not a
child session, last updated over an hour ago, holds at most 3 messages in memory (memory reads no further, and reads
the filings and citations below only for a session within that bound), has no tool rows, attachments, task, topic or
conversation filing, or wiki-cited summary, its transcript reads (an unreadable transcript, or a memory key that is not a canonical session id,
is never a candidate), and both memory and the transcript agree that it is `empty` (no user or assistant turn, and a
title that does not name the session under note 72, or is its id) or a `one-word-reply` (one user turn of at most 280
characters, at most one assistant turn whose non-blank text is a single word once whitespace and punctuation are
trimmed). Transcripts are read up to 32 at a time; the remembered sessions are described in one pass over each
harness store; summaries and wiki provenance are indexed by conversation (migration 0041; SQLite schema v26 indexes
wiki citations and provenance). Nothing is deleted without a request naming it. The bodies are `wire::gateway::session_delete` in
`contract/*.schema.json`; the verdict is `kernel::memory::trash_verdict`. The routes are new, with no TypeScript
recording.
(76) Grok capture through rh (Phil, 2026-10-30): `rh capture hook --harness grok-build` captures a Grok Build session from its hook command, replacing the rivetOS grok kit's capture worker. On `Stop`, `PreCompact` or `SessionEnd` (`hook_event_name`; any other event is ignored) it reads the session's whole `updates.jsonl` under the Grok home, with no size cap, maps it as the Grok store reader does, including the turn still running, and posts one batch through `POST /api/capture`: agent `rivet-grok`, channel `grok-build`, session key `grok-build:<session>`, the title the session list shows for it (a manual title, else Grok's generated title, else its session summary; none while untitled, so (72) applies), settings `{source: "grok-build-hook", session_id, cwd, last_event, last_ingest_at}` with the payload's `cwd` (the filing folder rule now applies to Grok sessions), and `finalize` on `SessionEnd`. A session whose log holds no message yet posts nothing, so a session start or a model discovery run no longer leaves an empty conversation. The Grok store reader's event ids are now the kit's, `grok-build:<session>:<ordinal>`, so capture appends to the conversations the kit wrote without duplicating them: the ordinal is `turn × 1,000,000 + sub`, where a `user_message_chunk`'s turn is its `promptIndex` (else the current turn, at least 0) and its sub counts that turn's user chunks from 0, and any other update's turn is the order of its `promptId` among the log's distinct prompt ids (else the current turn, -1 before any) and its sub is 10,000 plus its 0-based line. A line that is not JSON takes no ordinal and moves no turn, as in the kit. Failed posts spool as (B3) capture does; the command always exits 0.
(77) Session context (Phil, 2026-10-30): `HarnessEvent` gains `context`,
`{ "type": "context", "sessionId", "used", "window"?, "source" }`: how many tokens a chat's context holds and, when the
agent says, the size of its window. With `source` `harness` it is the ACP agent's own `session/update` `usage_update`
(`used`, and `size` as `window`; a `size` of 0 sends none), forwarded as each one arrives; claude-agent-acp, OpenCode and
dsh send one every turn, and claude-agent-acp sends another after a compaction with the tokens it kept. An agent that has
sent no `usage_update` in its process gets one with `source` `last-call` at each turn's end, when its prompt result's
`_meta` names its last model call (`inputTokens`, which counts cached input, plus `outputTokens`, as Grok Build sends);
`window` is the `_meta.totalContextTokens` that the harness's latest `session/new` (a chat's own, or model discovery's,
for a resumed chat) gave that call's `_meta.modelId` in `models.availableModels`, and is absent when it gave none. A
turn's summed `usage` is never counted as context, and no window is assumed: a client shows `used` without a share when
`window` is absent. A `models` that is not ACP's model state fails `session/new`. The gateway keeps each chat's latest `context` and sends it after the
`transcript` a session socket opens or resyncs with. `turn-complete` gains `modelMs`, the milliseconds the model spent on
the turn as the gateway saw its agent's events: from the prompt to the turn's end, less each wait between a tool call or
permission request and the tool's result; it is absent when no time passed or the turn ran in a terminal. A client's
tok/s is the turn's `usage.completionTokens` over `modelMs`, so a turn whose events arrive late or all at once keeps its
real pace. Bodies are `wire::harness::HarnessEvent` and `wire::events::ContextUsageSource` in `contract/*.schema.json`.
(78) Summary scheduling (Phil, 2026-10-30): every cron entry's job (`idle-enqueue`, `stale-compaction-sweep`,
`reap-dead-jobs`, `topic-page-sweep`, `deleted-session-sweep`, `unembedded-sweep`, `unchunked-sweep`, `tidy-sweep`,
`wiki-backfill`, `stale-wiki-sweep`) and every `sync-topic-page` job is enqueued at priority -100
(`kernel::queue::MAINTENANCE_PRIORITY`), ahead of every work job, so a backlog never holds a sweep back; enqueueing a
queued sweep again updates its priority. A `compact-conversation` job ranks by its conversation's latest message when it
is enqueued: within 24 hours, -20 (idle, threshold or explicit) or -10 (stale); older, 0 or 10. The idle and stale
sweeps read the conversations that hold unsummarized messages in one query (id, unsummarized count and latest message
time, newest first, with no message bodies) and enqueue in that order. Both stores claim by priority, then `run_at`,
then insertion order; memory-sqlite keeps non-zero priorities in `rh_job_priorities` (SQLite schema v27), so the
recorded `ros_jobs` table is unchanged. One conversation is compacted by one job at a time: on Postgres each
`compact-conversation` job joins the graphile named queue of its key (`compact-<conversationId>`), so one enqueued while
another runs waits for it; SQLite keeps one row per key.
(79) Session driver (Phil, 2026-10-30): a session's Chat (ACP) and Terminal (the harness's TUI) take turns holding it,
and `HarnessEvent` gains `driver`, `{ "type": "driver", "sessionId", "driver"?, "busy", "queued", "approval"? }`, sent on
the session's stream each time one of them changes and, held per session, after each `transcript` a session socket
sends. `driver` is `chat` or `terminal`, whichever holds the session now, and is absent while neither does; `busy` says a
turn is running as rh knows it (a chat turn in flight, or a prompt submitted in the terminal whose end has not been
reported); `queued` is the text of each chat message waiting, oldest first. The view you type in takes the session: a
chat message sent while the terminal holds it but is not busy stops the TUI and runs at once; one sent while the
terminal is busy queues until the terminal's turn ends (the harness hook's `turn.end`, B2) or its TUI exits, and is
never dropped. A terminal asked for while a chat turn runs opens when that turn ends, as before. rh counts a terminal
as busy from the first input it forwards to the TUI that carries a carriage return (a keystroke over `/term` or a
submitted `POST /term/inject`) until the hook's `turn.end` or the TUI's exit; a TUI rh just started, or one nobody has
typed a prompt into since its last turn ended, is not busy. The TUI exiting on its own (the person quits it) hands the
session back, so a queued chat message runs then. rh never reads a transcript to decide any of this. Bodies are
`wire::harness::HarnessEvent` and `SessionDriver` in `contract/*.schema.json`.
(80) Terminal turns in Chat (Phil, 2026-10-30): while the terminal holds a session (`driver` `terminal`, note 79), each
harness session socket on it follows the session's transcript, as the store readers in capture read it for
`GET /api/harness-sessions/{session}/transcript`, and sends a `transcript` (`from` 0, the whole read, then the waiting
approvals, the held `context` and the held `driver`) whenever the read differs from the last one it sent: a terminal
turn's prompt, replies, tool calls and results reach Chat as they are written, with or without a hook. It reads a
second after it starts following and after each read that found a change, and while nothing changes backs off to one
read every 2 s while `busy` and every 5 s otherwise; a submitted prompt (`busy` turning true) brings it back to one a
second, and it reads once more when the terminal stops holding the session. A harness that buffers its transcript
shows its turn as late as it writes: Claude Code appends each record about 3 s after it happens. What a read shows is per harness: Claude Code and Grok Build carry messages,
tool calls and their results; Codex, OpenCode, omp, Kimi Code, Hermes and Cursor carry message text only; dsh and
Cowork have no transcript source and send none. Following only displays: it never changes who holds the session.
(81) Terminal approvals (Phil, 2026-10-30): B2 gains the kind `approval.wait`, `{ "v": 1, "session", "type":
"approval.wait", "tool", "args"?, "ts"? }`, which a hook kit sends when the harness's TUI shows a permission prompt
(Claude Code's `PermissionRequest` hook). While the terminal holds the session (note 79), its `driver` event carries
`approval` (`{ "tool", "input"? }`, the hook's `tool` and `args`) until the next event any hook sends for the session,
the next input forwarded to its TUI, the terminal's `turn.end`, or the TUI's exit. A chat client shows it as waiting in
the terminal, to be answered there; it is not an `approval-request` and `POST …/approvals` cannot answer it. On a
session the terminal does not hold, `approval.wait` changes only the session view's `tool`. Only a harness whose hook
reports the prompt shows it: Claude Code through `PermissionRequest`; Codex, OpenCode and omp have no hook or transcript
record of a waiting prompt. Bodies are `wire::events::AgentEventKind` and `wire::harness::TerminalApproval` in
`contract/*.schema.json`.
(82) Opencode capture through rh (Phil, 2026-10-30): `rh capture hook --harness opencode` captures an opencode session, replacing the rivetOS opencode kit's ingester. Opencode has no hook commands, so a plug-in in its config (`~/.config/opencode/plugins/`) runs it on `session.idle` and `session.error` with `{session_id, hook_event_name, cwd}` on stdin; any other event is ignored. It reads every part of the session from `opencode.db` and posts one batch through `POST /api/capture` as (76) does: agent `rivet-glm` and channel `opencode` (the kit's, so conversations it wrote are appended to), session key `opencode:<session>`, the session's opencode title, settings `{source: "opencode-hook", session_id, cwd, last_event, last_ingest_at}`, no `finalize`. Each message's event id is its opencode part id (`prt_…`), as the kit wrote; a reasoning part is an assistant `[thinking] <text>` row, a tool part a `[tool-result] <tool>` or `[tool-failure] <tool>` row (its state's status `error`) carrying the input and output, and step, patch and compaction parts are left out. The Opencode store reader's transcript keeps leaving reasoning out and showing a tool's output. (72) also counts a default title followed by ` - ` and an RFC 3339 time, opencode's `New session - 2026-10-30T19:02:47.323Z`, as a placeholder.
(83) Codex capture through rh (Phil, 2026-10-30): `rh capture hook --harness codex` captures a Codex session from its hook command, replacing the rivetOS Codex kit's ingester. On `Stop` it captures and on `SessionEnd` it captures and finalizes (`hook_event_name`; any other event is ignored). It reads the session's whole rollout (`sessions/YYYY/MM/DD/rollout-<time>-<session>.jsonl` under the Codex home) and posts one batch through `POST /api/capture` as (76) does: agent `rivet-gpt` and channel `codex` (the kit's), session key `codex:<session>`, the title the session list shows, settings `{source: "codex-hook", session_id, cwd, last_event, last_ingest_at}`. Rows are the kit's, one per `response_item`: a user `message`'s `input_text` and an assistant's `output_text` (joined by newlines, trimmed, left out when empty or when they open with `<environment_context>`, `<skills_instructions>` or `<multi_agent_`); a `reasoning` item as an assistant `[thinking] <text>` row (its `text`, else its `summary` and `content` texts joined); a `function_call` or `custom_tool_call` as a `[tool] <name>` row carrying its input; an output as `[tool-result] <name>`, or `[tool-failure] <name>` when it has an `error`, carrying its output, the name taken from its call (`unknown` when its call was not seen). Developer messages and every other line are left out. A row's event id is the item's `id`, else its `call_id`, else `codex:<session>:line:<0-based line>`, and its metadata names the rollout path and that line, so capture appends to the conversations the kit wrote without duplicating them. `tool_args` is an object: an input that is not a JSON object, or a string that does not parse as one, is sent as `{"input": <input>}`, where the kit sent the bare value.
(84) Claude Code capture as the kit keys it (Phil, 2026-10-30): `rh capture hook` writes the rows and event ids the rivetOS Claude kit writes today, so it appends to the conversations the kit captured without duplicating them. On `Stop`, `SubagentStop` or `SessionEnd` it reads the whole transcript (no size cap) and folds each `user` or `assistant` line that is not `isMeta`: its text blocks joined by newlines and trimmed (a string content as it is), else `[thinking] <its thinking blocks>`, else `[tool call] <names>` when it calls tools, else its tool results. An assistant line whose content does not open with `[tool call]` is an assistant row; every `tool_use` is a `[tool call] <name>` tool row carrying its input and its result from the transcript; a user line with no tool result is a prompt row. A row's event id is `claude-code:<session>:<line uuid>` (`…:tool:<tool_use id>` for a tool), or the occurrence id when the line has no uuid, and its metadata names the transcript and its 0-based line. A `UserPromptSubmit` posts nothing: its prompt is held under `<state>/claude-pending/<session>.jsonl` and the next turn end posts it as `claude-code:<session>:hook:<ingest key>` only when no prompt row of the transcript carries it, so a message Claude Code delivers outside the transcript (a cross-session message, a task notification) is still captured once. `PostToolUse` is unchanged (`…:tool:<tool_use_id>`), which also captures a subagent's tool calls that its own transcript holds. Before, turn ends kept only tools that had a result and no thinking, read at most 8 MB, and a prompt was `…:u<n>`. The recorded TypeScript hook batches predate the kit's current ids and are kept as format samples only.
(85) Kept out of the trash (Phil, 2026-10-30): `PUT /api/sessions/{session}/keep` (the session segment as note 75's `DELETE` takes it, no body) records that the caller kept the session when the trash review offered it, and answers `204`. A session the caller cannot see, or one with neither a harness store row nor memory rows, is `404 {"error":"not found"}`; keeping it again moves `kept_at` to the latest keep. The keep (`rh_session_keeps` `{session_key, kept_at, kept_by}`, migration 0043; SQLite schema v29) holds while the session's harness store row was last updated at or before `kept_at`: `GET /api/sessions/trash` leaves such a session out, and a session used again after its keep is judged as before, so it returns only if it is still obvious trash. A keep changes nothing else: the session lists, capture, the sweep and deletion ignore it. The route is new, with no TypeScript recording.
(86) Task and terminal-turn edges (Phil, 2026-10-30): three amendments to (74) and (79). Terminal parents: when a task's parent session is running in a terminal (rh holds its TUI), the child takes the mode that harness records for the session. Claude Code writes its current mode into the transcript (`permissionMode` on each prompt, and a `permission-mode` line whenever it changes, so it follows `--permission-mode`, `--dangerously-skip-permissions`, `permissions.defaultMode` and a change made in the TUI), and the gateway reads the newest one in the transcript's last 8 MiB (`kernel::transcript::SessionLister::permission_mode`). No other harness's TUI records its current mode where rh can read it, and launch flags miss a change made in the TUI, so a terminal parent of any other harness, or a Claude Code one whose transcript names no mode, gives its child `default`: the child's approvals wait for you, and its agent's own mode is not used. A parent that is a chat keeps (74)'s rule. A TUI that herdr runs now gets the session's `RIVETHUB_SESSION_KEY`, `RIVETHUB_SESSION`, `RIVETHUB_URL`, `RIVETHUB_TOKEN`, `RIVETHUB_CA` and `RIVETHUB_NAME` in its pane, as a direct PTY's always did; before, `rh mcp` in it sent no `parentSessionId`. Unclaimed tasks: a task still `queued` once `createdAt` plus `maxWallClockMs` has passed, because no node started it (its node is gone or not running tasks), becomes `timeout` with the error "no node started it before its maxWallClockMs ran out" and no result. Every node's gateway checks on its 30 s liveness beat (`kernel::tasks::TaskStore::expire`), so such a task ends within 30 s of its deadline, and a node that claims its job later finds it finished and skips it. A task without `maxWallClockMs` waits for a node as before, and a running task's deadline still counts from its start. Terminal turns without a hook: where herdr runs the TUI, rh subscribes to `pane.agent_status_changed` for every pane it starts (an agent pane, or a plain one running the roster's command) and keeps that socket open (it used to close its half, and herdr ended the subscription at once), and a pane whose detected agent is Codex, OpenCode or Claude Code reporting `idle` or `done` after `working` or `blocked` ends the terminal's turn as the hook's `turn.end` does (79): `busy` turns false, a held chat message runs, and `turn-complete` is sent. While such a pane reports `blocked` (a permission prompt or a question waiting on the person) its session needs you: a `session-updated` with `blocked` true, and false once it reports anything else. Those three are the harnesses herdr was shown to bracket a turn for, through a 20 s tool call and an approval or question (Claude Code since 2026-10-31: its `rh capture hook` sends no `turn.end`). herdr reports omp `idle` all through its turns, and Grok Build, Hermes and Kimi Code report their turns through hooks, so their pane status is not used. With direct PTYs, for a TUI rh adopted after a restart, and for every other harness, a held message waits for the hook or the TUI's exit as before; so does one held behind a carriage return that started no turn (a slash command).
(87) Model session titles (Phil, 2026-10-30): a title that names its session under (72) is still weak when it describes
the session instead of the work: it ends with the `…` of a cut summary sentence; it holds a UUID or a date and time
(`YYYY-MM-DD` then `T` or a space, then `HH:MM`); it is `Session ` and one word holding a digit or a colon (a session key,
`Session task:<uuid>`); or every word other than a bare number is a framing word or a harness name word (`Kimi Code
session start`, `Session Lifecycle`; the framing words are (72)'s, which also skips them as headings, plus activity,
bootstrap, initial, interaction, lifecycle, resumption and termination). A conversation needs a title when its title is
missing, does not name its session, or is weak; heartbeats, Grok Bot sandbox runs and test channels (the conversations
filing never files) are left alone. When the summarizer (B13) stores a conversation's first leaf and the title left
after (72) still needs one, it asks its own model once, with the leaf (at most 8,000 characters), temperature 0 and the
strict JSON schema `{"title": string}` as `response_format`, for a title of at most 8 words naming the work. The answer
is used only when it has at most 8 words, names the session and is not weak, and is stored only while the stored title
still needs one, so a title a person or a harness set since is never replaced; an unfit answer or a failed call keeps
the title and never fails the summary. `rh memory backfill-titles --model <name> --model-url <url> [--concurrency <n>]
[--dry-run] [--json]` (B9; Postgres only, concurrency 2 by default, 180 s per call) does the same for every summarized
conversation that needs a title, from its first three leaves, and reports `{dry_run, candidates, titled, pairs, unfit,
failed}`, each pair `{id, before, after}`; `--dry-run` asks the model and writes nothing. The rule is
`kernel::memory::{weak_title, needs_title, wants_model_title, fit_title}`.
(88) Owners (Phil, 2026-10-30; tenancy plan phases 1 and 2): every captured row records its owner, the caller the gateway authenticated for the request: a bearer token's user, a device certificate's user mapped through `users.json`, and a plug-in or loopback caller as the node's owner (`ownerUserId`). On a personal node loopback is that person, so `rh capture hook` (Claude Code, Grok Build, opencode and Codex), which posts to its own node over loopback, captures as the node's owner; what loopback means on a shared household node (the space) is decided with the node layout. `POST /api/capture` and the `memory_append` and `memory_ingest_session` tools pass the caller to the memory port (`MemoryStore::capture(batch, owner, node)`, `append(owner, batch)`); no body names an owner. `/events` keeps feeding live sessions under its caller and writes no memory. A conversation keeps the owner of its first capture and its messages take that owner, so a later caller appending to it does not take it over; the TypeScript runtime let a routed capture adopt an unowned conversation, and no conversation is unowned now. Derived rows take their source's owner: chunks their message's, summaries and conversation placements their conversation's, and wiki extractions, citations, provenance, entity candidates and deferred notes their summary's. A session placement takes the owner of the session's captured conversation, else whoever placed it (the route's caller, or the person approving a Tidy proposal); a delegated session's inherited placement takes its task's owner. A topic takes its creator (the route's caller, the Tidy approver, or for an entity topic the owner of the summary that promoted it); `type` topics, the seeded skeleton, have none. Postgres migration 0042 makes `owner_user_id` NOT NULL on `rh_conversations`, `rh_messages`, `rh_message_chunks`, `rh_summaries`, `rh_wiki_extractions`, `rh_wiki_citations`, `rh_wiki_provenance`, `rh_wiki_entity_candidates`, `rh_wiki_deferred`, `rh_session_placements` and `rh_conversation_placements`, adds it to `rh_topics` (required unless `kind` is `type`), and gives existing rows the store's owner: the `rh.store_owner` setting when set, else the one owner its conversations already carry; a store with rows and no single owner refuses until `rh.store_owner` names one (a connection URL ending `?options=-c%20rh.store_owner%3D<user>`). Rows derived from a conversation that already had another owner take that owner. `rh_conversations` and `rh_messages` keep the store's owner as their column default, so the direct-to-Postgres capture kits keep capturing until they move to the capture API. SQLite schema v28 does the same for `ros_conversations` and `ros_messages` and adds `owner_user_id` to `rh_topics`, `rh_session_placements`, `rh_conversation_placements`, `rh_wiki_entity_candidates` and `rh_wiki_deferred`; the recorded `ros_*` tables keep their schema, so a SQLite summary or wiki row's owner is its conversation's, and a SQLite store with no single owner takes the registry's default owner id, `owner`. Nothing reads by owner yet: who can see what is unchanged.
(89) Visibility and row-level security (Phil, 2026-10-30; tenancy plan phase 3): every owned row is private to its owner or visible to a space. Spaces come from `users.json` `spaces` (`{"<id>": {"kind": "household" | "team" | "compartment", "name", "members": [user ids]}}`); without that section a node has one `household` space holding its owner and every listed user. A visibility is `private` or a space id. A session's visibility is its owner's own mark (`PATCH /api/sessions/{session}/visibility` with `{"visibility": "private" | "<space>" | null}`, `null` clearing the mark), else the first explicit mark walking up its topic's tree (`PATCH /api/topics/{topic}/visibility`, same body), else the owner's default (`PUT /api/me/settings` with `defaultVisibility`) when the session started at or after the default was chosen, else private; a mark into a space the owner is not a member of stays private, and only an item's owner marks it (403 otherwise). So existing data, and every session started before its owner chose a shared default, is private until shared. A topic's visibility is its own mark, else its ancestors', else its creator's default under the same rule; seeded `type` topics are visible to everyone. Messages, chunks and summaries inherit their conversation's visibility by reference: the Postgres policies admit them when the viewer owns them or can see their conversation, so a mark takes effect in one row and a private session never feeds a row another user can read; a shared session in a private topic shows to the space without its placement. Wiki notes (extractions, citations, provenance, entity candidates, deferred) are their owner's only. Wiki pages carry an owner and a visibility and stay private to the store's single owner until per-scope pages exist (a shared article built only from shared notes, plus a private overlay per user; not built yet). A space member reads shared rows and writes only their own. `GET /api/me` returns the caller, their spaces and their settings (`chosen` is false until they pick a default); `GET /api/sessions/{session}/visibility` returns a session's visibility, own mark and owner; topic records carry `owner`, `visibility` and `visibilityMark`, and topic session listings carry `visibility` and `visibilityMark`. Enforcement: Postgres migration 0044 turns on row-level security, forced on the table owner, for conversations, messages, chunks, summaries, the wiki tables, topics, topic aliases and decisions, both placement tables, session marks, user settings and spaces. rh sets `rh.principal`, `rh.spaces` and `rh.maintenance` on every pool checkout from the work's tenancy scope (`kernel::tenancy::Scope`): a gateway request runs as its caller's viewer and an unidentified one as closed; background workers and operator commands run as maintenance; the MCP sidecar, plug-ins, mesh calls and the task runner run as the node's owner; work spawned from a scope carries it. Outside any scope a connection sees and writes nothing. Text search keeps its GIN indexes under row security only when the text-match operators `ts_match_vq`, `textlike`, `texticlike`, `textregexeq` and `texticregexeq` are LEAKPROOF: 0044 marks them when it runs as a superuser and otherwise refuses, naming the statements a superuser runs once per database. Vector search turns on pgvector's strict iterative scan (pgvector 0.8 and later), so a viewer who can see few rows still gets its limit. SQLite (`rh local`) stores marks, settings and spaces and answers the visibility routes but does not filter reads: a SQLite node serves one user. A compartment is a space kind only; per-compartment database roles come later.
(90) Plug-in migrations (Phil, 2026-10-29; done 2026-10-30): with the core at the workspace, memory, delegation,
notifications and tasks (note 50), the surfaces outside it move to plug-ins or go, each checked first against the code of
every client (rh, the desktop, RivetHub web, Electron and Android, rivet-android, the hook kits and plug-ins). Removed, with
no caller: the mic bridge (`GET`/`HEAD /api/audio`, `/api/audio/status` and `/api/audio/health`, the `/api/audio/mic`
socket, their `/audio` aliases, the `RIVETHUB_AUDIO` switch and `<state>/audio/mic.pcm`), `GET`/`POST /layout` and
`GET /api/capabilities`, with their `wire` types (`AudioStatus`, `AudioHealth`, `AudioBackend`, `AudioFormat`, the
`AudioMic*` frames, `NodeCapabilities`, `Subsystems` and the `Harness*Capability` types). They answer `404`; `/layout`
and `/audio` stay outside the static fallback, and their B1 recordings stay, skipped. Deprecated: `POST
/api/voice/transcribe` and `POST /api/voice/speak` with the `RIVETHUB_VOICE_*` settings. The voice plug-in
(rivethub-plugins `plugins/voice`, `host.network`) answers the same bodies, limits and errors on `POST
/api/plugins/voice/http/transcribe` and `/http/speak`, its STT and TTS URLs and default instructions being plug-in
settings; the proxy waits 60 s for its first byte (note 55) where `/api/voice/speak` waited 120 s. The two routes are
removed once RivetHub web's composer calls the plug-in. Deprecated and kept while a client calls them: `GET /v1/models`
and `POST /v1/chat/completions`, which rivet-android's native chat sends to a remote node, and the conversation store
they write, `GET`/`POST /api/conversations` and `GET`/`PATCH`/`DELETE /api/conversations/{id}` (filing's
`/api/conversations/{id}/placement`, note 42, is core). Kept: `/api/files/*`, through which Electron and Android
self-update download `builds/rivethub/latest.json` and the build, until updates have a route of their own; and
`/api/hub/state` until Android drops it.
(91) Web panel bridge (2026-10-30, from the desktop's web panel test build): a page posts note 60's messages with
`parent.postMessage(message, "*")`, as from an iframe, and gets the client's as `message` events on its window, the
message being the event's `data`; `source` and `origin` are not part of the contract, so a page tells messages apart by
`type`. A client that shows the page as a top-level document, as RivetHub desktop does, gives it a `parent` whose
`postMessage` reaches the client. The desktop serves each plug-in and node at an origin of its own
(`rhpanel://<host>/`), answering every request through the proxy with the client's credentials, and sends note 60's
policy in place of the `sandbox` policy rh puts on `panel` and `http` answers: `default-src 'self'; style-src 'self'
'unsafe-inline'; img-src 'self' data: blob:; sandbox allow-scripts allow-same-origin allow-forms`. Scripts therefore run
only from files on the plug-in's own routes, never inline; forms submit to script only; pop-ups, downloads, dialogs and
navigating away from the page do nothing. A message longer than 256 KiB, or one that does not parse as `PanelMessage`, is
dropped; `resize` heights are kept between 48 and 2400. Colour roles become CSS names by kebab-casing their contract
names, so `surfaceContainerHigh` is `--rh-color-surface-container-high` and `category1` is `--rh-color-category1`. The
desktop serves the app's own fonts at the page's origin under `/.rivethub/fonts/`, answered by the client and never sent
to the plug-in, with `@font-face` rules for the families `--rh-font-family` and `--rh-font-mono` name, so a page can use
them; `/.rivethub/` is therefore not a path a page can reach on its plug-in. Tab past the page's last focusable element
(Shift+Tab past its first) gives the keyboard back to the client, which moves it on as its own Tab would. A page whose
process stops is started again, twice a minute at most; after that the client shows it stopped and offers to restart it.
(92) DeepSeek Harness capture through rh (Phil, 2026-10-30): `rh capture hook --harness dsh` captures a dsh session, replacing the rivetOS dsh kit's capture worker. dsh has no hook commands, so a Cordis plug-in in the dsh profile runs it on each `turn/end` session event with `{session_id, hook_event_name, cwd}` on stdin; any other event is ignored. It reads the session's whole transcript, `session.<version>.jsonl.zstd` (zstd frames, one per append; a last frame still being written is left for the next turn) or `session.jsonl`, under `$DSH_HOME/sessions/<workspace>/<session>/` (default `~/.dsh`), and posts one batch through `POST /api/capture` as (76) does: agent `rivet-deepseek` and channel `dsh` (the kit's), session key `dsh:<session>`, the last `session/title` as the title, settings `{source: "dsh-hook", session_id, cwd, last_event, last_ingest_at}`, and `finalize`, as the kit finalized at each turn end. Rows are the kit's: a `user/message` from the user (not one whose `source.kind` is another sender) and an `assistant/message` as their text parts joined by newlines and trimmed, reasoning parts as `[thinking] <text>`, left out when empty; a `tool/call` as a `[tool] <name>` row carrying its arguments and, folded in, the text of the `tool/result` with the same call id; a result whose call is not in the transcript as `[tool-result] <name>` or `[tool-failure] <name>` (an `error`, or an `isError` part), the name `unknown` when neither carries one; and `turn/end`, `compaction/start` and `session/title` as system rows `[dsh.<type>] <reason kind, else title, else preset>`. A row's event id is `dsh:<session>:<uuid>`, the event's id, else its data's `id` or call id, else its message's, and `dsh:<session>:seq:<seq>` when it has none, where `<session>` is the transcript header's id; its metadata names the transcript and its 0-based line, so capture appends to the conversations the kit wrote without duplicating them. An event with neither an id nor a seq is skipped (the kit hashed its content; dsh v4 transcripts hold none). `tool_args` is an object as in (83).
(93) `/v1` removed (Phil, 2026-10-30): rivet-android, the only client that sent it, is retired (rivetOS M7: its tree is
deleted and tagged `archive/rivet-android-final`), so the deprecated surface of (90) goes: `GET /v1/models`, `POST
/v1/chat/completions` with its `x-rivet-conversation` and `x-rivet-title` request headers, and the conversation store they
wrote, `GET`/`POST /api/conversations` and `GET`/`PATCH`/`DELETE /api/conversations/{id}`, with their `wire` types
(`ChatRequest`, `Completion`, `CompletionChoice`, `CompletionChunk`, `ChunkChoice`, `CompletionDelta`, `Models`, `Model`,
`ErrorKind`, `ErrorDetail`, `ErrorResponse`, `Conversation`, `Conversations` and `ConversationTitle`). They answer `404
{"error":"not found"}`, and `/v1` stays outside the static fallback. The store kept one JSON file per conversation under
`<state>/conversations/` and no table; rh no longer reads or writes that folder and leaves its files where they are.
Filing's `/api/conversations/{id}/placement` (42) is core and unchanged. A node's configured agents now take turns only
through the mesh agent channel, which amends the last sentence of (11). The CORS preflight still names the two headers in
`access-control-allow-headers`, as B1's recordings do.
(94) Device update routes (Phil, 2026-10-30): RivetHub Electron and Android update from the mesh feed, the folder
`<files_root>/builds/rivethub/` and its `latest.json`, through two read-only routes of their own instead of `/api/files`.
`GET /api/updates/manifest` answers that `latest.json` as it is (`application/json`, `Cache-Control: no-store`), and
`GET /api/updates/{file}` streams one build from that folder (`application/octet-stream` with its `Content-Length`, as an
`attachment`). They admit the callers every B1 route admits (a client certificate the gateway verified, or loopback) and
take no path: `{file}` is one name of ASCII letters, digits, `.`, `_`, `+` and `-` that starts with a letter or digit
(else `400`), and it must be the `file` of an entry in the manifest, an object of platform entries `{"<platform>":
{"file", …}}` where an entry may be `null` (else `404`). The folder must be `<files_root>/builds/rivethub` itself, not
reached through a link, and the manifest and the build are opened without following a link: either is `403`. A manifest
over 1 MiB, or one that does not parse, is `500`; a node without `files_root`, the folder or the manifest is `404`.
RivetHub Electron (`updater.ts`) and Android (`Updater.kt`) move both requests to these routes in one release. Deprecated:
`/api/files/*` and its `/files/*` aliases. The files plug-in (rivethub-plugins `plugins/files`, `host.files.read` and
`host.files.write`) answers their queries, bodies and errors on `/api/plugins/files/http/{list,download,upload,mkdir,
rename,delete}` under the folder its `root` setting names, and RivetHub web's Files page and workflow editor call it
there; through the proxy an upload must be answered within note 55's 60 s, and a download carries the `sandbox` policy.
The routes stay until the installed Electron and Android builds are past the release that moved their updates.
(95) One family store (Phil, 2026-10-30; tenancy plan phase 4): `rh memory merge-store --from <postgres url> --owner <user> [--into <url>] [--moves <file>] [--dry-run] [--json]` copies a second memory database into the family store (`--into`, else `RIVETHUB_PG_URL`) in one transaction, after both are migrated to the same schema. Every copied row takes `--owner`, except the conversations a moves file lists (`<conversation id> <user>` per line, `#` comments), which take that user with every row derived from them; conversations, messages, chunks and summaries keep their ids, so the visibility rules of note 89 apply unchanged and everything merged starts private. A session in both stores under different ids keeps the copy with more messages: the family copy stays when it is at least as long, otherwise the merged copy replaces it and keeps the family copy's owner. A row whose id is already in the family store is the same row and is skipped; an id that names a different row refuses the merge. Seeded type topics join the family's by slug; another topic whose slug is taken gets `-<owner>` appended; an alias already in use is dropped; every merged wiki page's slug gets the prefix `<owner>-`, with citations, provenance, related links, redirects and touched-page lists following. Embeddings of a different size are cleared so the embedder redoes them. Each run is recorded in `rh_store_merges` (`runs`, the rows added per table) and its topic, page and replacement remaps in `rh_merge_map`; a rerun adds only rows the source gained since and changes nothing already merged. Postgres migration 0048: a conversation's or message's owner defaults to the connection's `rh.principal`, else the store's owner, so a direct-to-Postgres capture kit that connects with `options=-c rh.principal=<user>` captures as that user under row-level security; tags carry the owner of what they tag (a trigger fills it), Tidy batches carry an owner, and tasks, tags, Tidy batches and proposals, `rh_store_merges` and `rh_merge_map` are under forced row-level security (owner only, or maintenance). `rivethub-migrate` runs as maintenance. Workers on a shared store: filing runs as the conversation's owner, so the categorizer sees only topics that owner can see and files nothing into another person's private topic; Tidy proposals and topic-page sync run as the node's owner (`ownerUserId`); the wiki builder writes each summary's notes to pages of that summary's own scope (note 96). `users.json` marks a user `"admin": true` to pass the node-owner checks (agent presets) alongside `ownerUserId`.
(96) Per-person and shared wiki pages (Phil, 2026-10-31; tenancy plan): wiki pages live in scopes, one per person and one per space, recorded in `rh_wiki_scopes` (scope, owner, visibility, key prefix, directory). The node owner's private scope is the root of `WIKI_DIR`, as before; another person's private pages live in `WIKI_DIR/users/<user>` and a space's shared pages in `WIKI_DIR/spaces/<space>`, each its own git repository (created on first write), so every scope keeps its own history and the root repository ignores `users/` and `spaces/`. A page row carries its scope, its file name in that scope (`local_slug`) and the owner and visibility of its scope: a private scope's pages are its owner's, a space's pages belong to the space and every member reads and edits them. Page keys stay unique across scopes: the root uses the file name, other scopes prefix it with `<owner>-` or `<space>-`, adding `-2`, `-3` on a clash. A summary's notes go only to pages of the summary's scope: its owner's private scope when the session is private, the space's scope when it is shared. Topic pages live in their topic's scope (owner and effective visibility); a note from a private session filed under a shared topic goes to the session owner's own page for that topic (same title, created on first use), never to the shared page, and a note from a shared session under a private topic goes to the topic's private page. Entity and direct patches resolve only against pages of the same scope, so a private fact never updates a shared page. Postgres migration 0049 gives every existing page a scope: pages merged from another store (note 95) become that person's pages under their original file names, so Coco's existing wiki repository, moved to `WIKI_DIR/users/coco` with its history, is her personal wiki; every other page is the node owner's root scope, and a store with unmerged pages of more than one owner refuses. Every scope's pages follow note 100's writer rules: notes and rewrites take the slug's advisory lock and the scope's writer lock and re-read the page, budgets apply per page, and what a write drops is archived in that scope's own repository (`<scope directory>/archive/<file>.md`); `GET /api/wiki/{slug}/archive` and `recompile-wiki` read and write the page's own scope. Pages written before a session changed visibility stay where they were written. The summarizer model stays one per worker: per-owner models would need per-job provider selection, so a merged person's summaries use the worker's model. SQLite is unchanged.
Naming: no bare "RivetHub clients" anywhere — the clients are RivetHub web / Android / Electron; the product is RivetHub.
(97) Flagged jobs (Phil, 2026-10-30): a job that keeps failing leaves the queue as a flagged record, so the jobs behind it
run. Postgres counts every failure of a job in `rh_job_failures` under its key (`job:<id>` without one), so the count
survives re-enqueueing a key, which resets graphile's attempts; a job is flagged when its count reaches
`RIVETHUB_FLAG_AFTER_FAILURES` (default 5), when it fails its last allowed attempt, or when its key is already flagged.
A retry-later answer (a throttled or unreachable model) pauses the job without counting a failure, and `extract-wiki`
writes no failed extraction for it. Flagging deletes the job and writes `rh_flagged_jobs` (migration 0047): task, key, payload, priority, max attempts,
failures, last error, first and last failure. A job that later completes under the same key clears its count and its
flagged record. The hourly `reap-dead-jobs` sweep flags every job already out of attempts or past the count, instead of
reviving dead `compact-conversation` jobs; `run-task` jobs are never flagged (tasks keep their own failed state). The wiki
backfill and `stale-wiki-sweep` skip summaries whose `wiki-ext-` key is flagged, dismissed or not. `GET
/api/memory/flagged` answers `{ groups: [{ task, cause, count, lastFailedAt, sample }] }`, newest failure first: open
records grouped by task and cause (the error's first line with uuids as `…` and digit runs as `N`), `sample` the group's
newest record `{ id, task, key?, subject?, payload, error, failures, firstFailedAt?, lastFailedAt, flaggedAt }`, `subject` the title of what the job worked on: the conversation its payload's `conversationId` names, or that holds its `messageId` or `ros_messages` target, else the wiki page of a `ros_wiki_topics` target. `POST
/api/memory/flagged/{id}/retry` deletes the record and its count and enqueues the job again with fresh attempts; `POST
/api/memory/flagged/{id}/dismiss` hides it (a dismissed key stays flagged for the sweeps that skip it, and a new failure
shows it again). Both answer `204`, `404` for a record that is not open, and the three routes are the node owner's
(`403` otherwise, `503` without the store). The upkeep `failing` counts (44) add each task's open records. SQLite has
no new tables: a job is dead at its last attempt or the fifth, `reap-dead-jobs` marks queued jobs past five attempts
dead, its dead jobs are the flagged records (no `firstFailedAt`; retry requeues the row, dismiss deletes it), and
re-enqueueing a key still revives its dead job. Wiki order: `stale-wiki-sweep` re-adds missing leaves newest first, and
one summarized within the last 24 hours is enqueued at priority -10 instead of 10, so new sessions reach the wiki ahead
of the backlog.
Wiki backfill (Phil, 2026-10-30): `wiki-backfill` runs every minute and keeps a tier of `extract-wiki` jobs at priority
50 (`kernel::wiki::builder::BACKFILL_PRIORITY`), below all new work, so it is claimed only when nothing newer is ready.
It selects leaf summaries outside heartbeat sessions that have no extraction or a failed one (or a done one with pages but
no commit), newest first, skipping any whose `wiki-ext-` key is queued or flagged; a done or skipped extraction, of any
pipeline version, is never re-run. The tier is topped up to one job while new work waits (a runnable `extract-wiki` job
below priority 50 or a `compact-conversation` job below 0) and otherwise to the model's free slots: the worker reads
`vllm:num_requests_running` and `vllm:num_requests_waiting` from `RIVETHUB_WIKI_BACKFILL_METRICS_URL` and allows
`min(RIVETHUB_WIKI_BACKFILL_MAX` (6)`, RIVETHUB_MODEL_SLOTS` (8) `− running)` jobs when nothing waits, one otherwise or
without the URL. In `rh worker` the tier is off unless the worker running `wiki-backfill` has
`RIVETHUB_WIKI_BACKFILL=1`, and until then the cron only settles deferred notes; `rh local` keeps it on, since that is how
a SQLite node finds new summaries. `RIVETHUB_CLAIM_MAX_PRIORITY` caps the priorities a Postgres worker claims, and a
worker without `RIVETHUB_WIKI_BACKFILL=1` defaults to 49, so it never takes the tier. A Codex worker is let onto the tier
by hand (`RIVETHUB_CLAIM_MAX_PRIORITY=50`) only while at least 30% of its weekly quota is left, and set back to 49 below
that.

(98) Memory counts, access counts and said chunks (Phil, 2026-10-31; memory in milliseconds): Postgres migration 0050 keeps per-conversation message counts in `rh_message_counts` (one row per conversation, owner and tool name, `''` for no tool, holding total, tool calls, embedded, queued and failed), maintained by statement triggers on `rh_messages`; `GET /api/memory/stats` sums them instead of scanning the messages, under the same read rule as the messages (the viewer's own rows, or rows of a conversation the viewer can see). Search access counts move to `rh_message_access` and `rh_summary_access`: a search hit upserts its row there, starting from the message's or summary's own `access_count`, which is no longer written, so a search no longer rewrites a message row; ranking and export read the new tables first and the old columns after. A chunk carries `said`, true when its message is a user or assistant message; a vector search that can only return user or assistant messages (the desktop's, which leaves out tool and system) searches chunks through a vector index of said chunks only. The migration itself takes short locks; `rh db migrate` and `rivethub-migrate` then fill the counts for existing conversations in batches of 50 under a short share lock on `rh_messages`, and mark existing said chunks in batches of 1,000. `rh_backfills` names what is still filling; until a backfill finishes, stats scan the messages and vector search uses every chunk. Common text: when Postgres estimates more than 5,000 matches for a text search, the text arm ranks only the 5,000 newest matches (messages by `created_at`, summaries by `latest_at`), breaking equal scores newest first; a search estimated at 5,000 matches or fewer ranks every match, unchanged. SQLite is unchanged.

(99) Bare Claude keys in search and summaries (Phil, 2026-10-31): a conversation recorded before keys named their harness,
a bare uuid on the `claude-code` channel, is the session `claude-code:<uuid>` as filing reads it (note 42). Memory search
answers that session id as a hit's `sessionId`, in Postgres and SQLite, so a client names the harness and opens the
session; any other key is answered as stored. `GET /api/memory/summaries/{session}` finds the conversation stored under
`claude-code:<uuid>` or the bare uuid, as filing's conversation keys do, the most recently active first.
(100) Bounded wiki pages (Phil, 2026-10-31): a wiki page is updated, not appended to, and stays within a size budget:
summary 2,400 characters (as before), article 16 KiB, each topic section 4 KiB and all sections 12 KiB plus what the
article leaves unused of its 16 KiB, shared so no section is cut below 512 bytes (the oldest note sections that are not
a sub-topic's are archived whole and dropped first), History 48 entries of one line each (200 characters), and the
newest 64 entities. Merged rewrite: when `extract-wiki` routes a placed session, `topic` in the
extraction input gains `current`, the bound page's `summary` and `article` cut to budget for a root topic, or
`section`, the sub-topic's section cut to 4 KiB; the prompt asks for the topic's patch as a merged rewrite of that text,
with what the summary says about parts of the topic folded in. That patch replaces the page's summary and article, or
the sub-topic's section, instead of appending (23). The page summary is a stable lead: what the topic or entity is,
then at most one sentence starting "Currently". A merged write (a topic page or an entity merge) keeps the page's
existing description and replaces only that last "Currently" sentence (left out when it would take the summary past
2,400 characters); an update that carries none leaves the summary as it is, an empty summary takes the update's lead, and only recompile-wiki's summary-only call or an edit rewrites the
description. News goes to the article, the sections and History. A patch whose `type` names a known entity with a page is merged into
that page by a second model call that sends only the entity page's summary and article cut to budget plus the patches'
text and answers `{summary, article, history}`; a failed merge falls back to the patch as before. Notes that still
arrive as separate patches, and deferred notes, go first in their section. Lost updates: a merged write carries the
text its model read, and the writer compares it with the page under the writer lock and the slug's advisory lock; when
another write landed in between it rebases, keeping the blocks the other write added, removing the blocks this rewrite
dropped from what it read, and placing its new blocks after the block they followed. Archive: every Postgres page write
(extractions, topic page syncs, edits, consolidation folds) archives what it removes: blocks a rewrite dropped from the
article or a section, text over budget (trimmed from the end at paragraph boundaries; a write never fails for size),
the legacy "Superseded article", "Superseded current state" and "Summary overflow (v7 cap)" History copies (no new ones
are written), History entries pushed past 48, and the long body of an existing History entry when it is shortened. A
block counts as removed only when fewer than 60% of its words (four or more letters, or containing a digit) are still
on the page after the write, and is archived only when no block already in that month's archive holds 60% or more of
them, so a rephrasing, a "Currently" swap or a repeat archives nothing. Archived text is appended to the month's
newest numbered part, `archive/<slug>/<YYYY-MM>.<n>.md`, while the part stays within 64 KiB, and otherwise starts part
`n + 1`, so a write never re-stores a large file; an unnumbered `<YYYY-MM>.md` from before parts is read and served but
never appended to. The month's parts are in the wiki repository, outside `topics/`, each archived text a block under
`<a id="a-<UTC timestamp>"></a>` that names the commit where it was last inline, and is committed with the page. The
page gets at most one History entry a day titled "Archived" that links `/wiki/<slug>/archive/<YYYY-MM>#<anchor>` and names that
commit; History pushed out on its own adds no entry. Archive files are not pages: they have no `rh_wiki_topics` row and
are not embedded, searched or listed. `GET /api/wiki/{slug}/archive/{month}` answers one month as `text/markdown` and
`/wiki/{slug}/archive/{month}` as HTML keeping the anchors (all of the month's files, oldest first); without `{month}`
they answer every month joined, oldest first,
after a pre-month `archive/<slug>.md` if one is left; `404` when there is none. The writers run git with `gc.auto=0` and
`maintenance.auto=false`, so packing never runs inside a worker; it runs outside them (`git gc --auto` on a timer). Repair: `recompile-wiki`
(`{slug, since}`, handled by the wiki builder; key `wiki-recompile-<slug>`, priority 5) restores the page's real History
entries found in its git versions since `since` (one per date and title, newest first, at most 47 so the Archived entry fits; the
rest go to the archive), then rebuilds the page from its newest sources: the summaries the page cites, newest first,
each section from the summaries of sessions placed in that section's topic and the article from the rest (12 KiB and
8 KiB of material per merge call, each summary dated; a section or article with no cited summaries but over budget
merges its newest 8 or 12 KiB instead; at most 32 sections a page), with the first 2 or 4 KiB of the old text as the
current text and the newest material winning; when the summary is empty or anything was merged, a summary-only merge
call (`summaryOnly`) writes the page's lead from the rebuilt page. It writes through the same writer, so the replaced
text is archived; a page that changed meanwhile is rebased onto the newer page: the blocks the other writes added go first in
the article and in each section, the recompiled text after them, their new History entries are kept and the recompiled
summary stands. `rh memory recompile-wiki --since <iso> [--min-bytes <n>]
[--slug <slug>]... [--dry-run] [--json]` queues one job per page at or over `--min-bytes` (default 61440) or per
`--slug`. SQLite (`rh local`) applies the merged rewrite, the rebase and the budgets to its stored pages but keeps no
archive: it has no repository and stores no History.

(101) Coalesced wiki extraction (Phil, 2026-10-31): when an extract-wiki job's summary routes to a topic page (or a
section of one), the wiki builder also claims up to 7 more queued, unclaimed extract-wiki jobs whose summaries are
placed in the same topic and route to the same page and section, highest priority then newest first, while the
summaries stay within 24,000 characters, and sends them in one extraction call: the input carries `summaries`
(`{id, date, content}`, oldest first) in place of the single summary's fields, with the same `topic` and `types`. The
prompt still forbids invented facts, folds what every summary supports into the same patches, keeping each one's
specific names, numbers, versions and paths, lets the newer summary win where they disagree, and writes one history
entry for them together. The result is applied once, under the lead job's summary. Every coalesced summary cites the
topic page (they are all placed in its topic) and gets a `done` extraction row with the same pages and commit; any
other page a patch writes (an entity, or a topic of its own) is cited, with its page citation, source, provenance and
citation row, only by the summaries whose text names the patch's title, slug or an alias, or by the lead summary alone
when none does. The
claimed jobs are completed with it, or released (their attempt not counted) when the call or the write fails. A job
whose summary is already extracted is completed; one that routes elsewhere is released. A summary that routes to no
topic is extracted alone, as before. Each extraction call logs its summary count and prompt and completion tokens.

(102) Lighter memory reads (Phil, 2026-10-31; desktop smoothness): `GET /api/memory/summaries/{session}` and
`GET /api/memory/conversations/{id}/summaries` take `outline=true`, which answers the same DAG with each node's
`content` cut to its first 320 bytes on a character boundary, `truncated: true` on a node that was cut and
`sourceMessageIds` empty; without it the body is unchanged (`truncated` is left out when false). `GET
/api/memory/summaries/{session}/{summary}` answers one summary of that session's DAG whole, as a
`MemorySummaryNode`, and `404` for an id outside it. `GET /api/memory/search` takes `snippet=true`, which answers each
hit's `content` as at most 480 characters around the first query word of three or more characters (or the first 480),
marked with `…` where it was cut; a client that does not show `expanded` should also pass `expand=false`. Search no
longer answers a message or summary twice when it was captured twice: hits with the same source, session, time and
content keep the best-ranked one. `GET /api/topics/{topic}/page` for a sub-topic answers its root's page as a view of
that sub-topic: `markdown` is the page title, its summary and the sub-topic's `##` section (the last one headed with its
name, up to the next `##`), `citations` only those filed in the sub-topic, and `aliases`, `entities` and `sources` empty;
`currentState` and `article` stay the root's, so an edit from the view still edits the whole page. The gateway compresses
responses with gzip or brotli when the request accepts them, except event streams, audio, video, archives, octet streams
and NDJSON.

(103) Summary tree (Phil, 2026-10-31; monster sessions): `GET /api/memory/summaries/{session}?tree=true` answers the
session's DAG a level at a time: `summaries` the top-level nodes (no parent in the DAG), earliest first by `earliestAt`
(else `latestAt`) then id, cut as `outline=true` cuts them, each with `children` (how many nodes roll up into it);
`offset` and `limit` take a window of them (absent, the first and all). `counts` is the whole DAG's
`{ leaves, branches, roots, embedded, top }`, `top` the number of top-level nodes, and `messages` the tally as before.
`GET /api/memory/summaries/{session}/{summary}/children` answers `{ summaries, total }`: that node's children in the
same order, cut and counted the same way, with the same `offset` and `limit`, `total` all its children, and `404` for a
summary outside the session's DAG. Without `tree` the route answers as note 102 says.
(104) Capture event-id index (Phil, 2026-10-31): Postgres 0051 indexes each message's capture event id per conversation
(`rh_messages_event_id` on `(conversation_id, (metadata ->> 'event_id'))` where the id is set), so capture's check for
already-stored event ids probes the index instead of reading every message of a long session. Row-level security lets
it use that index only when `jsonb_object_field_text` (the `->>` operator) is LEAKPROOF: the migration marks it when it
runs as a superuser and otherwise refuses with the statement a superuser runs once per database (as 0044 does for the
text-search operators). On a large store the index can be built first with `CREATE INDEX CONCURRENTLY` under the same
name and definition; the migration then leaves it as it is.
(105) Fresh installs (Phil, 2026-11-01): a new Postgres database needs only the superuser steps and
`rivethub-migrate postgres`. Migration 0052 creates the job queue when the `graphile_worker` schema is absent: the
schema graphile-worker 0.17.3 installs with `--schema-only` (the `job_spec` type; `_private_jobs`, `_private_job_queues`,
`_private_tasks` and `_private_known_crontabs` with their identity sequences, constraints, indexes and row-level
security enabled; the `jobs` view; `add_job`, `add_jobs`, `complete_jobs`, `force_unlock_workers`,
`permanently_fail_jobs`, `remove_job` and `reschedule_jobs`; and its `migrations` rows 1 to 19), the version every
existing install carries and the queue code reads and writes. An existing schema is left as it is, so the migration is a
no-op where a TypeScript install or graphile-worker created the queue. `memory_postgres::SCHEMA_VERSION` is 52.
graphile-worker is MIT licensed (copyright 2019 Benjie Gillam; notice in `crates/migrate/README.md`). A failed
migration names its version and carries the database's message, detail and hint (`applying the postgres migrations
failed at 0044: migration 0044 turns on row-level security, …`); connect, version and backfill failures carry the
database's reason too. B9 and B16 texts that named retired commands: a missing config is `No config found. Write
~/.rivethub/config.yaml (rh local writes a standalone one) or set RIVETHUB_CONFIG` (`… or use --config <path>` from
`rh start`) in place of `rh config init`; the off-loopback TLS refusal asks for the node's holder certificate
(`~/.rivethub/tls/holder.cert.pem` and `holder.key.pem` from `rh mesh enroll`; devices enroll with `rh cert renew`) in
place of `rivet-ca issue-node` and `issue-client`; and the device-certificate page names `rh cert renew` in place of
`rivet-ca.sh issue-client`. The recorded refusal text is translated to the new wording by
`testkit::translate_recorded_product_names`. `rh local`: with no config it writes `gateway.host: 0.0.0.0` only when TLS
for that bind resolves (`RIVETHUB_TLS_CERT` and `RIVETHUB_TLS_KEY`, or an issued node pair under the shared
directory) and `127.0.0.1` otherwise, logging that a LAN bind needs TLS, so a first run on a new machine serves on loopback;
a config that binds off loopback without TLS stops before anything starts, naming the file and the three ways out
(`rh local --no-lan`, `gateway.host: 127.0.0.1`, or the TLS settings). `rh mcp` on a SQLite config (memory backend
`sqlite`) loads it as `rh local` does, opens the SQLite file it names (`memory.sqlite.path`) with the same query
embedder, and serves B7 on stdio only (`rh local` serves the network transports); delegation and plug-in tools reach
the running `rh local` as they reach a gateway.
(106) Long transcripts read back a page at a time (Phil, 2026-11-01): a Claude Code transcript is read in windows of
the JSONL file (`kernel::transcript::TranscriptWindow`): the latest part (its last 8 MiB), from a byte offset to the end,
or the part ending at an offset. A window that starts partway through the file starts at the first prompt in it, read
further back while it holds none, so a window never opens on half a turn; `StoreTranscript::earlier` (replacing
`truncated`) is the offset it starts at, absent when it starts at the top. `GET /api/harness-sessions/{session}/transcript`
takes `before=<offset>` (the page ending there) or `from=<offset>` (from there to the end) and answers `earlier`; the
socket's `transcript` frame carries `earlier` beside `truncatedBefore`, and after its first read the socket keeps reading
from where that read started, so the turns it sends only grow and pages read back before them stay contiguous. A fork's
`from` (`HarnessSessionForkRequest`) is the offset its `prompts` are counted from, the start of the turns the client
holds; without it they count from the latest part, as before. Other harnesses read whole transcripts: `earlier` stays
absent and a `before` read is empty.
(107) Sessions keep their model and follow the terminal (Phil, 2026-11-01): a top-level Claude Code session's listing
names the model of its last reply, read from the 64 KiB tail the listing already reads, and a live chat's own model
(the one it started on or was switched to) wins over it. `GET /api/harness-sessions/{session}`, the harness session list
and a fork's source name a recorded model the way the harness offers it: the offered id that is one of its parts
(`claude-haiku-5-5` is `haiku`), when exactly one is; so a fork starts on its source's model and a client shows it by its
label. The socket's replayed context count (the chat harness's last `context` frame) is dropped once the terminal starts
a turn, which that harness never counts, and the transcript's count stands. After each transcript it reads the socket
replays a `session-updated` with `blocked` true while the session waits on you, as it replays approvals, context and
driver, so a client opened then shows the wait rather than a running turn.

(108) One writer per page route (Phil, 2026-11-01; QA memory sweep): an extract-wiki job whose summary routes to a
topic page leases its route, the page or one sub-topic's section on it, before it reads the route's current text, and
holds the lease through the extraction call and the write. On Postgres the lease is a session advisory lock (key pair
`0x77696b69`, `hashtext(<page slug>[#<topic id>])`) on a connection that is closed when the lease ends, so a worker
that dies frees it. A job whose route another writer holds goes back to the queue without spending an attempt, to run
no sooner than 30 s later; while it waits it can join the next batch for its route, because note 101's coalescing
takes queued jobs whatever their `run_at`, and claiming a job by id ignores `run_at`. A route's queued summaries are
so extracted one batch at a time, each reading what the last one wrote, instead of several writers rewriting one
section from the same text and rebasing onto each other. Rebase (note 100): a block the other write added is kept only
when fewer than 60% of its words (the archive rule's words) are already in this rewrite, so another writer's rewording
of a paragraph this rewrite also rewrote is replaced, not kept beside it; recompile's "their new blocks first" follows
the same rule. Every page write (Postgres and SQLite) keeps one copy of a repeated paragraph in the article and in
each section: a later exact repeat of 40 or more bytes is dropped, and so is a later prose paragraph of 12 or more words
80% of whose words an earlier prose paragraph in the same part already holds (a lead repeated with another "Currently"
sentence); headings are never dropped, and lists, tables, quotes and code only as exact repeats. Summaries: the summarizer's prompts ask for the work itself (the leaf prompt in the third person) and
never a description of the batch: no batch, leaf or message numbers, ids, channel names, timestamps or tool-call
tallies. The leaf summarizer's input shows each tool call's input (first 600 bytes) and output (first 300 bytes) under
its "[tool call] <name>" line, so a batch of tool calls is summarized by what they did. A branch or root summary cut off
at its token limit is retried over half as many children, and a summarize job whose conversation was deleted while it
waited completes without a summary. Stores without concurrent writers (SQLite) take the lease at once. Leads stay put (QA pass 2): a sub-topic section's first paragraph is
written the way a page summary is, keeping its existing description and replacing only its closing "Currently"
sentence; a lead holds one paragraph (a later paragraph that 60% restates the first is dropped), and a "Currently"
sentence that restates the one already there (80% of its words, every number among them) is kept as it was. On
Postgres an update without a merge base no longer appends its summary to the page's (`merge_summary_text`); it goes
through the same stable lead, as SQLite already did. Extraction and merge prompts keep durable knowledge (what was
decided, built, changed, found, broken or fixed, and why) and leave out how a session went about it: clicks and screen
coordinates, screenshot and scratch file names, test-run and wait-loop mechanics, single-run timings, temporary ports,
throwaway hosts and credentials, and session, run, job or batch ids. A leaf summary that still opens with the batch
("The batch began…", "In this batch, …") is reworded before it is stored: the agent's name becomes the subject, or "The
work" when the batch is described rather than acting.

(109) Search hits that hold the match (Phil, 2026-11-01; QA memory sweep): `GET /api/memory/search` with
`scope=messages` and no `mode` is a transcript search and searches text (mode `fts`): every hit is a message that holds
the query (a full-text match, or a literal match when full text finds none), ranked with the query's meaning as
before; a message found by meaning alone is no longer answered there. Other scopes without a mode stay hybrid, and an
explicit `mode` is honoured. On Postgres a text search (`mode=fts`) answers a message whose text is only reasoning (it
starts `[thinking]`) after every message that holds what was said, and ranks only the 5,000 newest matches when
Postgres estimates more, as hybrid's text arm does (note 98). A literal (trigram) match in a search that leaves out tool
rows (`exclude_roles` holding `tool`, or a `role` other than `tool`) looks in message text only: only tool rows carry a
result, and the few older rows that carry one repeat it in their text. A literal with no letters is matched with
`LIKE`, the same match without case folding. Each search arm reads only a hit's identity and ranking columns; the hits
answered read their text afterwards, one query per table. SQLite is unchanged.

(110) Topic changes on the event stream (Phil, 2026-11-01; desktop QA): a decision added, edited or superseded
through `/api/topics/{id}/decisions…`, and a topic changed through `PATCH /api/topics/{id}` (its name, aliases, parent
or working directory), send `{type: "topic.changed", v: 1, topic}` to every unfiltered `/ws` and `/api/events/ws`
socket on that node, `topic` being the topic's id. It names no content: a client reads the topic or its decisions again
through the routes, under their own rules. Writes through another node's gateway to a shared database send nothing
here. The body is `wire::gateway::topics::TopicChangedNotice` in `contract/sent.schema.json`.
(111) Summary jobs keep their place in line (Phil, 2026-11-01; queue review): the idle and stale sweeps, which run when
a summarizer starts and every 5 and 15 minutes, and a compaction that queues its own follow-up, arm
`compact-<conversationId>` instead of replacing it (`JobKeyMode::Arm`). A waiting job is kept as it is: its `run_at`,
attempts and last error stay, and its priority only rises, taking the new trigger with it, when the new arm is more urgent
(an idle sweep lifts a stale job from 10 to 0 or -20; a stale sweep no longer drops an idle job to 10). A job that is
running gets one queued behind it, and a dead one is revived, as before. A conversation queued since an older sweep
therefore keeps its original `run_at` and is compacted before newer ones of the same priority, where every sweep used to
re-date all of them to the sweep's time, newest conversation first, and the oldest never reached the front. A failing
job keeps its attempts, so it reaches the flagged list instead of being reset by the next sweep. SQLite keeps a waiting
row's `run_at` and raises its `rh_job_priorities` entry the same way. A compaction with nothing to take (fewer messages
than its trigger's floor, too few leaves or branches to roll up) makes no model call, as before.

(112) Sessions after QA pass 2 (Phil, 2026-11-01): a terminal that resumes a session (`POST /term` with `resume`, no
agent) starts in the session's working directory, the live chat's or else the one its harness recorded, when that folder
still exists; an agent launch keeps its agent's folder. The Claude Code capture hook captures only a transcript named
for its session (`<session>.jsonl`, as Claude names them): Grok Build runs the hooks in Claude's settings too, and its
sessions no longer leave a second `claude-code:` conversation of prompts and tool rows. rh's harness discovery starts
each agent with `RIVETHUB_MODEL_RUN=1`, so its probe sessions capture nothing. A Claude turn's usage counts each model
call's output once (Claude repeats a call's usage on every record of its content blocks), and a transcript read carries
`contextUsed`, the context its last model call left (that call's prompt and reply), on the socket's `transcript` frame
and the transcript route; a client's context meter shows it, falling back to the newest turn's prompt and reply.

(113) Failed tasks are told (Phil, 2026-11-01): the `task.done` notification on `/api/notifications/ws` carries the
task's `agent` and, when it has one, its `error`. A client following each node's notifications tells the person when a
delegated task fails or runs out of time, with the agent and the reason, which shows even for a task that never
started a session.

(114) Tidy keeps old names and skips what is moot (Phil, 2026-11-01; QA pass 2): a merge keeps the source's name, with
its aliases, as aliases of the topic it merges into, and a rename keeps the former name as an alias unless the same
change sets the aliases outright (or the two names differ only in case), on Postgres and SQLite alike, so an entity or
topic search, and the wiki's entity matching, still find it by the old name ("TVRT" finds gpubox after "Merge TVRT into
gpubox"). Approving a proposal whose change has nothing left to do sets it aside instead of approving it: a merge whose
topic is gone or whose target is gone, a delete of a topic already gone, a rename to the name it has, a move to the
parent it has, a close of a closed topic, and a type that already exists. A set-aside proposal is stored rejected, so it
is not proposed again, and is not listed as pending. `POST /api/tidy/batches/{batch}/approve` answers
`{ decided, skipped: [{ proposalId, reason }], failed: [{ proposalId, error }] }` (`wire::gateway::tidy::TidySkipped`):
`skipped` holds each set-aside proposal with why in plain words ("the topic it changes is already gone", "it is already
named “g9”", "it is already there", "it is already closed", "the topic it merges into is gone", "the type already
exists"); a proposal approved alone that has nothing left to do answers `409` with the same reason. An approval that
stopped part way is finished by approving again: the changes it already made are skipped as done, the rest applied.

(115) Search hits name their session; catch-up reads one summary (Phil, 2026-11-01; QA pass 2): every hit of
`GET /api/memory/search` (and of the agent and MCP searches, which read the same store) carries `title`, the title of
the hit's session: the conversation's title when it names the session (not a placeholder), else, for a conversation
with no title, the title read from its newest summary, else from its first user message; absent when none names it.
On Postgres the titles are read in one query beside the hits' text; SQLite answers no `title` yet. To open a hit at its
match, a client opens `sessionId` and shows the transcript turn whose `startedAtMs`..`endedAtMs` holds the hit's
`createdAt` (a message hit's `id` is its stored message id); when the transcript read answers `earlier` and the hit is
older than its first turn, the client reads back with `before=<earlier>` (note 106) until a turn holds it.
`GET /api/memory/summaries/{session}?latest=<n>` (`n` from 1 to 50; it takes precedence over `tree` and `outline`)
answers the session's `n` summaries that end last, newest first and a leaf ahead of a rollup ending at the same time,
each whole and without source message ids, with `messages` all zero (no tally is counted). It reads only those rows, so
a session of 5,355 summaries answers in 11 to 26 ms where its outline took 376 to 583 ms and 3.5 MB; a topic's
catch-up reads `latest=1` for each session it shows.

(116) The agent catalog for plug-ins (Phil, 2026-11-01; plug-in QA): `GET /api/catalog/agents`, every agent a task can
be delegated to (note 74), is open to a plug-in holding `agents.read`, so a plug-in that delegates can check an agent name
before it runs. `sessions.read` no longer names `/api/catalog`, which note 74 removed; the bare `/api/catalog` is refused.

(117) A session live in another process is never resumed (Phil, 2026-11-01): before rh resumes a Claude Code session it
does not hold (a turn sent to `POST /api/harness-sessions/{session}/turn` with no live chat, or `POST /term` with
`resume`), it reads Claude's process files (`<claude config>/sessions/<pid>.json`: `pid`, `sessionId`, `procStart`) and
treats the session as running elsewhere when a file names it and its pid is alive with the same start time. Such a
request is refused with 409 `session_running_elsewhere` ("This session is running in another terminal…") and starts no
process. `GET /api/harness-sessions/{session}` answers `runningElsewhere: true` for it, and its socket follows the
transcript as it does a terminal rh runs, so a client shows it read-only and live. A session rh holds (a live chat or
its own terminal) is unaffected.

(118) A model server that cannot be reached is an outage (Phil, 2026-11-01; failing-jobs triage): a model call that
fails to connect at all (refused, no route to host, the name not resolving, the connection timing out) is
`ModelCallFailure::Unreachable`, and every worker treats it as note 97's retry-later answer: the job is held and retried
as the server comes back, spending no attempt and counting no failure, where before only a refused connection did and
a datahub network drop on 2026-10-31 failed and flagged 30 jobs (20 wiki extractions in one minute). A call that
connected and then broke stays a transport failure and fails the job as before.

(120) Remote capture with a device identity (Phil, 2026-11-01): `rh capture hook` posts to another machine's gateway when
`memory.capture.gateway` is set: `url` (the gateway's `https://` address), `ca` (a PEM bundle that verifies the gateway's
certificate), and `cert` and `key` (a PEM client certificate chain and its private key). All four are required. The hook
trusts only `ca`, presents `cert` and `key` on every request, refuses a `url` that is not HTTPS, and does not use the
node's own `gateway` section; without `memory.capture.gateway` it posts to its own gateway as (88) describes. The remote
gateway attributes the batch to the user `users.json` maps the certificate's device to, as (88) describes for device
certificates, so a machine that runs no node and holds no database credential captures as its user. The certificate
carries one URI SAN, `spiffe://rivet.mesh/device/<user>/<device>`: it is issued for a one-time token on the `devices`
provisioner whose subject and SAN are that URI, redeemed with `rh cert renew --principal <uri> --token-file <token>
--once`, and `rh cert renew` without a token keeps renewing it. A batch the remote gateway does not take spools as (B3)
capture does.

(121) A database login pinned to one person (Phil, 2026-11-01; tenancy plan section 7, personal nodes): a personal node
connects to the family store with its own Postgres role, and the store itself decides who that role is. Postgres
migration 0053 adds `rh_role_principals (role_name, user_id)`, readable by every login and written only by the table's
owner or a superuser. For a login listed there (the pinned role; `session_user`, so neither `SET ROLE` nor a SECURITY
DEFINER body moves it) the three scope resolvers every policy and owner default already call return the mapped user as
`rh_scope_principal()`, that user's rows of `rh_space_members` as `rh_scope_spaces()`, and false from
`rh_scope_maintenance()`, whatever `rh.principal`, `rh.spaces` or `rh.maintenance` the connection set; a role not
listed keeps note 89's settings unchanged, so service logins and workers behave as before. A pinned login therefore
reads its own rows and the rows shared to spaces it really belongs to, writes only rows it owns (a capture naming
another owner is refused; a capture naming none takes the pinned user), and cannot widen itself by calling
`rh_elevate()`. The trigger and refresh functions that elevate (every function calling `rh_elevate()` and the four
triggers that call `rh_refresh_*`) run SECURITY DEFINER as the migration's role with `search_path = pg_catalog,
<schema>, pg_temp`, so a pinned login's own writes still refresh visibility across owners and keep `rh_message_counts`,
and the three `rh_refresh_*` helpers are not callable by PUBLIC; `rh_scope_maintenance()` is false whenever
`current_user` is a pinned role, and false for a pinned login unless `current_user` is the owner of `rh_elevate()`,
so `SET ROLE` into some other role the login belongs to does not lift the pin and only the store's own code elevates. Two
policies change: `rh_space_members` shows a login its own memberships (and everything under maintenance) rather than
every member of its spaces, and `rh_message_access` and `rh_summary_access` take a write for any message or summary the
writer can read, so a pinned node's search access counts land. A pinned node's `sync_spaces` is a no-op with one info
line (the store's admin owns spaces), `rh` commands on it act as the pinned person, and a connection outside any scope
sees that person's rows rather than nothing. Operator steps are in OPERATIONS.md, Person logins. Guardian access for
children is not built; it will be a further policy term, not a change to the pin.

## 4. Gaps to close before building the affected slice

- MCP `tools/list` and sample-call recording (B7); CLI golden transcripts (B9); ACP conformance runs against installed
  binaries (B12). Each is a recorder under `tools/` plus files under `golden/`, recorded from the TypeScript runtime.

Naming (Phil 2026-10-12): the single-node install with an embedded database is "standalone" (not "laptop mode") in every
Rust identifier, log line and document. The CLI command `rh local` keeps its name because it is part of the B9 contract.


B14 implementation policy: the PKI adapter limits token issuance and signing separately to sixty attempts per minute per
CA, counts rejected signing attempts, permits only unambiguous unreserved SPIFFE path components, and refuses a certificate
lifetime extending beyond either CA certificate. Principal revocation also invalidates pending enrollment and future renewal
for that principal. Imported legacy CNs confer no authorization; the gateway's later migration verifier owns registry mapping.
The adapter writes its signed CRL inside CA state and atomically publishes the same public output at the configured compatibility path.
