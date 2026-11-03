# Operating RivetHub on Postgres

For whoever runs the Postgres memory database (the datahub) and its workers. A standalone `rh local` node needs none of
this; its notes are at the end.

## The database

rh connects as an ordinary role that owns the database and its tables, never as a superuser. Row-level security is
forced on the owned tables (CONTRACT note 89), so the owner is subject to it like any other role; rh sets `rh.principal`,
`rh.spaces` and `rh.maintenance` on each connection from the work it is doing.

A new database needs two superuser steps, once per database, and then its migrations:

1. **Extensions:** `vector` and `pg_trgm`. pgvector 0.8 or later lets vector search scan on until a viewer who can see
   only some rows still gets a full page of results.
2. **LEAKPROOF operators.** Under row-level security Postgres uses an index for a condition only when its operator
   is LEAKPROOF. Migration 0044 needs the five text-match operators, and 0051 needs `->>`. Both steps, run as a
   superuser in the database (`psql -d <database>`):

   ```sql
   CREATE EXTENSION IF NOT EXISTS vector;
   CREATE EXTENSION IF NOT EXISTS pg_trgm;
   ALTER FUNCTION ts_match_vq(tsvector, tsquery) LEAKPROOF;
   ALTER FUNCTION textlike(text, text) LEAKPROOF;
   ALTER FUNCTION texticlike(text, text) LEAKPROOF;
   ALTER FUNCTION textregexeq(text, text) LEAKPROOF;
   ALTER FUNCTION texticregexeq(text, text) LEAKPROOF;
   ALTER FUNCTION jsonb_object_field_text(jsonb, text) LEAKPROOF;
   ```

   A migration that runs as a superuser marks the operators itself; one that does not refuses at 0044 or 0051,
   naming the statements.
3. **The schema:** `rivethub-migrate postgres` (next section). It also creates the job queue the workers and tasks
   use: graphile-worker's `graphile_worker` schema at the version every existing install carries (0.17.3), created by
   migration 0052 when the database has none and left alone when it has one.

## Migrations

```sh
rivethub-migrate postgres --url "$RIVETHUB_PG_URL"     # or --config FILE (its connection_string, else RIVETHUB_PG_URL)
rivethub-migrate status postgres                       # current, required, pending; read-only
```

Run it before starting any service on a new database and after every upgrade; `rh update` runs the new release's
`rivethub-migrate` itself before swapping binaries. Services never migrate: each verifies the schema version it needs
(52 at 0.7.0) and refuses to start behind it. `rh doctor` and `rh db status --url <url>` report the same status.
Migrations run as maintenance, so they see and change every row. They are forward-only: rolling back a binary does not
undo them.

When a migration fails, `rivethub-migrate` names it and prints the database's message, detail and hint, including any
statement a superuser has to run:

```text
rivethub-migrate: applying the postgres migrations failed at 0044: migration 0044 turns on row-level security, and
search keeps its text indexes under it only when these operators are LEAKPROOF: … A superuser marks them once per
database, for example: ALTER FUNCTION ts_match_vq(tsvector, tsquery) LEAKPROOF; and the same for each listed
function. Then run the migration again.
```

**Store owner (0042).** Every row has an owner. On an existing store whose conversations already have more than one
owner, 0042 refuses until the connection names the owner of the older, unowned rows:
`postgres://…/memory?options=-c%20rh.store_owner%3D<user>`.

**Event-id index (0051).** 0051 builds `rh_messages_event_id` inside the migration, which locks `rh_messages` for writes
while it builds. On a large store, build it first without the lock, as the owner, then migrate; the migration finds it
and leaves it as it is (the LEAKPROOF mark on `jsonb_object_field_text` is still required):

```sql
CREATE INDEX CONCURRENTLY IF NOT EXISTS rh_messages_event_id
    ON rh_messages (conversation_id, (metadata ->> 'event_id'))
    WHERE (metadata ->> 'event_id') IS NOT NULL;
```

## Backups

