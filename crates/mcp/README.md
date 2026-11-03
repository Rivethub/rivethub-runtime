# MCP adapter (CONTRACT B7)

`Server::new` accepts an `Arc<dyn MemoryStore>`, an explicit memory-write permission,
and a `CancellationToken`. It serves `memory_search`, `memory_browse`, `memory_get_full`,
`memory_stats`, `memory_tags`, `wiki_search`, and `wiki_read`. Memory responses are JSON text
serialized directly from kernel result types. No concrete memory adapter is linked.

`Server::builder` injects an `Arc<dyn kernel::tasks::Delegation>` and a transport surface.
`delegate_task` and `list_agents` call that port. `delegate_task` is registered only on
`Surface::Stdio`. `Surface::Remote` (streamable HTTP and Unix) registers `list_agents` and
not `delegate_task`. `Server::new` registers neither. The composition root wires the port.

`echo`, `skill_list`, and `skill_manage` are excluded by the product decision. The skills
implementation is removed. `internet_search` and `web_fetch` remain absent because harnesses
own web tools.

`transport::serve_stdio` serves newline-framed MCP. `serve_tcp` and `serve_unix` expose streamable
HTTP at `/mcp` using rmcp, with axum owning independent connections and rmcp owning sessions.
TCP requires a validated, nonempty `Bearer`; Unix accepts `Option<Bearer>`. Every HTTP request,
including session requests, passes authorization. Token digests are compared with `subtle`.
Browser origins are refused; rmcp's default loopback Host validation remains enabled.
`serve_tcp_with_config` accepts explicit rmcp Host and Origin allowlists for remote deployments.
`bind_unix` publishes the socket only after setting mode 0600 in a private directory. An existing
socket is removed only after a refused connection and a check that its device and inode have
not changed. Live sockets, non-sockets, symlinks, failed probes and timed-out probes produce
typed errors naming the path. The composition root owns socket-path removal after shutdown.

`Client::connect` accepts any rmcp client transport, including `TokioChildProcess` and
`StreamableHttpClientTransport` (with a reqwest client for TCP or Unix HTTP). `Client::tools`
returns `Arc<dyn kernel::tool::Tool>` values retaining remote schemas, content, and tool errors.
`close` joins the protocol service. Cancelling the supplied token stops discovery and calls.
The composition root creates the runtime, supplies transports and credentials, and owns shutdown.

B7 defaults: missing arguments are an empty argument object. Search uses hybrid mode, both
scopes and limit 10; browse uses descending order, no tools and limit 50; tags default to pending
and limit 100. Explicit limits are validated. Browse passes named windows to the kernel and
rejects `window` combined with either `since` or `before`, naming all conflicting fields.
Memory writes require explicit permission.

## Schemas and recordings

Each argument struct derives `JsonSchema`. Descriptions and recorded bounds are field attributes;
tool names, descriptions and annotations live beside the structs. Shared kernel and wire enums
also derive their schemas. The generator inlines subschemas. The `!deny_unknown_fields` schema
attribute preserves the recorded absence of `additionalProperties`, while serde still rejects
unknown arguments. UUIDs and timestamps use string schemas as recorded and retain typed decoding.

Three named, tested schema transforms preserve other recorded presentation details:
`recorded_schema_metadata` removes generated titles, formats and defaults recursively;
`optional_non_null` exposes optional fields without a null alternative, except `parent_value`,
whose recorded null means clear; `search_mode_order` moves the derived vector variant last.
These transforms do not change argument decoding or validation.

`tests/replay.rs` compares derived `tools/list` against the recording filtered to the stdio
catalog (every recorded tool except the five product-removed tools), as parsed rmcp values.
Only the recording's product names are translated by
`translate_recorded_product_names`: `RivetOS` to `RivetHub`, `rivetos` to `rh`, and `~/.rivetos`
to `~/.rivethub`. Other recorded description prose stays intact. The checked `SKIPPED` list
names every omitted tool and its reason; calls to omitted tools have no handlers. The recorded
unknown-tool error is replayed. Initialization checks the recorded protocol and capabilities;
server identity is `rivethub-mcp-server`, versioned with this crate.

No memory-call payloads were recorded. Tests check kernel requests and results directly. Search
bounds, summary expansion, synthesis and agent-filtered stats have no corresponding kernel inputs;
explicit requests for these features are refused. No expansion or synthesis is implied when absent.
Taxonomy upsert is a full replacement through the kernel; aliases must be supplied explicitly.
The enabled-write catalog describes replacement semantics and changes its annotation accordingly.
The `rivethub` composition root builds the server for `rh mcp` and `rh local`.

`Server::builder(...).plugins(port)` takes a `kernel::plugins::PluginTools` and lists, after
the B7 tools, the plug-in creator's four tools and every running plug-in's tools as
`<id>-<tool>` (CONTRACT note 57); a call goes to the port and its result becomes one text
content. The port changing its version sends `notifications/tools/list_changed`. `rh local`
passes the gateway's plug-in host directly; `rh mcp` passes `plugins::SocketTools`, which
reads `<state>/plugins/tools.sock` with the bearer in `tools.token`.

Validation covers derived schemas and replay, wrong-type errors, read/write tag routing,
unsupported-option refusal, conflicting browse filters, authenticated TCP and Unix client bridges,
permissions, stale socket recovery, live/non-socket refusal and cancellation.

Wiki calls use `MemoryStore::wiki_topics` and `wiki_page`. Search defaults to five results and
validates limits from 1 through 20. The response preserves the typed topic list, including its total.
Read returns small pages verbatim; above 24,000 Unicode characters it returns a bounded view of
the title, summary, article, crosslinks and recent history. Explicit sections use the typed page
fields. Full reads of oversized pages are refused. Section truncation is marked and all cuts count
Unicode characters. Missing articles and pages are explicit tool errors. Redirect targets pass
the shared `WikiSlug` validation; cycles and chains exceeding 16 lookups are refused.

The recording contains wiki schemas but no wiki call payloads. `tests/wiki.rs` checks typed port
requests and responses, every section, Unicode size boundaries, valid redirects, invalid targets,
cycles, missing pages, store failures, invalid arguments and cancellation. Schema description translation applies
only to the expected recording, including the slug example; actual schemas are compared unchanged.
