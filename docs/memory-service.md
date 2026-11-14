# Memory service

Every node is a memory service: capture, recall, and the organizer
(topics, entities, the wiki, tags, and Tidy), with attribution kept on the
stored rows. The gateway mounts `/healthz`, `/api/capture`, `/api/memory`
(including tags), `/api/wiki`, `/api/topics` and the other memory routes,
the stored sessions and their transcripts, and the plug-in host. It has no
live-session, chat, terminal, task or delegation routes; those run in T3
Code. `runtime.profile` chose between this surface and a larger one before
0.8. The key still loads, and `rh config validate` warns that it is ignored.

## Contract

Memory HTTP responses carry `x-rivethub-contract: 1`.
`GET /api/memory/contract` returns `{ "contractVersion": 1 }`.
Recall tools stamp the same number on `_meta.contractVersion`.
A client compares that number before it renders a response.
The number moves independently of the crate version.

## Models per job

Each memory job can run on its own model. Set them under `memory.models`:

```yaml
memory:
  models:
    default:
      provider: main
      model: big-model
    summarizer:
      provider: local
      model: small-model
    wiki_writer:
      provider: main
      model: big-model
      effort: high
    embedder:
      provider: embeddings
```

`default` is required. `summarizer`, `categorizer` and `wiki_writer` inherit
it when they have no entry. `embedder` takes either a `provider` or an
`endpoint` with a `model`. Without `memory.models` every job uses the default
agent's provider and model, as before. The node does not log that on startup.
`rh doctor` says so when no model can be resolved (CONTRACT note 173).

`GET /api/memory/models` lists the five roles with the assigned provider and
model, whether the role inherits `default`, a health of `healthy`, `slow` or
`failing`, and the number of jobs waiting. On a team hub only the owner can
read it.

## Shared database files

`rh db status` prints a `foreign:` line. A file RivetHub created alone
reports `foreign: none`. A file that also holds another program's virtual
table (for example `vec0`) names that module and any trigger that refers to
it. `rh doctor` adds the same list to the memory check. A current schema
with foreign objects is a warning; a pristine current schema stays a pass.
Migration still runs. Rebuilds copy rows, including `owner_user_id`, instead
of renaming tables, so a trigger for an unloaded module does not stop the
upgrade.

## More than one node

`RIVETHUB_HOME` is the state directory for that process. It must be an
absolute path with no `..` components. Two processes with different
`RIVETHUB_HOME` values use different databases, config files, and owner
locks, and they do not adopt `~/.rivetos`.
