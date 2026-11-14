use crate::Error;
use crate::catalog::ToolInput as _;
use chrono::{DateTime, Utc};
use kernel::memory::{BrowseFilter, Limit, MemoryStore, SearchQuery};
use rmcp::model::CallToolResult;
use serde::Deserialize;
use uuid::Uuid;
use wire::gateway::memory::{BrowseWindow, MemoryBrowseOrder, MemoryScope, SearchMode};
use wire::message::Role;

#[derive(schemars::JsonSchema, Debug, Deserialize)]
#[schemars(!deny_unknown_fields)]
#[serde(deny_unknown_fields)]
pub struct Search {
    #[schemars(description = "Search query — natural language question or keywords")]
    query: String,
    #[serde(default)]
    #[schemars(description = "Search mode (default hybrid). hybrid: fuses full-text + trigram + vector with RRF — best general recall, robust to dotted/literal terms (domains, IPs, model ids). fts: full-text only. trigram: fuzzy/literal-token match. regex: pattern match. vector: pure semantic (ANN over HNSW).", transform = crate::catalog::search_mode_order)]
    mode: SearchMode,
    #[schemars(description = "Where to search (default: both)", transform = crate::catalog::optional_non_null)]
    scope: Option<MemoryScope>,
    #[schemars(description = "Max top-level results (1–50, default: 10)", range(min = 1, max = 50), transform = crate::catalog::optional_non_null)]
    limit: Option<u32>,
    #[schemars(description = "Filter by agent (opus, grok, etc.)", transform = crate::catalog::optional_non_null)]
    agent: Option<String>,
    #[schemars(description = "Only message hits with this role (user, assistant, system, tool). Summaries carry no role and are not filtered; pass scope=messages for messages only.", transform = crate::catalog::optional_non_null)]
    role: Option<Role>,
    #[schemars(description = "Only message hits from this tool (e.g. Bash, Read), matched exactly. Summaries are not filtered; pass scope=messages for messages only.", transform = crate::catalog::optional_non_null)]
    tool_name: Option<String>,
    #[schemars(description = "Only results whose conversation carries this accepted key:value tag (e.g. project:acmeapp). See memory_tags.", transform = crate::catalog::optional_non_null)]
    tag: Option<String>,
    #[schemars(description = "Only return results after this date (ISO UTC timestamp). Prefer window= for local-day bounds.", with = "Option<String>", transform = crate::catalog::optional_non_null)]
    since: Option<DateTime<Utc>>,
    #[schemars(description = "Only return results before this date (ISO UTC timestamp). Prefer window= for local-day bounds.", with = "Option<String>", transform = crate::catalog::optional_non_null)]
    before: Option<DateTime<Utc>>,
    #[schemars(description = "Shortcut for time-bounded filters — resolves in the SERVER local timezone. last_7d/last_14d are rolling (prefer over this_week early in the week). Used only when neither since nor before is provided.", transform = crate::catalog::optional_non_null)]
    window: Option<BrowseWindow>,
    #[schemars(description = "Auto-expand top summary hits to show source messages (default: true)", transform = crate::catalog::optional_non_null)]
    expand: Option<bool>,
    #[schemars(description = "Use LLM to synthesize a focused answer from results (default: false). Requires a configured compactor endpoint.", transform = crate::catalog::optional_non_null)]
    synthesize: Option<bool>,
}

