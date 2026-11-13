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
3. **The schema:** `rivethub-migrate postgres` (next section). It also creates the job queue the workers use: graphile-worker's `graphile_worker` schema at the version every existing install carries (0.17.3), created by
   migration 0052 when the database has none and left alone when it has one.

## Migrations

```sh
rivethub-migrate postgres --url "$RIVETHUB_PG_URL"     # or --config FILE (its connection_string, else RIVETHUB_PG_URL)
rivethub-migrate status postgres                       # current, required, pending; read-only
```

Run it before starting any service on a new database and after every upgrade; `rh update` runs the new release's
`rivethub-migrate` itself before swapping binaries. Services never migrate: each verifies the schema version it needs
(62 at 0.8.3, unchanged since 0.8.0) and refuses to start behind it. `rh doctor` and `rh db status --url <url>` report the same status.
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

## Upgrading from 0.8.2 to 0.8.3

0.8.3 needs no schema migration. It fixes what a new user on a standalone SQLite node (the one `local.sh` installs and
`rh local --ensure` starts) ran into:

- `rh service init` wrote a unit that ran `rh start`, which refuses the standalone config `rh local` writes (no
  `runtime`, `agents` or `providers`), so the unit restarted forever. The unit now runs `rh local` for a standalone
  SQLite config, or when no config exists yet, and `rh start` for a node config; it prints which. A unit written by an
  older `rh` for a standalone node still says `rh start`: stop it, delete the file and run `rh service init` again.
- `rh service init` now also runs `systemctl [--user] enable rivethub`, so the node comes back after a reboot (or, for a
  user unit, at login, and at boot when lingering is on). A unit written before 0.8.3 is not enabled: run
  `systemctl --user enable rivethub` (or `systemctl enable rivethub` as root).
- `rh update` failed with a bare `No such file or directory (os error 2)` on every node without a systemd unit. Without
  a unit it now stops the node `rh local --ensure` started and starts it again with the new binary, keeping its port and
  bind flags, and confirms its health; with no node running it installs and says nothing needed a restart. `--no-restart`
  still skips this.
- Without `--version`, `rh update` no longer installs a release older than the running `rh`: GitHub's latest stable
  release can be an old tag (`v0.1.4`), which 0.8.2 tried to install over itself. It says the running `rh` is already
  newer or up to date and exits 0.
- `rh update --version <tag>` pins exactly that release, a prerelease included, on the default stable channel; 0.8.2
  refused a pinned prerelease with a misleading "no archive for this platform". A missing tag, a draft or a missing asset
  is now named as such.
- `rh harness connect opencode` installs the OpenCode capture plug-in embedded in `rh`; `RIVETHUB_OPENCODE_PLUGIN` is no
  longer needed and, when set, still names a file used instead. A connect that skips a part no longer ends with a bare
  `connected.`: it names the skipped part and says which part it connected. A 0.8.2 `rh harness connect opencode` that
  printed `connected.` wired recall only; run it again to add capture.
- `rh harness connect` (and `disconnect`) that only created new files printed `Dry run: nothing was written.` although
  it wrote them; it now prints that line only for `--dry-run`.

## Upgrading from 0.8.1 to 0.8.2

0.8.2 needs no schema migration. It fixes capture on a node whose gateway serves TLS from a certificate that lists another
name before `localhost`: on 0.8.1 the hook reached the gateway by that name, the owner gate refused the local owner token
with a 401, and every batch was dead-lettered. After the upgrade, move them back to the queue; the next hook delivers them:

```sh
rh capture spool --requeue-dead
```

Also in 0.8.2:

- A capture into a session that belongs to another person now gets a 403, not a 500. The hook dead-letters that batch at
  once instead of retrying it, and `rh capture backfill` counts it against that session and goes on.
- `rh capture backfill` covers dsh sessions. A session it cannot read fails alone; the others are still captured.
- A stored share receipt or origin that is not a JSON object reads as absent, so it never reads as verified.

## Upgrading from 0.8.0 to 0.8.1

0.8.1 needs no schema migration. It is also a security release, so these things behave differently after the upgrade
(CONTRACT notes 143 to 152 have the rules).

### Local access

- **Loopback is not the owner any more.** Every route needs a credential except `/healthz` (name, node and version
  only), the static shell and `/login`, `POST /api/auth/session` (which needs a live single-use login code), and
  `GET /api/drops/{id}` when a team server has drops enabled. A script or a `curl` that talked to `127.0.0.1:5174`
  without a credential now gets 401. Send the local owner token:
  `curl -H "Authorization: Bearer $(cat $(rh auth token-path))" ...`. `rh`, the capture hook and the systemd user unit
  read that file themselves. For a browser, run `rh open` and use the single-use link it prints.
