# Golden deviations — crates/config

The final naming sweep changes the `memory.postgres.review_endpoint` removal
message from "compaction" to "the summarizer". The replay applies that contract
change to the expected finding only; fixture bytes remain unchanged.

The tasks refactor retires `tasks.pricing`, `tasks.eval` and `runtime.heartbeats`
(CONTRACT note 74). The replay drops recorded findings at or under those paths and
expects the one warning that names each retired key.

The memory-service cut retires whole sections (CONTRACT note 132), listed in
`config::validate::retired::RETIRED`: `mesh` (only `mesh.node_name` is still
read). The replay drops recorded findings at or under a retired path and expects
the one warning that names it whenever the input sets a key there other than a
kept one. Five `mesh` type-confused cases (`validate_config_mesh_channels_misc.jsonl`
:373, :434, :598, :625 and :674) now deserialize, because the retired keys have no
type, and their `rejected` rows left the table.

The `den` section is now named `gateway` (`den` is read as a deprecated alias).
The replay renames recorded `den` paths and messages to `gateway` on the
expected side only; fixture bytes remain unchanged.

The replay in `tests/validate/` drives every recorded case in
`tests/golden/fixtures/validate_config*.jsonl` through `config::validate_document`,
one test per recording file (a failing file lists every failing case with its
diff). Proof frame:

- A case is WELL-TYPED when `serde_path_to_error::deserialize` of the document
  into the typed `Config` succeeds. Well-typed cases must match the recorded
  findings exactly, compared as a sorted multiset of (severity, path, message).
  The CONTRACT §3 decisions (11, 12/20, 13, 15) and the 2026-10-13 provider
  prune (below) are applied to the recorded
  side first — they are boundary behaviour, not deviations.