impl crate::catalog::ToolInput for Search {
    const NAME: &'static str = "memory_search";
    const DESCRIPTION: &'static str = "Search RivetHub persistent memory (conversation history + summaries). Hybrid FTS + semantic + temporal scoring with auto-expansion of summary hits to their source messages. Use this to find past decisions, prior context, or \"what did we say about X\" before asking the user. Mirrors the in-process `memory_search` tool exposed to local agents. Truncated hits include a `memory_get_full id=` handle — call that tool to recover the full capture payload.";
    fn annotations() -> Option<rmcp::model::ToolAnnotations> {
        Some(rmcp::model::ToolAnnotations::from_raw(
            None,
            Some(true),
            None,
            Some(true),
            None,
        ))
    }
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
#[schemars(!deny_unknown_fields)]
#[serde(deny_unknown_fields)]
pub struct Browse {
    #[schemars(description = "Browse a specific conversation by ID", with = "Option<String>", transform = crate::catalog::optional_non_null)]
    conversation_id: Option<Uuid>,
    #[schemars(description = "Filter by agent (opus, grok, etc.)", transform = crate::catalog::optional_non_null)]
    agent: Option<String>,
    #[schemars(description = "Only results whose conversation carries this accepted key:value tag (e.g. project:acmeapp). See memory_tags.", transform = crate::catalog::optional_non_null)]
    tag: Option<String>,
    #[schemars(description = "Show messages after this time (ISO UTC timestamp). Bare dates are UTC midnight (= previous evening US local). Prefer window=.", with = "Option<String>", transform = crate::catalog::optional_non_null)]
    since: Option<DateTime<Utc>>,
    #[schemars(description = "Show messages before this time (ISO UTC timestamp). Same UTC gotcha as since. Prefer window=.", with = "Option<String>", transform = crate::catalog::optional_non_null)]
    before: Option<DateTime<Utc>>,
    #[schemars(description = "Shortcut for time-bounded windows — resolves to (since, before) in the SERVER local timezone, no TZ math required. last_7d/last_14d are rolling (prefer over this_week early in the week). Used only when neither since nor before is provided.", transform = crate::catalog::optional_non_null)]
    window: Option<BrowseWindow>,
    #[serde(default)]
    #[schemars(
        description = "Include role=tool rows (tool calls/results). Default false — browse returns user/assistant/system only so a limit=50 window is not flooded by tool noise. Set true when debugging capture, harness wiring, or tool failures."
    )]
    include_tools: bool,
    #[serde(default)]
    #[schemars(
        description = "Chronological order — asc (oldest first) or desc (newest first, default)"
    )]
    order: MemoryBrowseOrder,
    #[schemars(description = "Max messages to return (1–200, default: 50)", range(min = 1, max = 200), transform = crate::catalog::optional_non_null)]
    limit: Option<u32>,
}

impl crate::catalog::ToolInput for Browse {
    const NAME: &'static str = "memory_browse";
    const DESCRIPTION: &'static str = "Browse RivetHub conversation messages chronologically. Unlike memory_search (which ranks by relevance), this returns messages in time order. By default excludes role=tool rows (pass include_tools=true to see tool calls/results). Use to review what happened in a session, catch up on recent activity, or read a specific conversation by ID. For time-bounded questions (\"today\", \"yesterday\", \"this morning\"), prefer window= over raw since/before so local midnights convert correctly to UTC. Capture-truncated rows append `→ memory_get_full id=` — use that tool for the full payload.";
    fn annotations() -> Option<rmcp::model::ToolAnnotations> {
        Some(rmcp::model::ToolAnnotations::from_raw(
            None,
            Some(true),
            None,
            Some(true),
            None,
        ))
    }
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
#[schemars(!deny_unknown_fields)]
#[serde(deny_unknown_fields)]
pub struct Stats {
    #[schemars(description = "Filter stats to a specific agent (optional)", transform = crate::catalog::optional_non_null)]
    agent: Option<String>,
}

impl crate::catalog::ToolInput for Stats {
    const NAME: &'static str = "memory_stats";
    const DESCRIPTION: &'static str = "RivetHub memory system health check — message/summary counts, embedding queue depth, unsummarized messages, compaction status, missing summaries, and breakdowns by agent/role/kind. Use to diagnose memory issues or check if background jobs are keeping up.";
    fn annotations() -> Option<rmcp::model::ToolAnnotations> {
        Some(rmcp::model::ToolAnnotations::from_raw(
            None,
            Some(true),
            None,
            Some(true),
            None,
        ))
    }
}

#[derive(schemars::JsonSchema, Debug, Deserialize)]
#[schemars(!deny_unknown_fields)]
#[serde(deny_unknown_fields)]
pub struct GetFull {
    #[schemars(
        description = "Row id, as shown by memory_search / memory_browse truncation hints",
        length(min = 1)
    )]
    id: String,
}

impl crate::catalog::ToolInput for GetFull {
    const NAME: &'static str = "memory_get_full";
    const DESCRIPTION: &'static str = "Fetch the complete, untruncated payload for a memory row whose content or tool_result was elided at capture time (rows marked \"…[truncated]\" / \"⚠ truncated at capture\" by memory_search or memory_browse). Pass the row id from that hint. Re-reads the original capture JSONL line from disk — not a generic file reader. Mirrors the in-process `memory_get_full` tool.";
    fn annotations() -> Option<rmcp::model::ToolAnnotations> {
        Some(rmcp::model::ToolAnnotations::from_raw(
            None,
            Some(true),
            None,
            Some(true),
            None,
        ))
    }
}

