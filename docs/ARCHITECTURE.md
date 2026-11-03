# RivetHub Rust runtime — architecture (text twin of the design artifact, for builders)

Design artifact (visual, authoritative): https://claude.ai/artifact/L7cAvh16cYEZDhzPy4FHCd. Contract: CONTRACT.md. Rules: BUILDER-RULES.md.

## Layers and the dependency rule

| Layer | Crates | May depend on |
|---|---|---|
| foundation | `values` (the value types kernel and wire share: `SessionId`, `HarnessId`, the free-form JSON holders `Extra` and `JsonObject`, the `Millis` time format, a conversation's `Message`, `Role`, `ThinkingLevel` and `LlmUsage`, and the plug-in names `PluginId`, `ToolName` and `RouteName`), `wire` (serde types for every boundary payload; re-exports the `values` types at its own paths), `config` (config.yaml + env → `Config`) | external crates only; `wire` also uses `values`, and `config` uses `wire` for the shared closed sets (`HarnessId`, `ThinkingLevel`, effort) |
| kernel | `kernel` (ports = traits, domain types; no I/O), `testkit` (dev-only: replayers, fakes, stubs) | values, config — NEVER wire: the kernel owns its domain types, the gateway and adapters translate to and from `wire`, and the cargo-deny ban on `wire` leaves the kernel out (PATTERNS `KERNELWIRE`) |
| adapters | `memory-postgres`, `memory-sqlite`, `providers`, `harness-acp`, `terminal`, `mcp`, `capture`, `pki`, `stepca` | kernel, wire, config, externals — NEVER another adapter or a service, except `pki` → `stepca`: `stepca` cannot depend on kernel, so the certificate-authority port is implemented in `pki` |
| services | `gateway` (with the session registry, mesh, task engine and plug-in host), `agent-loop`, `workers` | kernel, wire, config — NEVER an adapter |
| operations | `migrate` (every Postgres and SQLite schema change; `cli` owns the `rivethub-migrate` operator binary) | wire, config, externals (sqlx, rusqlite); depended on only by `cli` as a normal dependency and by tests as a dev-dependency — NEVER directly by a runtime crate (`rivethub` is checked transitively by the xtask MANIFEST rule, except for the direct `cli` → `migrate` edge used for schema verification) |
| composition | `cli`, `rivethub` (binary; one composition root per service process) | everything |

The gate (`cargo xtask check`) fails any Cargo.toml that breaks this.

## Ports (kernel traits) and who implements them

| Port | Methods (shape) | Adapters | Callers |
|---|---|---|---|
| `MemoryStore` | capture(batch, owner, node) · append(owner, batch) · search(query) · browse(filter) · stats · get_full(id) · tags(action) · context(session) | memory-postgres, memory-sqlite | gateway, agent-loop, workers, mcp |
| `EmbeddingStore` | load(target) · write(embedding) · mark_unembeddable(target, reason) · record_null(target) · mark_permanent(target) | memory-postgres (`rh_messages`, `rh_summaries`, `rh_message_chunks`, `rh_wiki_topics`); memory-sqlite (`ros_messages`, `ros_summaries`, `ros_wiki_topics`; no chunks) | workers |
| `TopicStore` | create(topic) · update(id, changes) · merge(source, into) · find(id or slug, following merge redirects) · list (with session count and last activity) · redirects · place(placement) · unplace(session) · placement(session) · sessions(topic) · suggest(suggestion) · suggestion(session); search is a provided method over list and redirects. The domain types and rules (slug folding, one placement per session, the categorizer never overrides your placement, sub-topics one level deep, merge plans, the first-leaf categorizer verdict) live in `kernel::topics`; adapters apply them inside one transaction, and a placement clears the session's suggestion | memory-postgres, memory-sqlite (`rh_topics`, `rh_topic_aliases`, `rh_topic_redirects`, `rh_session_placements`, `rh_session_suggestions`) | gateway, workers (the summarizer categorizes a session when its first leaf summary is stored) |
| `TopicPageStore` | topic_page(slug) · page_taken(slug) · save_topic_page(topic, page) (writes and binds) · redirect_page(from, to) · fold_page(child, into, mark). The sync rules (page per root, sub-topic sections, hand-off on reparent, fold on merge) live in `kernel::topics::sync_topic_page`; the route's page lookup is `kernel::topics::bound_page` | memory-postgres (the wiki git page, frontmatter `sections`, `rh_wiki_topics.topic_id`), memory-sqlite (`ros_wiki_topics`, `rh_topic_pages.sections`) | workers (`sync-topic-page`; `extract-wiki` through `kernel::wiki::route`) |
| `DecisionStore` | decisions(topic) · decision(id) · keep_decisions(batch) (upserts and queues `sync-topic-page` for each topic in one transaction). The `Decision` entity and its rules (non-empty text, only an active decision is edited or superseded, a replacement belongs to the same topic) live in `kernel::topics`; `sync_topic_page` renders a root's and its sub-topics' decisions as the page's `Decisions` section; a merge moves the source's decisions to the target | memory-postgres (`rh_topic_decisions`, migration 0030), memory-sqlite (`rh_topic_decisions`, schema v17) | gateway (`/api/topics/{topic}/decisions`), workers (`sync-topic-page`) |
| `StartupStore` | keep_startup(session, context) · startup(session). A session's startup context is assembled by `kernel::topics::startup_context` (topic page, recent sessions in the topic, relevant hits) and budgeted by the pure `kernel::topics::assemble`, rendered for delivery by the pure `kernel::topics::render`, and kept with how it was delivered; the token estimate and `relative_when` are `kernel::memory::budget` | memory-postgres (`rh_session_startup`), memory-sqlite (`rh_session_startup`, schema v16) | gateway (`POST /api/harness-sessions` and `POST /api/terminal` with a topic, `GET /api/harness-sessions/{session}/startup-context`) |
| `SessionDeletion` | delete_session(session key, by) → counts (writes the tombstone) · tombstones(keys) → deleted_at (a tombstone is lifted when the session is used again) · session_activity(keys) (turn, tool, filing and citation facts for the trash review, read no further than `TRASH_MESSAGES_MAX` messages) · empty_sessions(limit) (remembered with no messages) · purge_tombstoned(limit) · keep_session(session key, by) and kept(keys) → kept_at (a keep holds until the session is updated after it). The trash rule is the pure `kernel::memory::trash_verdict` over kernel-owned turns; the gateway translates transcripts and the contract's trash reason | memory-postgres (`rh_session_tombstones`, migration 0039; lookups indexed by migration 0041; `rh_session_keeps`, migration 0043), memory-sqlite (`rh_session_tombstones`, schema v24; lookups indexed by schema v26; `rh_session_keeps`, schema v29) | gateway (`DELETE /api/sessions/{session}`, `POST /api/sessions/delete`, `GET /api/sessions/trash`, `PUT /api/sessions/{session}/keep`, list filtering), workers (`purge-deleted-sessions`) |
| `JobQueue` | task_names(role) · enqueue(job) → id · claim(selector) (by priority, then run_at) · read · subscribe · complete/fail · revive_dead | memory-postgres (graphile tables), memory-sqlite (`ros_jobs`, priorities in `rh_job_priorities`, schema v27) | memory adapters, workers, tasks |
| `ModelProvider` | chat(request) → response · stream(request) → chunk stream (text, reasoning, tool-call deltas, usage) · embed(texts) · list_models | providers: ONE OpenAI-compatible client (vLLM, SGLang, hosted OpenAI-compatible APIs; no sampling fields on the wire) + cli-backed over a headless ACP session; no native Anthropic/Gemini/xAI clients | agent-loop, workers, memory search (query embedding) |
| `HarnessAdapter` | new_session(spec, with the rendered startup context) · system_prompt(harness) → `SystemPromptSupport` · resume(id) · prompt(id, input) · cancel(id) · answer_permission(id, decision) · set_option(id, model/effort) · switches(id) · admit(id, option) · events(id) → SessionEvent stream | harness-acp (one ACP client + a per-harness launch table: native ACP, or codex-acp / claude-code-acp, and the `_meta` field each adapter reads to append to its system prompt; the terminal CLI flags are `kernel::harness::cli_system_prompt`) | gateway's session registry |
| `TerminalMux` | spawn(session, argv, cwd, size) · attach(id) → byte stream · input(id, bytes) · resize · kill · list | terminal (herdr over a PTY; the only multiplexer) | gateway |
| `Tool` | definition() → `rmcp::Tool` · call(args, ctx) → `rmcp::CallToolResult` (the MCP tool shape, no translation) | RivetHub's own capabilities (memory, wiki, delegation, skills) in mcp; remote MCP servers' tools via the mcp client | agent-loop, mcp server |
| `TranscriptReader` | sessions(store) · read(session) → turns · tail(session, since) | capture's store readers (one per harness) | capture (incremental + backfill), gateway (transcript route) via the binary's wiring |
| `HubStateStore` | load(user) · replace(user, base version, state) → saved or conflict (compare-and-set on `version`) | memory-postgres (`rh_hub_state` on the memory database, so every node on one datahub shares a user's document); gateway's file store under `<state>/hub-state/` when the node has no Postgres or the table is not migrated yet | gateway `GET`/`PUT /api/hub/state` |
| `TenancyStore` | sync_spaces(spaces) · default_visibility(user) · set_default_visibility(user, visibility, now) · session_visibility(viewer, sessions) · mark_session(viewer, session, mark, now) · mark_topic(viewer, topic, mark). Which rows a viewer reads is not a port argument: `kernel::tenancy::Scope` rides with the request or job and the Postgres pool applies it on every checkout (CONTRACT note 89) | memory-postgres (`PostgresTenancy`), memory-sqlite | gateway |
| `SummaryWatch` | written() → a stream of the summaries stored (id, session key, kind, depth) | memory-postgres (`NOTIFY rh_summary_written` from every summary write, listened for by the gateway's process), memory-sqlite (in process) | gateway's plug-in host (`summary.written`) |
| `PluginTools` | tools() → the running plug-ins' tools and a version · call(plugin, tool, call, the MCP request's tool-use id) · changed(seen version) → the next version | gateway's plug-in host in-process (`rh local`); mcp's `SocketTools` over the node's `<state>/plugins/tools.sock` (`rh mcp`) | mcp server (lists plug-in tools after its own and sends `tools/list_changed`) |
| `CertificateAuthority` | issue_token(principal) · sign(CSR, token or verified renewal principal) · chain · crl · revoke(principal) | pki over the stepca HTTP client: one `step ca token` minter shared with `rh ca token`, roster SANs, step-ca chain and CRL, revoke by principal via `step ca revoke`; no local signer | gateway `/ca/v1/`, cli `rh ca` and `rh cert renew` |

A port exists only where several implementations exist or where it is the boundary to an external system. Dropped:
`SessionRegistry` (one implementer, one in-process caller — the registry is a `gateway` module; the agent and task processes
reach sessions over the HTTP API), `Env` (config owns the environment), `Fs` (direct I/O; tests use temp directories),
`Clock` (functions take `now: DateTime<Utc>` as an argument where time is data; timers use `tokio::time`, paused in tests).

### SQLite store gaps

`memory-sqlite` (schema v30) implements the memory, queue, embedding, summary, topic, wiki-builder, tidy, tenancy and task
ports that `rh local` composes. It has no message-chunk table, so `EmbeddingStore` refuses chunked embeddings
(`EmbedTable::MessageChunks`, or a `StoredEmbedding` with chunks) with a typed error and standalone nodes embed without
chunking. Vector search scans the stored vectors; there is no approximate-nearest-neighbour index. It stores visibility
marks but does not filter reads by them (one user per SQLite node, CONTRACT note 89), and keeps no wiki archive or History.
`rh local` arms vector and hybrid search with a query embedder built from `memory.sqlite.embed_endpoint`,
`embed_model` and `embed_api_key` (or the provider `embed_provider` names), falling back to `RIVETHUB_EMBED_URL` / `RIVETHUB_EMBED_MODEL`, the same
endpoint the standalone embedder worker uses. Without an endpoint, those modes answer with keyword results and
`degraded: "embedding endpoint not configured"`. `memory.sqlite.embed_query_instruction` sets the text prepended to
each search query before it is embedded, as `memory.postgres.embed_query_instruction` does for Postgres: absent keeps
the default retrieval instruction, an empty string embeds the bare query (for embedding models trained without one).
With workers on, `rh local` also runs the `enqueue-unembedded` sweep on SQLite: once at start and then every ten
minutes, the embedder queues an `embed-target` job for each message, summary and wiki topic that has no vector and
no embed status (rows captured before an endpoint was configured, or whose last job died), heals rows whose embedding
came back null, and revives a dead `embed-target` job under the same key, as graphile does on Postgres. Cron entries
are only scheduled for tasks the queue claims, so SQLite never schedules `enqueue-unchunked`. The embedder and summarizer
runners of `rh local` also queue every claimed cron entry once when they start, so an idle conversation is summarized
without waiting for the next `*/5` tick; `rh worker` on Postgres keeps the wall-clock schedule only.

### Memory sweep settings

The embedder's `embed-target` and `enqueue-unchunked` share one chunk window: `memory.postgres.chars_per_chunk` (optional,
positive, default 6000 in `EmbedSettings`; the datahub runs 4000). Sweep limits come from the environment:
`RIVETHUB_EMBED_SWEEP_LIMIT` (enqueue-unembedded, default 200), `RIVETHUB_CHUNK_BACKFILL_LIMIT` (enqueue-unchunked, default
200), `RIVETHUB_EMBED_SWEEP_MAX_ATTEMPTS` (default 5), and `RIVETHUB_WIKI_SWEEP_LIMIT` (enqueue-stale-wiki, default 20,
clamped to 0..=200, 0 disables it). The stale-wiki sweep runs only with `RIVETHUB_WIKI_EXTRACTION=1`.

### Wiki topic identity and consolidation

`kernel::wiki::page::topic_identity(slug, title)` is a topic's identity key: the slug's words without the product and project words
(`rivet`, `rivetos`, `rivethub`, `project`, `projects`) or connector words (`the`, `and`, `of`, `for`, `in`, `on`, `to`,
`with`, `via`, `by`), each singularized, sorted and deduplicated. A trailing `-2`..`-9` is a dedupe suffix and is dropped
unless the title carries that number, so `grok-4` stays apart from `grok-3` and `rivet-grok`. Numbers inside words
(`web114`, `web115`) are kept. A slug made only of those words is its own key. Both wiki builders resolve a proposed topic to an
existing one with the same key (after exact, redirect and alias matches) instead of creating a fork.

The wiki builder claims `consolidate-wiki` (payload `{ dryRun?, limitClusters? (50), minClusterSize? (2) }`) on SQLite and
Postgres. `rh local` queues one each time it starts with a wiki directory; on Postgres, `rh memory consolidate-wiki
[--limit <n>]` queues one for the `rh worker --role wiki-builder` process, and `--dry-run` prints the planned folds without
writing. No model is involved. Topics that share an identity key fold into one survivor: the page with the fewest
product, connector and dedupe words, then the most citations, then the longest history, then the oldest. The remaining
topics are then grouped by their first two slug tokens, the group's stem topic (or else its shortest slug) is the parent,
and only true hyphen-boundary variants of that parent are folded. The child slug and its aliases become parent aliases and
redirect rows (served as 308s), redirects that pointed at the child point at the parent, citations and provenance
move to the parent, the denser summary and article win, and the text not kept is appended to the parent's article under
`Superseded parent text` / `Merged from <slug>` headings. On Postgres the merged page file, its history and citations
included, and the removal of each child's page file land in one wiki git commit. A second run finds nothing to fold. A page bound to a topic is never folded as a child, and
it is the survivor of its identity group.

## Process shape (Phil 2026-10-12; `rh agent` retired by CONTRACT note 74)

One process per service, each with one tokio multi-thread runtime built in its own composition root:
`rh gateway` (gateway with its session registry, terminal, devices, the certificate authority, the plug-in host and the
task engine, which claims its node's `run-task:<node>` jobs), `rh worker --role <embedder|summarizer|categorizer|wiki-builder|all>`,
and `rh mcp` (sidecar). `rh start` supervises those roots as children of the same binary and stays hidden from help;
`rh stop` signals that supervisor. `rh local` composes the gateway, the task engine, the SQLite worker roles and network
MCP in one process on one SQLite store.
Processes communicate only through contract boundaries (HTTP API B1,
Postgres queue B4/B13). Inside a process: supervised tasks under one `CancellationToken` tree; `parking_lot` for short state,
`tokio::sync` across awaits, `spawn_blocking` for blocking work; `tracing` for logs.

## Session model (SESSION-MODEL.md, Phil 2026-10-12)

One `SessionId` (serialized exactly as today's session-id strings). Lifecycle new/resume/prompt/cancel/end. One typed
`SessionEvent` stream mapped to ACP `session/update` kinds and emitted to clients in today's v1 wire shape. Chat runs over
ACP only: natively (grok, hermes, kimi, opencode, omp, dsh) or through the codex-acp and claude-agent-acp
adapter binaries; cursor is terminal-only until it has ACP, cowork is captured only, and dsh is chat-only. Terminal = PTY under herdr attached to the same SessionId.
Multi-client harnesses share one agent session; single-client ones have one owner (Idle|Chat|Terminal) and switch by turn-end
handoff (TurnEnded from the protocol or the harness's own Stop hook; queued chat messages delivered after the handoff; never
keystroke injection, never screen scraping). Transcript stores are read only to capture sessions run outside RivetHub.

## Standard crates

axum + hyper + tokio-tungstenite · reqwest (rustls) · sqlx + pgvector · rusqlite (bundled) · serde, serde_json, serde_yaml ·
clap · agent-client-protocol (ACP) · rmcp (MCP) · chrono · regress (user-supplied ECMAScript regex) · tokio::process,
portable-pty · rcgen + rustls-webpki · thiserror · tracing · tokio-util (CancellationToken) · parking_lot · schemars (tool schemas).

## Targets (production lines)

wire 3,000 · config 2,000 · kernel 800 · testkit 1,200 · gateway 8,200 · agent-loop 2,600 · workers 4,000 ·
memory-postgres 5,000 · memory-sqlite 4,000 · providers 900 · harness-acp 1,600 · terminal 2,000 · mcp 2,400 ·
capture 4,500 · pki 1,200 · cli 4,900 · rivethub 500 · migrate 1,000. Total ≈ 50,000 against ≈ 120,000 lines of
TypeScript. Not separate crates: hooks (agent-loop), agent presets (gateway), wiki builder (workers),
harness store readers (capture). Certificate authority signing and policy are in the `pki` adapter behind
`kernel::ca::CertificateAuthority`; CLI only hosts commands and holder renewal. No local-db: standalone runs on SQLite.
Workflows is a plug-in (rivethub-plugins `plugins/workflows`) and reaches rh only over B17. The clients live outside this
repository: the native desktop in rivethub-desktop, and RivetHub web, Android and Electron and the sites under `apps/` in
the rivetOS monorepo. The chat-channel bots and CFO tooling live in their own private repos (rivethub-channels, rivet-cfo). No tools crate: the TypeScript's built-in shell/file/search/interaction/web tools are deleted (unused; every
harness has its own).

## Build order

Wave 0 foundation (wire, config, kernel, testkit; Phil reviews the types) → Wave 1 memory path (memory-postgres, gateway
skeleton, capture with its readers; B3 B4 B6) → Wave 2 sessions (gateway's session registry + session/term routes, harness-acp, terminal; B1 B2
B12) → Wave 3 agent (providers, agent-loop, mcp; B7 B8 B15) → Wave 4 platform (workers, tasks,
mesh, memory-sqlite; B5 B13 B14) → Wave 5 operations (cli incl. CA, standalone on SQLite, update/doctor/service; B9 B10 B11
B16) → cutover. Each wave's gate is a boundary working end to end against the recordings.

Standalone: `rh local` composes the gateway, the SQLite worker roles, and network MCP into one process on the SQLite store (one composition root, same crates). It runs the task engine on the SQLite store (task routes and MCP delegation share it; no mesh executor), and the wiki builder when `memory.sqlite.wiki_dir` is set. `Config::load` with `Profile::Node` still requires runtime, agents, and providers; `rh local` loads with `Profile::Standalone` and does not invent them.
Mesh (registry, heartbeat, agent channel) is a gateway module; ssh helpers are in cli; mesh.json types in wire. 20 crates.
Schema changes never run inside the runtime: `rivethub-migrate postgres|sqlite` applies them (Postgres from the
`migrate` crate's `sqlx::migrate!` chain, SQLite through the v7 creation, the v1→v7 upgrade ladder and the steps to v30, with a
`<file>.v<N>.bak` copy of the original file before any read-write connection), and the stores verify one schema version each
(`memory_postgres::SCHEMA_VERSION`, `memory_sqlite::SCHEMA_VERSION`) and refuse to start behind it.
`cli` owns the `rivethub-migrate` operator binary. Its Postgres-dump import composes
`migrate` for schema provisioning with `memory-sqlite` for transactional data import;
keeping the binary in the composition crate avoids an adapter-to-adapter dependency.
The `migrate` library still owns all schema creation, upgrade ladders and version
checks. Its existing 1,000-line crate budget is unchanged; every file stays under
the 600-line file cap.
No runtime plugin loading: extension points are adapter crates behind ports, the public API, MCP and out-of-process
plug-ins (B17, the gateway's `plugins` module; launches go through its one `launch` function, which confines each
process with Landlock on Linux). `plugins:` and
`channels.*` are accepted-and-ignored compatibility keys.


## Certificate authority (step-ca, 2026-10-15b)

step-ca holds the intermediate. This runtime is a client. Gateway config (`gateway.certificate_authority`) names the HTTPS
URL, the root PEM, the provisioner password file, and the optional operator roster. The composition root connects
`pki::Authority` when that section is present and leaves the gateway without a certificate authority when it is absent.
An empty `~/.rivethub/ca` directory does not start or refuse a local signer.

Holders generate their own ECDSA P-256 keys and submit CSRs. Identity is a SPIFFE URI SAN:
`spiffe://rivet.mesh/node/<node>`, `spiffe://rivet.mesh/agent/<node>/<agent>`, or
`spiffe://rivet.mesh/device/<user>/<device>`. Node and agent names and IP SANs come from the operator roster. Device
pairing uses the device SPIFFE URI as the token subject. `rh ca token device <name>` still uses that operator name as
the subject and only SAN. Both paths call one minter.

`POST /ca/v1/sign` forwards a bearer token, or mints one for the verified principal on renewal. `GET /ca/v1/chain` is
the intermediate from the last sign plus step-ca roots. `GET /ca/v1/crl` is step-ca's CRL. Revoke records the decimal
serial of each leaf this process signed and runs `step ca revoke`. Certificates issued before the process started are
revoked by serial with `rh ca revoke`. step-ca unreachable is `CaError::Unavailable`. There is no local fallback signer,
no sqlite serial store, and no import of a CA directory.

## RivetHub operator and terminal names

B1/B12/B16 operator messages use `[rivethub-gateway]`, `RIVETHUB_TLS_CERT` and `RIVETHUB_TLS_KEY`.
ACP subprocess stderr uses the `rivethub::harness_acp::agent` tracing target. Gateway host and origin guidance
refers to the gateway settings. The YAML section is `gateway`; `den` is still read as a deprecated alias, with a
load-time notice, and setting both is a validation error.
New shell session IDs are `rivethub-<uuid-v4>`. Herdr config homes are `rivethub-<hash8>` under the existing
runtime directory or temporary directory; the hash inputs and socket layout stay unchanged.

Terminal children receive `RIVETHUB_SESSION_KEY`, `RIVETHUB_SESSION`, `RIVETHUB_URL`, `RIVETHUB_TOKEN`
when configured, and `RIVETHUB_CA` and `RIVETHUB_NAME`. Both current and deprecated user identity/token variables
are removed before spawning. B10 deprecated environment aliases remain owned by `config`, including its warning
and current-name precedence. The B12 `denSession` wire key and B1 legacy identity header remain compatibility
boundaries owned by `wire` and tenancy; this slice does not change their protocol shapes.

Recorded expected text is translated only by `testkit::translate_recorded_product_names`, including only the
environment names changed in this slice; other adapters retain their existing boundaries. JSON is parsed into
typed payloads before translating text fields. Actual results are compared without name normalization. Source
recordings remain unchanged. Gateway session replay exclusions are named with reasons in `session_skips`, with
a test accounting for every scenario. Provider, database, capture, mesh schema and config format changes are outside
this naming slice.
