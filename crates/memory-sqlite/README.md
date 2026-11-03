# memory-sqlite

`SqliteStore` is the standalone SQLite adapter for the kernel memory ports. It opens a file that `rivethub-migrate sqlite` created or upgraded to `SCHEMA_VERSION` (30) and refuses an older one. Every statement runs on `spawn_blocking` through `with_conn`.

The store implements `kernel::memory::MemoryStore`, `EmbeddingStore` and `SummaryStore`, the `JobQueue` (`port.rs`), and the topic, topic-page, decision, filing, tidy, wiki-builder, tenancy, session-deletion and task ports that `rh local` composes.

## What the schema does not have

`EmbeddingStore` accepts messages, summaries, and wiki topics. It refuses `EmbedTable::MessageChunks` and any `StoredEmbedding` whose `chunks` are not empty. The schema has no `ros_message_chunks` table. Those calls return `SqliteError::MissingTable` (`schema v7 has no ros_message_chunks table`), which the port maps to `EmbeddingStoreError::Query`.

`StoredEmbedding.content_hash` is not stored. `ros_messages`, `ros_summaries`, and `ros_wiki_topics` have no `content_hash` column. A successful write still stores the little-endian `f32` vector, sets `embed_status` to `done`, clears `embed_error`, and sets `embed_failures` to 0.

When `ros_meta` has `embed_dims`, a vector of a different width is `EmbeddingStoreError::Width`. A store built `with_embed_model` (the `rh local` composition passes the configured SQLite embedding model) also checks `embed_model`: the first write records the model and the vector width, a write while `embed_model` names another model is `EmbeddingStoreError::Model` and leaves the row unchanged, and `/api/memory/health` reports the mismatch as unavailable embeddings. Without a configured model the write accepts the vector and does not insert a meta row.

`SqliteStore::reembed` (`rh memory reembed [--dry-run] [--sqlite <file>]`) runs in one transaction: it clears every stored vector except rows marked `unembeddable`, replaces every embed job that is not running with one per message, summary and wiki topic, and deletes `embed_model` and `embed_dims` so the next write records the configured model and width.

## Search

Schema v8 adds `rh_messages_trigram` and `rh_summaries_trigram`, with backfill and insert/update/delete triggers. The existing `ros_*` tables remain the B5 compatibility schema. Apply the migration with `rivethub-migrate sqlite` before opening the store; the migration preserves a versioned backup.

Inject `QueryEmbed` through `SqliteStore::with_query_embed`. It accepts the kernel `ModelProvider`, a query instruction, a timeout, and a shutdown token. Queries use the kernel input composer and normalize the returned vector before scoring little-endian float32 BLOBs. Malformed stored vectors return an error. Missing or failed query embedding produces an explicit degraded notice; cancellation stops the search.

Hybrid search interleaves message and summary candidates within each arm, preserving SQLite's recorded source balance. The kernel supplies reciprocal-rank fusion, the summary bonus, recency/importance boost, and relevance gate. Exact literals use the trigram index as an additional arm. Short prose is excluded from hybrid candidates; tool results remain eligible. Explicit vector mode ranks by dot product and uses the shared composite score. Unconfigured search retains the recorded BM25 behavior and degraded notice.

Explicit trigram mode uses word-padded, case-insensitive set similarity with the Postgres `> 0.3` threshold. It scans the trigram virtual tables to preserve short-word and word-boundary matches that an internal-trigram MATCH alone would miss. Vector search likewise scans stored vectors; neither path is an approximate nearest-neighbor index.

REGEXP uses the Rust regex crate with a 1 MiB compiled-expression limit and bounded DFA cache, case folding, and newline matching. Invalid or oversized expressions are typed errors. PostgreSQL-specific regex extensions outside the regex crate's syntax are rejected; Unicode case folding follows the regex crate rather than the database server's locale.

`search_at` receives its calendar zone explicitly. The `MemoryStore` port uses UTC. The composition root can wire the query embedder through the builder without coupling store adapters.

## One-time Postgres import

`rivethub-migrate sqlite --path memory.sqlite --import-pg-dump memory.pg.sql`
creates a new target schema, or opens an existing target at the current schema, then imports a data-only, plain pg_dump from
Postgres 16 or 18. The command prints a JSON report with row counts for every memory
table. Import refuses non-empty targets and rolls back all data, index entries and
the completion receipt on any error. Dump SQL is never executed. Only supported
COPY blocks and the pg_dump envelope are accepted.

For an existing embedded Postgres configuration, set `memory.import_pg_dump` to the
dump path. The existing `rh local` preparation provisions the schema through
`migrate`; a pending PGlite configuration without an explicit SQLite path uses
`~/.rivethub/memory.sqlite`. The local composition imports before binding listeners
or starting workers and sidecars, and
records completion in the same transaction as the rows. Subsequent starts use that
receipt even if the dump has been removed. An absent dump produces an error with the
exact pg_dump table-selection arguments. A failed import remains pending and can be
retried after correcting the dump. Use the existing install's Postgres connection at
cutover; the Rust runtime does not launch PGlite.

The import API is `SqliteStore::import_pg_dump(path, cancellation)`. It runs blocking
file and database work under the store's task tracker and checks cancellation before
commit. Timestamps use the store's parser; vector normalization and byte packing are
shared with the adapter's existing embedding paths. Foreign keys are deferred until
commit so pg_dump table order and summary parent order do not matter.