pub(crate) fn limit(
    tool: &str,
    raw: Option<u32>,
    default: u32,
    maximum: u32,
) -> Result<Limit, Error> {
    if raw == Some(0) {
        return Err(Error::Invalid(format!(
            "Input validation error: Invalid arguments for tool {tool}: limit: Too small: expected number to be >=1"
        )));
    }
    if raw.is_some_and(|value| value > maximum) {
        return Err(Error::Invalid(format!(
            "Input validation error: Invalid arguments for tool {tool}: limit: Too big: expected number to be <={maximum}"
        )));
    }
    Ok(Limit::clamped(raw, default, 1..=maximum))
}

pub async fn search(
    store: &dyn MemoryStore,
    args: Search,
    zone: chrono_tz::Tz,
) -> Result<CallToolResult, Error> {
    let scope = match args.scope {
        Some(scope) => scope,
        None => MemoryScope::Both,
    };
    let expand = args.expand != Some(false);
    let query =
        SearchQuery {
            since: args.since,
            before: args.before,
            window: args.window.map(window),
            expand: args.expand,
            synthesize: args.synthesize,
            text: args.query,
            mode: search_mode(args.mode),
            scope: scope_of(scope),
            agent: args.agent,
            role: args.role,
            exclude_roles: Vec::new(),
            tool_name: args.tool_name,
            tag: args.tag.map(|tag| tag.parse()).transpose().map_err(
                |error: kernel::memory::TagLiteralError| Error::Invalid(error.to_string()),
            )?,
            limit: limit(Search::NAME, args.limit, 10, 50)?,
        };
    let outcome = store
        .search(query.clone().resolve_window(Utc::now(), &chrono::Local))
        .await?;
    let hits = hit_context(store, &outcome).await?;
    Ok(crate::text(crate::render_search::search(
        &query,
        &outcome,
        &hits,
        expand,
        crate::render_search::Clock {
            now: Utc::now(),
            zone,
        },
    )?))
}

async fn hit_context(
    store: &dyn MemoryStore,
    outcome: &kernel::memory::SearchOutcome,
) -> Result<crate::render_search::HitContext, Error> {
    let mut messages = std::collections::BTreeMap::new();
    for hit in outcome
        .results
        .iter()
        .filter(|hit| hit.source == kernel::memory::MemoryHitSource::Message)
    {
        let entry = store.get_full(&hit.id).await?.ok_or_else(|| {
            Error::Invalid(format!("message {} disappeared during search", hit.id))
        })?;
        messages.insert(hit.id.clone(), entry);
    }
    let sessions: std::collections::BTreeSet<String> = messages
        .values()
        .map(|entry| entry.session_id.clone())
        .collect();
    let tags = if sessions.is_empty() {
        std::collections::BTreeMap::new()
    } else {
        store
            .lookup_tags(kernel::memory::TagLookup {
                session_keys: sessions.iter().cloned().collect(),
                states: vec![kernel::memory::TagState::Accepted],
            })
            .await?
    };
    let mut carried = crate::provenance::Origins::new();
    let mut hit_keys = Vec::new();
    for hit in &outcome.results {
        let Some(key) = hit.session_id.as_deref() else {
            continue;
        };
        match outcome
            .summaries
            .get(&hit.id)
            .and_then(|detail| detail.provenance.clone())
        {
            Some(provenance) => {
                carried.insert(key.to_owned(), provenance);
            }
            None => hit_keys.push(key),
        }
    }
    let mut origins = crate::provenance::resolve(
        store,
        messages.values().chain(
            outcome
                .summaries
                .values()
                .flat_map(|detail| &detail.sources),
        ),
        hit_keys,
    )
    .await?;
    origins.extend(carried);
    Ok(crate::render_search::HitContext {
        messages,
        tags,
        origins,
    })
}