**Postgres.** `pg_dump` turns row-level security off for its reads, and a role that is subject to forced row-level
security then fails on the first secured table ("query would be affected by row-level security policy"), even as the
table owner. Dump as a superuser:

```sh
sudo -u postgres pg_dump -Fc -d memory -f memory-$(date +%F).dump
```

Restore as a superuser too, into a database owned by the same role (`createdb -O <role> memory`, then
`pg_restore -d memory memory-<date>.dump`). The dump carries the extensions and the `graphile_worker` schema but not
the LEAKPROOF marks, which belong to each database's catalog: run the six `ALTER FUNCTION` statements above on the new
database before starting rh, or search runs without its indexes.

**Wiki.** The wiki is git: `WIKI_DIR` (default `/rivet-shared/wiki`) and, with several people, a repository per person
under `users/<user>/` and per space under `spaces/<space>/`, which the root repository ignores. Back up each, for
example with `git -C <repo> bundle create <file>.bundle --all`.

**Node state.** `~/.rivethub/` on each node: `config.yaml`, the users file, `plugins/registry.json` (installed
plug-ins, their grants and settings) and `plugins/<id>/data/`, and `tls/` (the holder's key and certificate; the key
never leaves the holder, so a lost one means enrolling again).

## Workers

Run one `rh worker --role <role>` process per role (`embedder`, `summarizer`, `categorizer`, `wiki-builder`) as its own
systemd unit next to Postgres. Each worker's model is its configuration's `runtime.default_agent`, so give a role its
own config file when it should use a different model. A unit:

```ini
[Unit]
Description=RivetHub worker summarizer
After=network.target postgresql.service
Wants=postgresql.service

[Service]
Type=simple
User=rivethub
EnvironmentFile=/etc/rivethub/worker.env
Environment=RIVETHUB_WORKER_CONCURRENCY=2
ExecStart=/usr/local/bin/rh worker --role summarizer --config /etc/rivethub/summarizer.yaml
Restart=always
RestartSec=5
MemoryMax=2G

[Install]
WantedBy=multi-user.target
```

`worker.env` holds `RIVETHUB_PG_URL`. `rh worker --role <role> --check` validates a unit's configuration and
environment without connecting. On SIGTERM a worker releases the jobs it holds back to the queue, their attempts not
counted, and exits; work still running after a ten-second grace is logged and the exit is non-zero.

**Memory limits.** rh itself stays small (a few hundred MiB); the limit is there to keep one runaway job from taking the
host. One datahub's limits, as a starting point: categorizer 1G; embedder, summarizer and wiki builder 2G at a
concurrency of 1 or 2; a wiki builder with backfill on at a concurrency of 4, 6G. Git packing is kept out of the wiki
builder's cgroup (below).

| Variable | Default | Effect |
|---|---|---|
| `RIVETHUB_WORKER_CONCURRENCY` | 4 | jobs a worker runs at once |
| `RIVETHUB_WIKI_EXTRACTION` | off | `1` is required by `wiki-builder` and `all`, and turns on the 15-minute stale-wiki sweep |
| `RIVETHUB_WIKI_BACKFILL` | off | `1` lets this wiki builder keep a tier of backfill `extract-wiki` jobs at priority 50, below all new work |
| `RIVETHUB_WIKI_BACKFILL_METRICS_URL` | none | a vLLM `/metrics` URL; backfill then fills the model's free slots instead of one job |
| `RIVETHUB_WIKI_BACKFILL_MAX`, `RIVETHUB_MODEL_SLOTS` | 6, 8 | backfill jobs at most, and the model's slots |
| `RIVETHUB_CLAIM_MAX_PRIORITY` | 49 without backfill | the highest job priority this worker claims |
| `RIVETHUB_FLAG_AFTER_FAILURES` | 5 | failures before a job is flagged out of the queue |
| `RIVETHUB_SUMMARY_LEAF_TOKENS`, `_BRANCH_TOKENS`, `_ROOT_TOKENS` | 16384, 24576, 32768 | output budget of each summary request |
| `RIVETHUB_EMBED_TIMEOUT_MS` | 30000 | each embedding request |
| `RIVETHUB_EMBED_SWEEP_LIMIT`, `RIVETHUB_CHUNK_BACKFILL_LIMIT` | 200, 200 | rows per embedder sweep |
| `RIVETHUB_EMBED_SWEEP_MAX_ATTEMPTS` | 5 | attempts before the embed sweep gives up on a row |
| `RIVETHUB_WIKI_SWEEP_LIMIT` | 20 | summaries per stale-wiki sweep (0 to 200; 0 turns it off) |

