# RivetHub Rust runtime — architecture (text twin of the design artifact, for builders)

Design artifact (visual, authoritative): https://claude.ai/artifact/L7cAvh16cYEZDhzPy4FHCd. Contract: CONTRACT.md. Rules: BUILDER-RULES.md.

## Layers and the dependency rule

| Layer | Crates | May depend on |
|---|---|---|
| foundation | `values` (the value types kernel and wire share: `SessionId`, `HarnessId`, the free-form JSON holders `Extra` and `JsonObject`, the `Millis` time format, a conversation's `Message`, `Role`, `ThinkingLevel` and `LlmUsage`, and the plug-in names `PluginId`, `ToolName` and `RouteName`), `wire` (serde types for every boundary payload; re-exports the `values` types at its own paths), `config` (config.yaml + env → `Config`) | external crates only; `wire` also uses `values`, and `config` uses `wire` for the shared closed sets (`HarnessId`, `ThinkingLevel`, effort) |
| kernel | `kernel` (ports = traits, domain types; no I/O), `testkit` (dev-only: replayers, fakes, stubs) | values, config — NEVER wire: the kernel owns its domain types, the gateway and adapters translate to and from `wire`, and the cargo-deny ban on `wire` leaves the kernel out (PATTERNS `KERNELWIRE`) |
| adapters | `memory-postgres`, `memory-sqlite`, `providers`, `harness-acp`, `mcp`, `capture` | kernel, wire, config, externals — NEVER another adapter or a service |
| services | `gateway` (with the session owners, the notice channel and the plug-in host), `workers` | kernel, wire, config — NEVER an adapter |
| operations | `migrate` (every Postgres and SQLite schema change; `cli` owns the `rivethub-migrate` operator binary) | wire, config, externals (sqlx, rusqlite); depended on only by `cli` as a normal dependency and by tests as a dev-dependency — NEVER directly by a runtime crate (`rivethub` is checked transitively by the xtask MANIFEST rule, except for the direct `cli` → `migrate` edge used for schema verification) |
| composition | `cli`, `rivethub` (binary; one composition root per service process) | everything |

The gate (`cargo xtask check`) fails any Cargo.toml that breaks this.

## Ports (kernel traits) and who implements them

| Port | Methods (shape) | Adapters | Callers |
|---|---|---|---|
| `AccountLogin` | start device approval · poll local progress · validate/refresh bearer and email · revoke/forget | cli composition over its existing share-server OAuth client and locked login store | gateway account service, CLI updater |
| `MemoryStore` | capture(batch, owner, node) · append(owner, batch) · search(query) · browse(filter) · stats · get_full(id) · tags(action) · context(session) | memory-postgres, memory-sqlite | gateway, workers, mcp |
| `EmbeddingStore` | load(target) · write(embedding) · mark_unembeddable(target, reason) · record_null(target) · mark_permanent(target) | memory-postgres (`rh_messages`, `rh_summaries`, `rh_message_chunks`, `rh_wiki_topics`); memory-sqlite (`ros_messages`, `ros_summaries`, `ros_wiki_topics`; no chunks) | workers |
| `TopicStore` | create(topic) · update(id, changes) · merge(source, into) · find(id or slug, following merge redirects) · list (with session count and last activity) · redirects · place(placement) · unplace(session) · placement(session) · sessions(topic) · suggest(suggestion) · suggestion(session); search is a provided method over list and redirects. The domain types and rules (slug folding, one placement per session, the categorizer never overrides your placement, sub-topics one level deep, merge plans, the first-leaf categorizer verdict) live in `kernel::topics`; adapters apply them inside one transaction, and a placement clears the session's suggestion | memory-postgres, memory-sqlite (`rh_topics`, `rh_topic_aliases`, `rh_topic_redirects`, `rh_session_placements`, `rh_session_suggestions`) | gateway, workers (the summarizer categorizes a session when its first leaf summary is stored) |
| `TopicPageStore` | topic_page(slug) · page_taken(slug) · save_topic_page(topic, page) (writes and binds) · redirect_page(from, to) · fold_page(child, into, mark). The sync rules (page per root, sub-topic sections, hand-off on reparent, fold on merge) live in `kernel::topics::sync_topic_page`; the route's page lookup is `kernel::topics::bound_page` | memory-postgres (the wiki git page, frontmatter `sections`, `rh_wiki_topics.topic_id`), memory-sqlite (`ros_wiki_topics`, `rh_topic_pages.sections`) | workers (`sync-topic-page`; `extract-wiki` through `kernel::wiki::route`) |
| `DecisionStore` | decisions(topic) · decision(id) · keep_decisions(batch) (upserts and queues `sync-topic-page` for each topic in one transaction). The `Decision` entity and its rules (non-empty text, only an active decision is edited or superseded, a replacement belongs to the same topic) live in `kernel::topics`; `sync_topic_page` renders a root's and its sub-topics' decisions as the page's `Decisions` section; a merge moves the source's decisions to the target | memory-postgres (`rh_topic_decisions`, migration 0030), memory-sqlite (`rh_topic_decisions`, schema v17) | gateway (`/api/topics/{topic}/decisions`), workers (`sync-topic-page`) |
| `SessionDeletion` | delete_session(session key, by) → counts (writes the tombstone) · tombstones(keys) → deleted_at (a tombstone is lifted when the session is used again) · session_activity(keys) (turn, tool, filing and citation facts for the trash review, read no further than `TRASH_MESSAGES_MAX` messages) · empty_sessions(limit) (remembered with no messages) · purge_tombstoned(limit) · keep_session(session key, by) and kept(keys) → kept_at (a keep holds until the session is updated after it); unkeep_session(session key) · trash_session(session key, by) (idempotent, the first moment holds) · restore_session(session key) · trashed(keys) → trashed_at · purge_trashed(older than, limit) (deletes through the same path as delete_session). The trash rule is the pure `kernel::memory::trash_verdict` over kernel-owned turns; the gateway translates transcripts and the contract's trash reason | memory-postgres (`rh_session_tombstones`, migration 0039; lookups indexed by migration 0041; `rh_session_keeps`, migration 0043; `rh_session_trash`, migration 0057), memory-sqlite (`rh_session_tombstones`, schema v24; lookups indexed by schema v26; `rh_session_keeps`, schema v29; `rh_session_trash`, schema v31) | gateway (`DELETE /api/sessions/{session}`, `POST /api/sessions/delete`, `GET /api/sessions/trash`, `PUT`/`DELETE /api/sessions/{session}/keep`, `POST /api/sessions/{session}/trash` and `/restore`, list filtering), workers (`purge-deleted-sessions`, also past-retention trash) |
| `ModelCallStore` | record(owner, calls) → inserted and duplicate counts (idempotent on `call_id`: a repeat changes nothing) · session_calls(session) → the session's calls, oldest first. The pure rules live in `kernel::model_calls`: `ProxyHeaders` (names lowercased once, credential headers refused by name), `correlate` (a table with one row per harness: session header, `User-Agent` prefix and, for Codex, the turn-metadata header, naming the `claude-code:<id>` or `codex:<id>` session key) and `CallTotals` (call count, usage sums, errors, per-model counts). Headers never reach the port, so no adapter can store one; a call no row matches is stored with no session key | memory-postgres (`rh_model_calls`, migration 0062, owner-scoped by row-level security), memory-sqlite (`rh_model_calls`, schema v35) | gateway (`POST /api/capture/model-calls`, `GET /api/sessions/{session}/model-calls`) |
| `SessionCatalog` | list(query: view, harness, topic, text, cursor, exclude, limit) → page of `SessionRecord` (one store query, newest start first, stable cursor) · get(session) → record. A record carries the title (derived by the pure `kernel::memory::session_title`), message count, topic path, keep and trash marks, `kernel::memory::memory_state` and the gist. Which rows a viewer reads is the store's scope (note 89); child sessions are excluded by the gateway through `exclude` | memory-postgres, memory-sqlite | gateway (`GET /api/sessions`, `GET /api/sessions/{session}`) |
| `ReviewStore` | marks(owner) · mark(owner, mark). A mark is decided or snoozed until a time, keyed by `kernel::review::ReviewKey`; the queue itself is gathered from the fact, topic, tag and tidy ports at read time, and `kernel::review::fact_reviews` derives conflict and new-fact items from claims | memory-postgres (`rh_review_marks`, migration 0059), memory-sqlite (`rh_review_marks`, schema v33) | gateway `/api/review` |
| `ChangeFeed` | records(window) → change records; changes(window) is a provided method that derives `Change`s through the pure `kernel::changes::changes` (fact events through `kernel::facts::timeline`). `DigestStore`: digest(day, owner) · keep_digest(digest) · request_digest(request) (queues `digest-day`). The Today rules (headline, fallback summary, activity levels, recently updated, digest prompt) are pure functions in `kernel::changes` | memory-postgres, memory-sqlite (`rh_daily_digests`, Postgres migration 0060, SQLite schema v34; `rh_topic_redirects.merged_from`) | gateway (`GET /api/today`, `GET /api/changes`), workers (the summarizer's `digest-day`) |
| `JobQueue` | task_names(role) · enqueue(job) → id · claim(selector) (by priority, then run_at) · read · subscribe · complete/fail · revive_dead | memory-postgres (graphile tables), memory-sqlite (`ros_jobs`, priorities in `rh_job_priorities`, schema v27) | memory adapters, workers |
| `ModelProvider` | chat(request) → response · stream(request) → chunk stream (text, reasoning, tool-call deltas, usage) · embed(texts) · list_models | providers: ONE OpenAI-compatible client (vLLM, SGLang, hosted OpenAI-compatible APIs; no sampling fields on the wire) + cli-backed over a headless ACP session; no native Anthropic/Gemini/xAI clients | workers, memory search (query embedding) |
| `HarnessAdapter` | new_session(spec) · prompt(id, input) · cancel(id) · answer_permission(id, decision) · set_option(id, model/effort) · release(id) · events(id) → SessionEvent stream · offerings | harness-acp (one ACP client + a per-harness launch table: native ACP, or codex-acp / claude-code-acp) | providers (the CLI model providers, started through `harness_acp::HeadlessLaunch`) |
| `Tool` | definition() → `rmcp::Tool` · call(args, ctx) → `rmcp::CallToolResult` (the MCP tool shape, no translation) | RivetHub's own capabilities (memory, wiki) in mcp; remote MCP servers' tools via the mcp client | mcp server |
| `TranscriptReader` | sessions(store) · read(session) → turns · tail(session, since) | capture's store readers (one per harness) | capture (incremental + backfill), gateway (transcript route) via the binary's wiring |
| `TenancyStore` | sync_spaces(spaces) · default_visibility(user) · set_default_visibility(user, visibility, now) · session_visibility(viewer, sessions) · mark_session(viewer, session, mark, now) · mark_topic(viewer, topic, mark). Which rows a viewer reads is not a port argument: `kernel::tenancy::Scope` rides with the request or job and the Postgres pool applies it on every checkout (CONTRACT note 89) | memory-postgres (`PostgresTenancy`), memory-sqlite | gateway |
| `SummaryWatch` | written() → a stream of the summaries stored (id, session key, kind, depth) | memory-postgres (`NOTIFY rh_summary_written` from every summary write, listened for by the gateway's process), memory-sqlite (in process) | gateway's plug-in host (`summary.written`) |
| `PluginTools` | tools() → the running plug-ins' tools and a version · call(plugin, tool, call) · changed(seen version) → the next version | gateway's plug-in host in-process (`rh local`); mcp's `SocketTools` over the node's `<state>/plugins/tools.sock` (`rh mcp`) | mcp server (lists plug-in tools after its own and sends `tools/list_changed`) |

A port exists only where several implementations exist or where it is the boundary to an external system. Dropped:
`SessionRegistry` (one implementer, one in-process caller — the session owners and the notice channel are a `gateway`
module), `Env` (config owns the environment), `Fs` (direct I/O; tests use temp directories),
`Clock` (functions take `now: DateTime<Utc>` as an argument where time is data; timers use `tokio::time`, paused in tests).

### SQLite store gaps

`memory-sqlite` (schema v35) implements the memory, queue, embedding, summary, topic, wiki-builder, tidy and tenancy
ports that `rh local` composes. It has no message-chunk table, so `EmbeddingStore` refuses chunked embeddings
(`EmbedTable::MessageChunks`, or a `StoredEmbedding` with chunks) with a typed error and standalone nodes embed without
chunking. Vector search scans the stored vectors; there is no approximate-nearest-neighbour index. It stores visibility
marks but does not filter reads by them (one user per SQLite node, CONTRACT note 89), and keeps no wiki archive or History.
`rh local` arms vector and hybrid search with a query embedder built from `memory.sqlite.embed_endpoint`,
`embed_model` and `embed_api_key` (or the provider `embed_provider` names), falling back to `RIVETHUB_EMBED_URL` / `RIVETHUB_EMBED_MODEL`, the same
endpoint the standalone embedder worker uses. Without an endpoint, those modes answer with keyword results and
`degraded: "embedding endpoint not configured"`. `memory.sqlite.embed_query_instruction` sets the text prepended to
each search query before it is embedded, as `memory.postgres.embed_query_instruction` does for Postgres: absent uses
the Qwen retrieval instruction when the embed model's name contains `qwen` (any case) and no prefix for any other model,
and an empty string embeds the bare query. A Qwen model served under a name without `qwen` needs the instruction set
explicitly.
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

## Process shape (Phil 2026-10-12; `rh agent` retired by CONTRACT note 74; the memory service cut, note 132)

One process per service, each with one tokio multi-thread runtime built in its own composition root:
`rh gateway` (the gateway with its session owners, notice channel and plug-in host), `rh worker --role
<embedder|summarizer|categorizer|wiki-builder|all>`, and `rh mcp` (sidecar). `rh start` supervises those roots as
children of the same binary and stays hidden from help; `rh stop` signals that supervisor. `rh local` composes the
gateway, the SQLite worker roles and network MCP in one process on one SQLite store.
Processes communicate only through contract boundaries (HTTP API B1,
Postgres queue B4/B13). Inside a process: supervised tasks under one `CancellationToken` tree; `parking_lot` for short state,
`tokio::sync` across awaits, `spawn_blocking` for blocking work; `tracing` for logs.

## Sessions

rh runs no live sessions (CONTRACT note 132); T3 Code does. One `SessionId` (serialized exactly as today's session-id
strings) names a captured session. The gateway reads stored sessions through capture's `SessionLister` and
`TranscriptReader` for every harness id, and keeps which user owns each session in the session owners file. ACP is used
only by the CLI model providers: `harness_acp::HeadlessLaunch` starts a harness, natively or through the codex-acp and
claude-agent-acp adapter binaries, and the provider runs one headless turn on it.

## Standard crates

axum + hyper + tokio-tungstenite · reqwest (rustls) · sqlx + pgvector · rusqlite (bundled) · serde, serde_json, serde_yaml ·
clap · agent-client-protocol (ACP) · rmcp (MCP) · chrono · regress (user-supplied ECMAScript regex) · tokio::process ·
thiserror · tracing · tokio-util (CancellationToken) · parking_lot · schemars (tool schemas).

## Targets (production lines)

wire 3,000 · config 2,000 · kernel 800 · testkit 1,200 · gateway 8,200 · workers 4,000 ·
memory-postgres 5,000 · memory-sqlite 4,000 · providers 900 · harness-acp 1,600 · mcp 2,400 ·
capture 4,500 · cli 4,900 · rivethub 500 · migrate 1,000. Not separate crates: the wiki builder (workers) and the
harness store readers (capture). No local-db: standalone runs on SQLite.
Workflows is a plug-in (rivethub-plugins `plugins/workflows`) and reaches rh only over B17. The clients live outside this
repository: the native desktop in rivethub-desktop, and RivetHub web, Android and Electron and the sites under `apps/` in
the rivetOS monorepo. The chat-channel bots and CFO tooling live in their own private repos (rivethub-channels, rivet-cfo). No tools crate: the TypeScript's built-in shell/file/search/interaction/web tools are deleted (unused; every
harness has its own).

## Build order (history; note 132 later removed the sessions, agent, tasks, mesh and CA work)

Wave 0 foundation (wire, config, kernel, testkit; Phil reviews the types) → Wave 1 memory path (memory-postgres, gateway
skeleton, capture with its readers; B3 B4 B6) → Wave 2 sessions (gateway's session registry + session/term routes, harness-acp, terminal; B1 B2
B12) → Wave 3 agent (providers, agent-loop, mcp; B7 B8 B15) → Wave 4 platform (workers, tasks,
mesh, memory-sqlite; B5 B13 B14) → Wave 5 operations (cli incl. CA, standalone on SQLite, update/doctor/service; B9 B10 B11
B16) → cutover. Each wave's gate is a boundary working end to end against the recordings.

Standalone: `rh local` composes the gateway, the SQLite worker roles, and network MCP into one process on the SQLite store (one composition root, same crates). It runs the wiki builder when `memory.sqlite.wiki_dir` is set. `Config::load` with `Profile::Node` still requires runtime, agents, and providers; `rh local` loads with `Profile::Standalone` and does not invent them, and `rh config validate`, `rh status` and `rh doctor` accept such a SQLite config.
16 crates (the mesh, the agent loop, the terminal and the PKI left with note 132).
Schema changes never run inside the runtime: `rivethub-migrate postgres|sqlite` applies them (Postgres from the
`migrate` crate's `sqlx::migrate!` chain, SQLite through the v7 creation, the v1→v7 upgrade ladder and the steps to v35, with a
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


## RivetHub operator names

B1/B16 operator messages use `[rivethub-gateway]`, `RIVETHUB_TLS_CERT` and `RIVETHUB_TLS_KEY`.
ACP subprocess stderr uses the `rivethub::harness_acp::agent` tracing target. Gateway host and origin guidance
refers to the gateway settings. The YAML section is `gateway`; `den` is still read as a deprecated alias, with a
load-time notice, and setting both is a validation error.
B10 deprecated environment aliases remain owned by `config`, including its warning and current-name precedence.

Recorded expected text is translated only by `testkit::translate_recorded_product_names`, including only the
environment names changed in this slice; other adapters retain their existing boundaries. JSON is parsed into
typed payloads before translating text fields. Actual results are compared without name normalization. Source
recordings remain unchanged. Recorded cases for removed routes are named in the gateway parity skips with CONTRACT
note 132 as the reason. Provider, database, capture and config format changes are outside this naming slice.