pub async fn browse(
    store: &dyn MemoryStore,
    args: Browse,
    zone: chrono_tz::Tz,
) -> Result<CallToolResult, Error> {
    if args.window.is_some() && (args.since.is_some() || args.before.is_some()) {
        return Err(Error::Invalid(
            "window cannot be combined with since or before".into(),
        ));
    }
    if let (Some(since), Some(before)) = (args.since, args.before)
        && since >= before
    {
        return Err(Error::Invalid("since must precede before".into()));
    }
    let filter =
        BrowseFilter {
            conversation_id: args.conversation_id,
            agent: args.agent,
            role: None,
            tool_name: None,
            tag: args.tag.map(|tag| tag.parse()).transpose().map_err(
                |error: kernel::memory::TagLiteralError| Error::Invalid(error.to_string()),
            )?,
            window: args.window.map(window),
            since: args.since,
            before: args.before,
            include_tools: args.include_tools,
            order: order(args.order),
            limit: limit(Browse::NAME, args.limit, 50, 200)?,
        }
        .resolve_window(chrono::Utc::now(), &chrono::Local);
    let entries = store.browse(filter.clone()).await?;
    let sessions = if entries.is_empty() {
        std::collections::BTreeMap::new()
    } else {
        store
            .lookup_tags(kernel::memory::TagLookup {
                session_keys: entries
                    .iter()
                    .map(|entry| entry.session_id.clone())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect(),
                states: vec![kernel::memory::TagState::Accepted],
            })
            .await?
    };
    let origins = crate::provenance::resolve(store, &entries, []).await?;
    let mut display_filter = filter;
    display_filter.window = args.window.map(window);
    Ok(crate::text(crate::render_memory::browse(
        &entries,
        &display_filter,
        &sessions,
        &origins,
        zone,
    )?))
}

pub async fn stats(store: &dyn MemoryStore, args: Stats) -> Result<CallToolResult, Error> {
    if args.agent.is_some() {
        return Err(Error::Refused("MemoryStore stats has no agent filter"));
    }
    Ok(crate::text(crate::render_memory::stats(
        &store.stats().await?,
    )?))
}

pub async fn get_full(store: &dyn MemoryStore, args: GetFull) -> Result<CallToolResult, Error> {
    if args.id.is_empty() {
        return Err(Error::Invalid(format!(
            "Input validation error: Invalid arguments for tool {}: id: Too small: expected string to have >=1 characters",
            GetFull::NAME
        )));
    }
    let entry = store.get_full(&args.id).await?;
    let origins = crate::provenance::resolve(store, &entry, []).await?;
    Ok(crate::text(crate::render_memory::get_full(
        &args.id,
        entry.as_ref(),
        &origins,
    )?))
}

const fn search_mode(mode: SearchMode) -> kernel::memory::SearchMode {
    match mode {
        SearchMode::Hybrid => kernel::memory::SearchMode::Hybrid,
        SearchMode::Fts => kernel::memory::SearchMode::Fts,
        SearchMode::Vector => kernel::memory::SearchMode::Vector,
        SearchMode::Trigram => kernel::memory::SearchMode::Trigram,
        SearchMode::Regex => kernel::memory::SearchMode::Regex,
    }
}

const fn scope_of(scope: MemoryScope) -> kernel::memory::MemoryScope {
    match scope {
        MemoryScope::Messages => kernel::memory::MemoryScope::Messages,
        MemoryScope::Summaries => kernel::memory::MemoryScope::Summaries,
        MemoryScope::Both => kernel::memory::MemoryScope::Both,
    }
}

pub(crate) const fn window(window: BrowseWindow) -> kernel::memory::BrowseWindow {
    match window {
        BrowseWindow::Today => kernel::memory::BrowseWindow::Today,
        BrowseWindow::Yesterday => kernel::memory::BrowseWindow::Yesterday,
        BrowseWindow::ThisMorning => kernel::memory::BrowseWindow::ThisMorning,
        BrowseWindow::ThisWeek => kernel::memory::BrowseWindow::ThisWeek,
        BrowseWindow::Last24h => kernel::memory::BrowseWindow::Last24h,
        BrowseWindow::Last7d => kernel::memory::BrowseWindow::Last7d,
        BrowseWindow::Last14d => kernel::memory::BrowseWindow::Last14d,
    }
}

const fn order(order: MemoryBrowseOrder) -> kernel::memory::MemoryBrowseOrder {
    match order {
        MemoryBrowseOrder::Asc => kernel::memory::MemoryBrowseOrder::Asc,
        MemoryBrowseOrder::Desc => kernel::memory::MemoryBrowseOrder::Desc,
    }
}