- **The owner credential has limits.** The gateway honors the owner token or an owner browser session only from a direct
  loopback peer, with a `Host` of `127.0.0.1`, `[::1]` or `localhost`, and with no `Forwarded`, `X-Forwarded-*` or
  `X-Real-IP` header. A reverse proxy or an `ssh -R` tunnel in front of the gateway no longer reaches the owner routes,
  and a desktop or script integration must connect directly to the loopback port. Behind a proxy, use a users-file
  token instead.
- **Fresh login for sensitive actions.** Token rotation, account sign-in, sign-out and edits, and memory export need a
  browser login made within `gateway.fresh_login_secs` (300 seconds by default), else 401 `reauth_required`: run `rh open`
  again. The owner token is always fresh. The same holds for sending a share and for signing in or out of a share
  server. `/api/share/**` takes only the owner token or an owner browser session, never a users-file token or a plug-in.
- **Rotation and downgrade.** The gateway creates `<state_dir>/local-owner.token` the first time the new version starts.
  `rh auth rotate` replaces it and ends every browser session. A new `rh` against a still-running 0.8.0 gateway keeps
  working until the gateway restarts. To go back to 0.8.0, delete `local-owner.token` first.

### Capture

The hook writes each batch to `~/.rivethub/capture-spool/` before it posts, and a 401 or a refused connection leaves it
there for the next hook to deliver. Nothing evicts the spool, so disk space is the only limit: the hook warns past 1,000
entries or 100 MB, and `rh status` and `rh doctor` warn too. A batch that cannot be written exits the hook with 1 and
`capture batch was NOT saved`. `rh capture spool` lists the queue and `--requeue-dead` retries the dead letters. `rh
capture backfill` captures everything already on disk, including Claude Code subagent transcripts.

Only the local owner's capture can record a share receipt. A batch from any other principal (a team member's token, a
plug-in) is kept, but the gateway strips its `settings.receipt`, so a session a member receives through your server reads
as unverified. `memory_append` and `memory_ingest_session` refuse an entry for an imported session (an `import:` key) or
one that carries a receipt.

### Receiving shared sessions

`rh memory receive` and `rh memory ingest-bundle` check every sanitized bundle for secrets and prompt injection before
they write. A sanitized bundle with any finding imports nothing: the command prints a `held:` report and exits 3, and a
received bundle's ciphertext stays in the inbox for a rerun. Decide per session with
`--session <n>=import|skip|hold-spans`, or for all with `--all=import|skip|hold-spans`. A session with a secret finding
takes only `skip` or `hold-spans` unless you also pass `--force`. For a sanitized bundle, `--force` alone imports
nothing flagged, because a decision is still needed. A raw bundle (one `rh memory sanitize` has not stamped) is different:
`ingest-bundle` refuses it unless you pass `--force`, and a forced raw import skips these checks entirely. The MCP tool
`session_import` refuses any bundle with findings and tells the agent to ask you to run the CLI command. The desktop's
receive routes apply the same rules to sanitized bundles.

### Share servers

- **Team servers.** A users-file token is not enough to serve shares: `gateway.drops.enabled` is `false` by default, and
  until it is on, the server has no drop routes. To turn it on, put this in config.yaml. `root` defaults to a `drops`
  directory in the state directory and must be owned by the server's user with mode 0700. Defaults are shown:

  ```yaml
  gateway:
    drops:
      enabled: true
      root: /var/lib/rivethub/drops   # optional
      max_bytes: 67108864             # at most 64 MiB per drop
      per_user_drops: 50
      per_user_bytes: 536870912       # 512 MiB per member
  ```

  A drop expires after 7 days and downloads once. Members upload with a bearer token from their entry in the users
  file, and a bare digest is a full token that also reads all memory, so give a member who only shares
  `{"digest": "<sha-256 hex>", "scopes": ["drops:write"]}`. The local owner token is not a users-file token and cannot
  upload. Serve it over TLS, or list the reverse proxy in `gateway.trusted_proxies` so the server
  can tell that a request arrived over HTTPS (CONTRACT note 143).
- **Sign in to share again.** 0.8.0 signed in to rivethub.cloud with its own `/api/cli` endpoints, which the cloud no
  longer serves. `rh share login` now uses OAuth device sign-in found through the server's RFC 8414 metadata, and asks
  for `offline_access` so the login can refresh. Run `rh share login` once on each machine. A team server offers no
  device sign-in: members use the users-file token.