- A TYPE-CONFUSED case (typed deserialization fails) produces exactly one error
  finding at serde's path, message `"<path>" has the wrong type: <serde
  message>`. The replay asserts only that the recorded outcome also had at
  least one error.
- `format_validation_result`, `resolve_env_vars` and `shared_dir` replays are
  exact and carry no deviations. The unused `shared_path`, `install_root` and
  `install_path` APIs were removed in the final fix round. Their recordings
  are named with reasons in `removed_path_helpers_have_named_recording_skips`,
  which checks that every named recording exists and contains cases.

The deviations below are the cases where the recording's expected findings are
not reproduced verbatim. They are pinned as data in
`tests/golden/deviations.jsonl` (one row per case: id, class, and the expected
findings that replace the recorded ones); `deviations_are_documented` asserts
the per-file counts against this document. Every well-typed case replays
against the recorded findings adjusted only by the CONTRACT decisions (Decision
11 above) and the config prune (below); 5,823 of the 6,008 validate cases
carry `RemovedByDecision` table rows. The remaining divergences
fall into the four classes below.

## Accepted-but-rejected (109 cases)

TS validated provider/plugin-side config keys only for presence and shape of
the section; it never type-checked individual values (plugin `ctx.pluginConfig`
is read loosely). The Rust typed model gives every key an honest type, so a
wrong-typed value under such a key now fails deserialization and the whole
document is type-confused. CONTRACT §2 puts these inputs outside the boundary
(inputs clients never send). Five examples per file:

- `validate_config.jsonl` — 2 accepted-but-rejected; ids: validate_config.jsonl:125, validate_config.jsonl:384
- `validate_config_agents_providers.jsonl` — 17 accepted-but-rejected; ids: validate_config_agents_providers.jsonl:61, validate_config_agents_providers.jsonl:86, validate_config_agents_providers.jsonl:166, validate_config_agents_providers.jsonl:235, validate_config_agents_providers.jsonl:249
- `validate_config_den.jsonl` — 2 accepted-but-rejected; ids: validate_config_den.jsonl:164, validate_config_den.jsonl:753
- `validate_config_memory.jsonl` — 27 accepted-but-rejected; ids: validate_config_memory.jsonl:58, validate_config_memory.jsonl:94, validate_config_memory.jsonl:201, validate_config_memory.jsonl:245, validate_config_memory.jsonl:291
- `validate_config_mesh_channels_misc.jsonl` — 19 accepted-but-rejected; ids: validate_config_mesh_channels_misc.jsonl:9, validate_config_mesh_channels_misc.jsonl:18, validate_config_mesh_channels_misc.jsonl:57, validate_config_mesh_channels_misc.jsonl:60, validate_config_mesh_channels_misc.jsonl:135
- `validate_config_runtime.jsonl` — 35 accepted-but-rejected; ids: validate_config_runtime.jsonl:2, validate_config_runtime.jsonl:24, validate_config_runtime.jsonl:44, validate_config_runtime.jsonl:51, validate_config_runtime.jsonl:85
- `validate_config_tasks_workflows.jsonl` — 6 accepted-but-rejected; ids: validate_config_tasks_workflows.jsonl:37, validate_config_tasks_workflows.jsonl:179, validate_config_tasks_workflows.jsonl:335, validate_config_tasks_workflows.jsonl:395, validate_config_tasks_workflows.jsonl:412

`validate_config_untyped.jsonl` has none. Examples of the class: a string
`runtime.plugin_dirs` (TS never type-checked it), a list `providers.claude-cli.binary`,
a string under `memory.sqlite.workers`. Every affected case carries at least
one typed Rust error; none loses a recorded warning without a replacement.

## Order-only "Available" lists (60 cases)

The `Provider … referenced by agent … is not defined` / `Default agent … is not
defined in [agents]` / `Heartbeat agent …` messages embed the agent or provider
names in document order. The typed model holds `agents` and `providers` as
`BTreeMap`, so the list is sorted; CONTRACT §2 excludes key order from the
boundary (the only order-sensitive boundary artefacts are capture event ids).
Paths, severities and the name multisets match; only the in-message order
differs. Five examples per file:

- `validate_config.jsonl` — 13 order-only Available lists; ids: validate_config.jsonl:31, validate_config.jsonl:65, validate_config.jsonl:88, validate_config.jsonl:336, validate_config.jsonl:339
- `validate_config_agents_providers.jsonl` — 26 order-only Available lists; ids: validate_config_agents_providers.jsonl:71, validate_config_agents_providers.jsonl:97, validate_config_agents_providers.jsonl:114, validate_config_agents_providers.jsonl:126, validate_config_agents_providers.jsonl:175
- `validate_config_runtime.jsonl` — 22 order-only Available lists; ids: validate_config_runtime.jsonl:54, validate_config_runtime.jsonl:68, validate_config_runtime.jsonl:143, validate_config_runtime.jsonl:209, validate_config_runtime.jsonl:211

## Sequence-shaped sections and entries (3 cases)

serde's derived structs accept the sequence form, so a section written as a
YAML list (`agents.local: []`, `providers.reasoning_effort: []`) deserializes
into the all-default entry instead of failing with the recorded
`… must be an object` error. The table pins the actual expected findings for
each such case. Examples:

- `validate_config_agents_providers.jsonl` — 3 sequence-shaped sections or entries; ids: validate_config_agents_providers.jsonl:47, validate_config_agents_providers.jsonl:81, validate_config_agents_providers.jsonl:657

## Explicit nulls read as missing (7 cases)

Absent and explicit `null` are the same "missing" on every typed field
(NULLABLE: no `Option<Option<T>>`). serde_yaml goes one step further
for the plain container shapes: an explicit null deserializes as the empty
sequence (`runtime.heartbeats`, `memory.capture.redaction.patterns`,
`den.allowed_origins`) or the all-default section (`den.devices`,
`memory.capture`), so the document stays well-typed and the recorded
must-be-array / must-be-an-object / must-be-a-list error is not reproduced.
The table pins the actual expected findings for each such case: recorded error
findings whose input path holds an explicit `null`, while the Rust side is
silent. Examples:

- `validate_config_den.jsonl` — 3 explicit nulls read as missing; ids: validate_config_den.jsonl:131, validate_config_den.jsonl:466, validate_config_den.jsonl:740
- `validate_config_memory.jsonl` — 3 explicit nulls read as missing; ids: validate_config_memory.jsonl:234, validate_config_memory.jsonl:337, validate_config_memory.jsonl:468
- `validate_config_runtime.jsonl` — 1 explicit nulls read as missing; ids: validate_config_runtime.jsonl:599

## Decision 15 — capture redaction is a real setting

- Decision: `memory.capture.redaction.{enabled,builtins,patterns}` is honoured
  by the hook-side capture writer (default off), so the TypeScript warning that
  the block is "validated but not yet injected into harness hook processes" no
  longer applies and is never emitted.
- Old: the warning above under `memory.capture.redaction.enabled` whenever
  `enabled: true`. New: the warning is absent; the setting is passed through
  and reachable as `Config::capture_redaction()`.
- The replay drops every recorded instance of the warning before comparing
  (a uniform contract adjustment, applied to every case).

## Decision 13 — workflows left the runtime

- Decision: the separate `rivethub-workflows` package drives workflows over the
  public API, so a top-level `workflows:` section is accepted and ignored with
  one warning for ANY value present; the recorded per-key validation
  (`"workflows" must be an object`, `"workflows.enabled" must be a boolean`,
  `Unknown workflows key "…"`, and so on) is not reproduced.
- The replay replaces every recorded finding under `workflows` with the single
  warning `"workflows" is ignored — workflows moved out of the runtime into the
  rivethub-workflows package.` whenever the key is present.

## Decision 12/20 — the plugins list is accepted and ignored

- Decision: the runtime loads no plugins, so a top-level `plugins:` list is
  accepted and ignored with one warning for ANY value present; the recorded
  shape validation (`"plugins" must be an array of npm package names`,
  `Each plugins entry must be a non-empty package name string`,
  `Duplicate plugin entry "…"`) is not reproduced. `plugins/tools/*` entries
  get a deprecation notice at load time (one per entry, `Loaded::notices`).
- The replay replaces every recorded finding under `plugins` with the single
  warning `"plugins" is ignored — the Rust runtime loads no plugins.` whenever
  the key is present.

## Decision 11 — provider types the runtime does not build

- Decision: a provider whose type is a removed type — given by `type` or
  implied by the provider id — produces exactly one error at
  `providers.<id>.type`: `Provider "<id>" type "<type>" is not built — <the
  type's alternative>.` The removed types and their alternatives (one match in
  `provider.rs`, `RemovedProviderType::message`): `anthropic` → the claude-code
  harness, `google`/`gemini` → the gemini harness, `xai` → the grok-build
  harness, `ollama` → a harness or `openai-compatible` against
  `http://<host>:11434/v1`, `llama-server` → `openai-compatible` against
  `<server>/v1`, `openai-compat` → spelled `openai-compatible` (this replaces
  the old split message and its entry-level path), `cli`/`pi-cli`/`qwen-code` →
  no CLI provider of that type, use a harness (`qwen-code` stays a harness id;
  only the provider type is removed). Recorded per-entry findings (model
  required, token-command family, unknown-key warnings) are not reproduced.
- The replay drops every recorded finding under `providers.<id>` and adds the
  decision error. Recordings never carry a `type:` key, so the id decides.

## The config prune — removed provider keys warn

- Decision (Phil 2026-10-13): model-server tuning belongs on the model server,
  so the provider keys `temperature`, `top_p`, `top_k`, `min_p`,
  `presence_penalty`, `frequency_penalty`, `seed`, `stop`,
  `repetition_penalty`, `min_tokens`, `default_tool_choice`,
  `mm_processor_kwargs`, `chat_template_kwargs`, `extra_body`, `models`,
  `models_ttl_ms`, `models_url`, `verify_model_on_init`, `num_ctx` and
  `keep_alive` are gone from `providers.<openai-family id>`. `token_command`,
  `token_ttl_ms` and `token_command_timeout_ms` are restored on those
  providers: the argv mints the bearer (one trailing newline removed), the
  command wins over `api_key`, and a zero ttl or timeout is an error. The embed keys
  `embed_token_command`, `embed_token_ttl_ms`,
  `embed_token_command_timeout_ms` and `embed_wire_shape` are gone from
  `memory.postgres` (and `memory.postgres.embedded`) and `memory.sqlite`. A
  config that still sets one gets ONE warning per key,
  `<key> is set on the model server, not in RivetHub`, instead of the recorded
  silence; removed keys under a removed provider type stay silent (the entry
  error above replaces everything under `providers.<id>`).
- The replay computes the expected warnings per case from the input and pins
  them in ONE typed table keyed by case id (`RemovedByDecision` in
  `tests/validate/main.rs`: reason plus the expected findings); the trial
  `removed_by_decision_table` pins the table's size. The table also retires the
  recorded findings at a removed key's path — the recorded wrong-value errors
  for `embed_wire_shape` and `embed_token_command`, for example — because the
  key no longer has a type or range to violate. Three
  `accepted-but-rejected`-adjacent rows changed for the same reason: with the
  typed fields gone, `validate_config_memory.jsonl` :665 and :713 deserialize
  and replay as well-typed (their `rejected` rows left the table), and
  `validate_config_agents_providers.jsonl` :543 stopped being type-confused and
  gained an `order` row for its Available lists.

## Decision 9 — no embedded Postgres (PGlite)

- Decision: `memory.postgres.embedded` selects no transport any more; the
  memory backend resolves to `sqlite` and the one-time Postgres import runs at
  first start. `memory.backend` (`postgres` | `sqlite`) is new and never
  appears in the recordings, so no recorded case diverges; the decision is
  asserted in `tests/decisions.rs`. An unknown `memory.backend` value is a
  type-confused document (serde unknown variant).

## Decisions that add behaviour the recordings cannot reach

Each carries a table test in `tests/decisions.rs` naming the decision:

- Decision 16: `RIVETHUB_*` variables are preferred over their `RIVETOS_*`
  equivalents, with a deprecation notice naming both; the config probe prefers
  `~/.rivethub/config.yaml` before `~/.rivetos/config.yaml` (legacy flag).
- Decision 9: the new `memory.backend` key accepts `postgres` and `sqlite`.
- Decision 10: `RIVETHUB_MCP_ENABLE_{SHELL,FILE,SEARCH}` and their `RIVETOS_`
  spellings are accepted, ignored, and reported as one deprecation warning
  each; `plugins/tools/*` entries in the `plugins:` list get a load-time
  deprecation notice each.
- Decision 2: `RIVETHUB_TERM_MUX=tmux` (or the `RIVETOS_` spelling) warns and
  resolves as unset; `herdr` stays the only multiplexer value. The
  `…_INJECT_*` and `…_TMUX_USER_CONF` variables are accepted, ignored, and
  warned.

`tests/decisions.rs` also pins the inventory's documented defaults that the
typed config exposes (`den.port` 5174, `memory.postgres.embedded.port` 5433,
`max_connections` 96) and verbatim messages the
recordings do not reach (blank `den.host`, `den.port: 0`, blank
`den.allowed_origins` entries, `runtime.turn_timeout: 0`, shell-unsafe
`mesh.advertise_host`, empty/ReDoS/invalid redaction patterns, claude-cli
`permission_prompts` / `permission_timeout_ms`).
`memory.postgres.embedded.auto_migrate` is not one of those defaults: the key
is gone, and a document that sets it warns once.

## Diagnostic-text allowance (CONTRACT §3)

`Invalid regex: <diagnostic>` findings (capture redaction patterns) keep the
TypeScript prefix verbatim; the diagnostic suffix comes from `regress` (the
plan's ECMAScript engine). CONTRACT §3 allows engine diagnostic text to differ
after the TypeScript prefix.

## Root-shape and wrong-type messages

`Config must be a YAML object (got …)` and the per-field `… must be a string`
/ `… must be an array` shape messages are replaced, per the proof frame, by
the single typed error `"<path>" has the wrong type: <serde message>` for
type-confused documents. Required-section and required-field missing messages
(`Missing required section "runtime"`, `Missing required field
"runtime.workspace"`, `"memory.sqlite.path" is required …`) remain verbatim
because absent keys deserialize to `Option::None` on the typed fields.