`memory.postgres.chars_per_chunk` (default 6000) sets the embedder's chunk window.

**The queue.** `rh memory queue-status` shows each task's pending, running, scheduled and dead jobs;
`rh memory retry-failed --task <task>` and `rh memory requeue --task <task>` put jobs back (`--dry-run` first). A job that keeps failing is moved to `rh_flagged_jobs` so the jobs behind it run; the node owner sees
them in the client's Memory Stats tab (`GET /api/memory/flagged`) and retries or dismisses each. A throttled or
unreachable model pauses a job without counting a failure.

## Wiki packing

The wiki writers run git with `gc.auto=0` and `maintenance.auto=false`, so packing never runs inside a worker (where it
would count against the worker's memory limit and could get the unit killed). Pack on a timer instead, under its own
limit. A script that packs every scope's repository:

```sh
#!/bin/sh
set -e
for repo in /rivet-shared/wiki /rivet-shared/wiki/users/* /rivet-shared/wiki/spaces/*; do
  [ -d "$repo/.git" ] || continue
  git -C "$repo" -c gc.auto=1000 -c gc.autoDetach=false -c pack.threads=2 -c pack.windowMemory=256m gc --auto --quiet
done
```

Installed as `/usr/local/bin/rh-wiki-gc`, it runs from a oneshot unit and a timer:

```ini
# rh-wiki-gc.service
[Unit]
Description=Pack the wiki git repositories outside the wiki workers

[Service]
Type=oneshot
User=rivethub
Nice=10
IOSchedulingClass=idle
MemoryMax=3G
ExecStart=/usr/local/bin/rh-wiki-gc
```

```ini
# rh-wiki-gc.timer
[Unit]
Description=Pack the wiki git repositories every 30 minutes

[Timer]
OnCalendar=*:0/30
RandomizedDelaySec=120
Persistent=true

[Install]
WantedBy=timers.target
```

`systemctl enable --now rh-wiki-gc.timer`. `gc --auto` does nothing until loose objects pass `gc.auto`.

## Several people on one store

- List the people and their devices in the users file (`gateway.users_file`; README, Tenancy). Rows captured over a
  device certificate or a bearer token take that person as owner; loopback, plug-ins and `rh mcp` act as the node owner.
- A capture kit that writes to Postgres directly names its person in the connection:
  `?options=-c%20rh.principal%3D<user>`.
- `rh memory merge-store --from <url> --owner <user> [--moves <file>] [--dry-run]` copies another memory database into
  this one in one transaction, private to that person; both must be migrated to the same version first. A rerun adds
  only what the source gained.
- Each person's wiki pages go to `WIKI_DIR/users/<user>/`, created on first write.

### Person logins

A personal node that writes to the store directly gets its own Postgres role, pinned to its person by the store
(CONTRACT note 121). Whatever `rh.principal`, `rh.spaces` or `rh.maintenance` that login sets, it reads its own rows and
the rows shared to spaces it belongs to, writes only its own, and never enters maintenance. Roles not listed in
`rh_role_principals` keep the connection settings, so service logins and workers need nothing.

Run once per person, in the store's database, as a superuser or as the role that ran the migrations (the table owner):

```sql
CREATE ROLE rivet_rose LOGIN PASSWORD '...';
GRANT USAGE ON SCHEMA public TO rivet_rose;
GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA public TO rivet_rose;
GRANT USAGE, SELECT ON ALL SEQUENCES IN SCHEMA public TO rivet_rose;
ALTER DEFAULT PRIVILEGES FOR ROLE rh_migrate IN SCHEMA public GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO rivet_rose;
ALTER DEFAULT PRIVILEGES FOR ROLE rh_migrate IN SCHEMA public GRANT USAGE, SELECT ON SEQUENCES TO rivet_rose;
REVOKE CREATE ON SCHEMA public FROM PUBLIC;
INSERT INTO rh_role_principals (role_name, user_id) VALUES ('rivet_rose', 'rose');
```

Here `rh_migrate` stands for the role that runs the migrations: the two `ALTER DEFAULT PRIVILEGES` lines give the login
the tables later migrations create, and the `REVOKE` (already the default on databases created under PostgreSQL 15 or
later) keeps any login from planting functions the elevated trigger bodies could resolve. An existing role is pinned
with the `INSERT` alone (`('rivet_coco', 'coco')`), after `GRANT SELECT ON rh_role_principals` if it was granted its
tables before 0053; the insert refuses a name that is not a role. Pin a role to one person only; never pin the role
that ran the migrations, a superuser, or a role with `BYPASSRLS`, and do not make a pinned role a member of the
migrating role (membership in any other role is harmless: a pinned login stays pinned under `SET ROLE`). Renaming or
recreating a pinned role unpins it, since the mapping is by name: repeat the `INSERT` under the new name. The node
then uses `postgres://rivet_rose:...@<host>/<database>` as its `RIVETHUB_PG_URL`; its `users.json` still names the
person and spaces for the gateway, but the store, not the node, decides what the login sees.

A person login gets nothing in the `graphile_worker` schema (CONTRACT note 123): the job queue carries no owner, so a
login that could add a job could have the store's workers read and rewrite anyone's rows. Grant it no `USAGE` there and
create no policies on the queue tables; migration 0055 takes both away from every role already listed in
`rh_role_principals`. The node notices that the queue is out of reach and captures without queueing, and the store's
workers find the rows on their own: the embedding sweep every 10 minutes (messages over 20 characters; the chunk sweep
every 15 minutes), summaries every 5 minutes (which also file and title the conversation), wiki extraction every
minute, and topic pages every hour. Set `tasks.enabled = false` on a person node; task delegation goes through the
queue, so a submit on such a node answers that the queue is absent, and the runner would otherwise log a failed claim on
every poll. `rh memory recompile`, `rh memory run` and the other queue commands answer the same way on a person login:
run them from the store's operator node.

Migration 0054 (CONTRACT note 122) puts the tables 0044 left open under row-level security, so the grants above stay
as written: a person login keeps full DML on every table and the policies decide which rows it reaches. Migration 0056
(CONTRACT note 124) closes the four tables 0054 left open: a person login reads `rh_topic_tombstones` and
`rh_tag_taxonomy` but writes only tombstones for its own topics and vocabulary rows it created, and it neither reads
nor writes `rh_worker_runs` or `rh_job_failures`. `rh_session_tombstones`, `rh_backfills` and the two migration ledgers
are readable by every login but written only under maintenance or, for tombstones, by the session's owner, and a
session key belongs to the first person who captured under it: a person login cannot open a conversation or write a
tombstone under another person's key. It writes wiki pages only in its own scope or a space scope it belongs to, under
that scope's key prefix. On a store that already holds
agent presets and conversations of more than one owner, 0054 stops until the connection names the person who owns the
existing presets, as 0042 did: `postgres://…/memory?options=-c%20rh.store_owner%3D<user>`. Run it before you create
the first person login.

## Standalone nodes

`rh local` migrates its SQLite file itself before opening it; `rivethub-migrate sqlite --path <file>` does the same by
hand and keeps a `<file>.v<N>.bak` copy before upgrading. Back up `~/.rivethub/memory.sqlite` by copying it while `rh
local` is stopped, or with `sqlite3 ~/.rivethub/memory.sqlite ".backup <file>"` while it runs, along with the wiki
directory `memory.sqlite.wiki_dir` names. SQLite has no row-level security and serves one person.