- **`cloud.disabled`.** New and off by default. See "Keeping data on your network" below to keep a hub off
  rivethub.cloud, and `rh local --no-cloud` to write it into a new standalone config.

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
plug-ins, their grants and settings) and and `plugins/<id>/data/`.

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

- List the people and the SHA-256 digests of their bearer tokens in the users file (`gateway.users_file`; README,
  Tenancy). Rows captured with a bearer token take that person as owner; loopback, plug-ins and `rh mcp` act as the node owner
  on a personal node.
- No node treats loopback as the owner (CONTRACT note 147), so a team server needs no extra switch: every caller needs
  a credential, and a same-host forwarder gains nothing. Give the server's own tools one: `rh` and the capture hook read
  `<state_dir>/local-owner.token`, and `memory.capture.gateway.token` names a remote capture gateway's token.
- Behind a reverse proxy, list it in `gateway.trusted_proxies` and have it overwrite `X-Forwarded-Proto`, not append
  to it: Caddy does; nginx needs `proxy_set_header X-Forwarded-Proto $scheme`.
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
minute, and topic pages every hour. `rh memory recompile`, `rh memory run` and the other queue commands answer on a person login that the queue is absent:
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

## Keeping data on your network

RivetHub Cloud features are on by default: sign-in, crash reports and usage counts, update checks against
rivethub.cloud, and sharing through the rivethub.cloud drop box. To turn all of them off for every user of a hub, put
this in config.yaml:

```yaml
cloud:
  disabled: true
```

With the policy on, sign-in and the cloud update source answer `409` (`disabled by this hub's policy`), no crash report
or usage count is written or sent, `rh update` fetches GitHub releases, and nothing is shared through rivethub.cloud.
Users cannot turn any of it back on; only an edit to config.yaml does. `rh status`, `rh doctor` and `/healthz` report
the policy. CONTRACT note 145 has the full rules.

To keep crash reports and usage counts but send them to your own collector, name it in `telemetry.url`:

```yaml
telemetry:
  url: https://telemetry.example.com/
```

The node then posts crash reports to `<url>/v1/telemetry/crash` and daily usage counts to `<url>/v1/telemetry/usage`
with the same JSON rivethub.cloud receives, and nothing goes to rivethub.cloud for telemetry. Answer `202` to accept a
report; `422` or `413` drops it. On any other answer a crash report stays queued for the next try, but a day's usage
report is not resent. This also works with `cloud: {disabled:
true}`: the cloud stays off and only telemetry goes to your url. `telemetry: {disabled: true}` turns telemetry off
everywhere and wins over the url. CONTRACT note 154 has the rules.

The entity worker matches observations with the ThinkingCap matcher and the embedder at `http://127.0.0.1:9402`
(`qwen3-embedding-0.6b`) unless config.yaml says otherwise:

```yaml
entities:
  matcher: jev            # jev | thinkingcap (default thinkingcap)
  matcher_url: http://host:8015
```

`matcher` must be `thinkingcap` or `jev`. `jev` needs `matcher_url`, and that url must be http or https. When
`memory.postgres.embed_endpoint` or `memory.postgres.embed_model` is set, entity matching uses it; with SQLite selected,
the same keys under `memory.sqlite` are used instead. An omitted `entities` section keeps the ThinkingCap matcher.
CONTRACT note 155 has the rules.

This hub's gateway and its `rh` CLI enforce the policy, including the share rules below, by reading this hub's
config.yaml. The policy governs the hub, not every program on the machine. A shell user who runs `rh` with a config
of their own is not bound by it; to keep such users off the network, restrict their outbound traffic as well.

To share only with your own team servers, list them. Each entry is a host with an optional `:port`:

```yaml
memory:
  share:
    server:
      url: team.example.com
    allowed_servers: [team.example.com]
```

A non-empty list admits only those servers, with or without `cloud.disabled`. With `cloud.disabled` and no list,
sharing is off. A list entry that is not a valid host makes every share command and `rh doctor` fail and name it.

At install time, `curl -fsSL https://get.rivethub.io/local.sh | bash -s -- --no-cloud`, `rh local --no-cloud`, or
`RIVETHUB_CLOUD=off` writes `cloud: {disabled: true}` into the config `rh local` generates. When a config already
exists without the key, `rh local` stops and names the file to add it to; it never edits your config.
