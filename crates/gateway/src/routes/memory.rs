use std::str::FromStr;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::State;
use axum::handler::Handler;
use axum::routing;
use axum::routing::MethodRouter;
use kernel::memory::BrowseFilter;
use kernel::memory::EmbedModelMismatch;
use kernel::memory::Limit;
use kernel::memory::MemoryEntry;
use kernel::memory::SearchQuery;
use kernel::memory::TagLiteral;
use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;
use wire::capture::CaptureResult;
use wire::gateway::OkReply;
use wire::gateway::memory::EmbedHealth;
use wire::gateway::memory::EmbedStatus;
use wire::gateway::memory::MemoryAppendToolArgs;
use wire::gateway::memory::MemoryBrowseMessage;
use wire::gateway::memory::MemoryBrowseResponse;
use wire::gateway::memory::MemoryBrowseToolArgs;
use wire::gateway::memory::MemoryContractResponse;
use wire::gateway::memory::MemoryGetFullToolArgs;
use wire::gateway::memory::MemoryHealthResponse;
use wire::gateway::memory::MemoryHealthStatus;
use wire::gateway::memory::MemoryIngestSessionToolArgs;
use wire::gateway::memory::MemoryScope;
use wire::gateway::memory::MemorySearchHit;
use wire::gateway::memory::MemorySearchResponse;
use wire::gateway::memory::MemorySearchToolArgs;
use wire::gateway::memory::MemoryStatsResponse;
use wire::gateway::memory::MemoryToolResponse;
use wire::gateway::memory::SearchMode;
use wire::json::JsonObject;
use wire::message::ToolResult;

use crate::Gateway;
use crate::error::ApiError;
use crate::extract::BeforeParam;
use crate::extract::Body;
use crate::extract::Query;
use crate::extract::RawLimit;
use crate::extract::RoleParam;
use crate::extract::RolesParam;
use crate::extract::ScopeParam;
use crate::extract::SinceParam;
use crate::extract::TagFilter;
use crate::extract::WindowParam;
use crate::memory_body;
use crate::tenancy::UserId;

const SEARCH_LIMIT_DEFAULT: u32 = 20;
const SEARCH_LIMIT_MAX: u32 = 50;
const SEARCH_TOOL_LIMIT_DEFAULT: u32 = 10;
const BROWSE_LIMIT_DEFAULT: u32 = 50;
const BROWSE_LIMIT_MAX: u32 = 200;
const MODEL_MISMATCH_IMPACT: &str = "New embeddings are refused so the stored vectors stay comparable; run rh memory reembed to re-embed with the configured model.";
pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route(
            "/search",
            routing::get(search).fallback(super::method_not_allowed),
        )
        .route(
            "/browse",
            routing::get(browse).fallback(super::method_not_allowed),
        )
        .route(
            "/stats",
            routing::get(stats).fallback(super::method_not_allowed),
        )
        .route(
            "/health",
            routing::get(health).fallback(super::method_not_allowed),
        )
        .route(
            "/contract",
            routing::get(contract).fallback(super::method_not_allowed),
        )
        .route(
            "/upkeep",
            routing::get(super::tidy::upkeep).fallback(super::method_not_allowed),
        )
        .route(
            "/models",
            routing::get(super::memory_models::list).fallback(super::method_not_allowed),
        )
        .route(
            "/flagged",
            routing::get(super::flagged::list).fallback(super::method_not_allowed),
        )
        .route(
            "/flagged/{id}/retry",
            routing::post(super::flagged::retry).fallback(super::method_not_allowed),
        )
        .route(
            "/flagged/{id}/dismiss",
            routing::post(super::flagged::dismiss).fallback(super::method_not_allowed),
        )
        .route(
            "/summaries/{session}",
            routing::get(super::memory_archive::summaries).fallback(super::method_not_allowed),
        )
        .route(
            "/summaries/{session}/{summary}",
            routing::get(super::memory_archive::summary).fallback(super::method_not_allowed),
        )
        .route(
            "/summaries/{session}/{summary}/children",
            routing::get(super::memory_archive::children).fallback(super::method_not_allowed),
        )
        .route(
            "/conversations/{id}/summaries",
            routing::get(super::memory_archive::conversation_summaries)
                .fallback(super::method_not_allowed),
        )
        .route(
            "/export",
            routing::get(super::memory_archive::export).fallback(super::method_not_allowed),
        )
        .route("/tool/memory_search", tool_route(tool_search))
        .route("/tool/memory_browse", tool_route(tool_browse))
        .route("/tool/memory_stats", tool_route(tool_stats))
        .route("/tool/memory_get_full", tool_route(tool_get_full))
        .route("/tool/memory_append", tool_route(tool_append))
        .route(
            "/tool/memory_ingest_session",
            tool_route(tool_ingest_session),
        )
        .route(
            "/tool/{name}",
            routing::post(unknown_tool).fallback(super::method_not_allowed),
        )
        .nest("/tags", super::tags::router())
        .fallback(unknown)
}

fn tool_route<H, T>(handler: H) -> MethodRouter<Arc<Gateway>>
where
    H: Handler<T, Arc<Gateway>>,
    T: 'static,
{
    routing::post(handler)
        .fallback(super::method_not_allowed)
        .layer(DefaultBodyLimit::max(super::BODY_LIMIT))
}

async fn unknown_tool() -> ApiError {
    ApiError::NotFound("unknown memory tool")
}

async fn unknown() -> ApiError {
    ApiError::NotFound("unknown memory resource")
}

#[derive(Debug, Default, Deserialize)]
struct SearchParams {
    mode: Option<SearchMode>,
    since: Option<SinceParam>,
    before: Option<BeforeParam>,
    window: Option<WindowParam>,
    expand: Option<bool>,
    synthesize: Option<bool>,
    q: Option<String>,
    scope: Option<ScopeParam>,
    agent: Option<String>,
    role: Option<RoleParam>,
    exclude_roles: Option<RolesParam>,
    tool_name: Option<String>,
    tag: Option<TagFilter>,
    limit: Option<RawLimit>,
    snippet: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct BrowseParams {
    role: Option<RoleParam>,
    agent: Option<String>,
    tool_name: Option<String>,
    tag: Option<TagFilter>,
    window: Option<WindowParam>,
    since: Option<SinceParam>,
    before: Option<BeforeParam>,
    limit: Option<RawLimit>,
}

async fn search(
    State(gateway): State<Arc<Gateway>>,
    Query(params): Query<SearchParams>,
) -> Result<Json<MemorySearchResponse>, ApiError> {
    let text = params
        .q
        .filter(|q| !q.is_empty())
        .ok_or_else(|| ApiError::BadRequest("q required".to_owned()))?;
    gateway.count(crate::account::UsageEvent::Search);
    let scope = params.scope.map_or(MemoryScope::Both, ScopeParam::get);
    let query = SearchQuery {
        since: params.since.map(SinceParam::get),
        before: params.before.map(BeforeParam::get),
        window: params
            .window
            .map(|window| memory_body::browse_window(window.get())),
        expand: params.expand,
        synthesize: params.synthesize,
        text: text.clone(),
        mode: memory_body::search_mode(params.mode.unwrap_or(match scope {
            MemoryScope::Messages => SearchMode::Fts,
            MemoryScope::Summaries | MemoryScope::Both => SearchMode::Hybrid,
        })),
        scope: memory_body::memory_scope(scope),
        agent: params.agent,
        role: params.role.map(RoleParam::get),
        exclude_roles: params
            .exclude_roles
            .map(RolesParam::get)
            .unwrap_or_default(),
        tool_name: params.tool_name,
        tag: params.tag.map(TagFilter::get),
        limit: Limit::clamped(
            params.limit.map(RawLimit::get),
            SEARCH_LIMIT_DEFAULT,
            1..=SEARCH_LIMIT_MAX,
        ),
    };
    let results = gateway
        .store()
        .search(query.resolve_window(chrono::Utc::now(), &chrono::Local))
        .await?;
    let origins = search_origins(&gateway, &results).await?;
    let snippets = params.snippet == Some(true);
    Ok(Json(MemorySearchResponse {
        scope,
        degraded: results.degraded.map(memory_body::memory_degraded_body),
        fallback: results.fallback.map(memory_body::memory_fallback_body),
        expanded: results
            .expanded
            .into_iter()
            .map(|entry| memory_body::memory_entry_body(entry, &origins))
            .collect(),
        synthesis: results.synthesis,
        results: results
            .results
            .into_iter()
            .map(|mut hit| {
                if snippets {
                    hit.content = super::memory_snippet::snippet(&hit.content, &text);
                }
                search_hit(hit, &origins)
            })
            .collect(),
        query: text,
    }))
}

async fn search_origins(
    gateway: &Gateway,
    outcome: &kernel::memory::SearchOutcome,
) -> Result<memory_body::Origins, ApiError> {
    memory_body::origins_of(
        gateway.store().as_ref(),
        outcome
            .results
            .iter()
            .filter_map(|hit| hit.session_id.as_deref())
            .chain(
                outcome
                    .expanded
                    .iter()
                    .map(|entry| entry.session_id.as_str()),
            ),
    )
    .await
}

fn search_hit(
    result: kernel::memory::MemorySearchResult,
    origins: &memory_body::Origins,
) -> MemorySearchHit {
    let origin = result
        .session_id
        .as_deref()
        .and_then(|session| origins.get(session));
    MemorySearchHit {
        chunk: result
            .chunk
            .map(|chunk| memory_body::memory_chunk_match_body(chunk, origin)),
        id: result.id,
        source: memory_body::memory_hit_source_body(result.source),
        content: memory_body::quoted(origin, result.content),
        created_at: result.created_at,
        score: result.relevance_score,
        role: result.role,
        agent: result.agent,
        kind: None,
        tool_name: None,
        conversation_id: None,
        session_id: result.session_id,
        title: result.title,
        fallback: None,
        tags: None,
        provenance: origin.map(memory_body::provenance_body),
    }
}

async fn browse(
    State(gateway): State<Arc<Gateway>>,
    Query(params): Query<BrowseParams>,
) -> Result<Json<MemoryBrowseResponse>, ApiError> {
    let filter = BrowseFilter {
        conversation_id: None,
        agent: params.agent,
        role: params.role.map(RoleParam::get),
        tool_name: params.tool_name,
        tag: params.tag.map(TagFilter::get),
        window: params
            .window
            .map(|window| memory_body::browse_window(window.get())),
        since: params.since.map(SinceParam::get),
        before: params.before.map(BeforeParam::get),
        include_tools: false,
        order: kernel::memory::MemoryBrowseOrder::Desc,
        limit: Limit::clamped(
            params.limit.map(RawLimit::get),
            BROWSE_LIMIT_DEFAULT,
            1..=BROWSE_LIMIT_MAX,
        ),
    };
    let entries = gateway
        .store()
        .browse(filter.resolve_window(chrono::Utc::now(), &chrono::Local))
        .await?;
    Ok(Json(browse_response(&gateway, entries).await?))
}

async fn browse_response(
    gateway: &Gateway,
    entries: Vec<MemoryEntry>,
) -> Result<MemoryBrowseResponse, ApiError> {
    let origins = memory_body::origins_of(
        gateway.store().as_ref(),
        entries.iter().map(|entry| entry.session_id.as_str()),
    )
    .await?;
    Ok(MemoryBrowseResponse {
        messages: entries
            .into_iter()
            .map(|entry| browse_message(entry, &origins))
            .collect(),
    })
}

fn browse_message(entry: MemoryEntry, origins: &memory_body::Origins) -> MemoryBrowseMessage {
    let origin = origins.get(&entry.session_id);
    MemoryBrowseMessage {
        id: entry.id.unwrap_or_default(),
        role: entry.role,
        agent: Some(entry.agent),
        content: memory_body::quoted(origin, entry.content),
        created_at: entry.created_at.unwrap_or_default(),
        conversation_id: None,
        session_id: Some(entry.session_id),
        tool_name: entry.tool_name,
        tags: None,
        provenance: origin.map(memory_body::provenance_body),
    }
}

async fn stats(State(gateway): State<Arc<Gateway>>) -> Result<Json<MemoryStatsResponse>, ApiError> {
    Ok(Json(memory_body::memory_stats_response_body(
        gateway.store().stats().await?,
    )))
}

async fn contract() -> Json<MemoryContractResponse> {
    Json(MemoryContractResponse {
        contract_version: wire::contract::MEMORY_CONTRACT,
    })
}

async fn health(
    State(gateway): State<Arc<Gateway>>,
) -> Result<Json<MemoryHealthResponse>, ApiError> {
    let store = gateway.store();
    let counts = store.stats().await?;
    let embeddings = embed_health(
        store.embeddings_armed(),
        store.embed_model_mismatch().await?,
    );
    let overall = if embeddings.status == EmbedStatus::Ok && counts.failed_embeddings == 0 {
        MemoryHealthStatus::Ok
    } else {
        MemoryHealthStatus::Degraded
    };
    Ok(Json(MemoryHealthResponse {
        observed_at: Some(chrono::Utc::now()),
        queue_status: None,
        queues: None,
        summarizer: None,
        failed_embeddings: None,
        skipped_embeddings: None,
        capture: None,
        status: overall,
        embeddings,
        embed_queue_depth: counts.embed_queue_depth,
    }))
}

fn embed_health(armed: bool, mismatch: Option<EmbedModelMismatch>) -> EmbedHealth {
    if let Some(mismatch) = mismatch {
        return EmbedHealth {
            status: EmbedStatus::Unavailable,
            error: Some(format!(
                "stored vectors were written by embedding model {}, the configured model is {}",
                mismatch.stored, mismatch.configured
            )),
            impact: Some(MODEL_MISMATCH_IMPACT.to_owned()),
            checked_at: None,
        };
    }
    EmbedHealth {
        status: if armed {
            EmbedStatus::Ok
        } else {
            EmbedStatus::Unavailable
        },
        error: None,
        impact: None,
        checked_at: None,
    }
}

async fn tool_search(
    State(gateway): State<Arc<Gateway>>,
    Body(args): Body<MemorySearchToolArgs>,
) -> Result<Json<MemoryToolResponse>, ApiError> {
    Ok(tool_response(search_tool(&gateway, args).await?))
}

async fn tool_browse(
    State(gateway): State<Arc<Gateway>>,
    Body(args): Body<MemoryBrowseToolArgs>,
) -> Result<Json<MemoryToolResponse>, ApiError> {
    Ok(tool_response(browse_tool(&gateway, args).await?))
}

async fn tool_stats(
    State(gateway): State<Arc<Gateway>>,
) -> Result<Json<MemoryToolResponse>, ApiError> {
    let stats = gateway.store().stats().await?;
    Ok(tool_response(serialize_tool(
        &memory_body::memory_stats_response_body(stats),
    )?))
}

async fn tool_get_full(
    State(gateway): State<Arc<Gateway>>,
    Body(args): Body<MemoryGetFullToolArgs>,
) -> Result<Json<MemoryToolResponse>, ApiError> {
    Ok(tool_response(get_full_tool(&gateway, args).await))
}

async fn tool_append(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Body(args): Body<MemoryAppendToolArgs>,
) -> Result<Json<MemoryToolResponse>, ApiError> {
    Ok(tool_response(append_tool(&gateway, &user, args).await?))
}

async fn tool_ingest_session(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Body(args): Body<MemoryIngestSessionToolArgs>,
) -> Result<Json<MemoryToolResponse>, ApiError> {
    Ok(tool_response(
        ingest_session_tool(&gateway, &user, args).await?,
    ))
}

fn tool_response(text: String) -> Json<MemoryToolResponse> {
    Json(MemoryToolResponse {
        ok: true,
        result: ToolResult::Text(text),
    })
}

fn serialize_tool<T: Serialize>(value: &T) -> Result<String, ApiError> {
    serde_json::to_string(value).map_err(ApiError::Serialize)
}

async fn search_tool(gateway: &Gateway, args: MemorySearchToolArgs) -> Result<String, ApiError> {
    let mode = match args.mode {
        Some(mode) => mode,
        None => SearchMode::Hybrid,
    };
    let tag = match args.tag.as_deref().map(TagLiteral::from_str) {
        Some(Ok(tag)) => Some(tag),
        Some(Err(error)) => return Ok(format!("Search failed: {error}")),
        None => None,
    };
    let scope = args.scope.unwrap_or(MemoryScope::Both);
    let query = SearchQuery {
        since: args.since,
        before: args.before,
        window: args.window.map(memory_body::browse_window),
        expand: args.expand,
        synthesize: args.synthesize,
        text: args.query.clone(),
        mode: memory_body::search_mode(mode),
        scope: memory_body::memory_scope(scope),
        agent: args.agent,
        role: args.role,
        exclude_roles: Vec::new(),
        tool_name: args.tool_name,
        tag,
        limit: Limit::clamped(args.limit, SEARCH_TOOL_LIMIT_DEFAULT, 1..=SEARCH_LIMIT_MAX),
    };
    let results = gateway
        .store()
        .search(query.resolve_window(chrono::Utc::now(), &chrono::Local))
        .await?;
    let origins = search_origins(gateway, &results).await?;
    let response = MemorySearchResponse {
        query: args.query,
        scope,
        degraded: results.degraded.map(memory_body::memory_degraded_body),
        fallback: results.fallback.map(memory_body::memory_fallback_body),
        expanded: results
            .expanded
            .into_iter()
            .map(|entry| memory_body::memory_entry_body(entry, &origins))
            .collect(),
        synthesis: results.synthesis,
        results: results
            .results
            .into_iter()
            .map(|hit| search_hit(hit, &origins))
            .collect(),
    };
    serialize_tool(&response)
}

async fn browse_tool(gateway: &Gateway, args: MemoryBrowseToolArgs) -> Result<String, ApiError> {
    let conversation_id = match args.conversation_id.as_deref().map(Uuid::from_str) {
        Some(Ok(id)) => Some(id),
        Some(Err(_)) => {
            return Ok("Browse failed: conversation_id must be a uuid".to_owned());
        }
        None => None,
    };
    let tag = match args.tag.as_deref().map(TagLiteral::from_str) {
        Some(Ok(tag)) => Some(tag),
        Some(Err(error)) => return Ok(format!("Browse failed: {error}")),
        None => None,
    };
    let filter = BrowseFilter {
        conversation_id,
        agent: args.agent,
        role: None,
        tool_name: None,
        tag,
        window: args.window.map(memory_body::browse_window),
        since: args.since,
        before: args.before,
        include_tools: args.include_tools.unwrap_or(false),
        order: memory_body::memory_browse_order(args.order.unwrap_or_default()),
        limit: Limit::clamped(args.limit, BROWSE_LIMIT_DEFAULT, 1..=BROWSE_LIMIT_MAX),
    };
    let entries = gateway
        .store()
        .browse(filter.resolve_window(chrono::Utc::now(), &chrono::Local))
        .await?;
    serialize_tool(&browse_response(gateway, entries).await?)
}

async fn get_full_tool(gateway: &Gateway, args: MemoryGetFullToolArgs) -> String {
    let Some(id) = args.id.filter(|id| !id.is_empty()) else {
        return "memory_get_full: id is required (string).".to_owned();
    };
    let entry = match gateway.store().get_full(&id).await {
        Ok(entry) => entry,
        Err(error) => return format!("memory_get_full failed: {error}"),
    };
    let origins = match memory_body::origins_of(
        gateway.store().as_ref(),
        entry.iter().map(|entry| entry.session_id.as_str()),
    )
    .await
    {
        Ok(origins) => origins,
        Err(error) => return format!("memory_get_full failed: {error}"),
    };
    let origin = entry
        .as_ref()
        .and_then(|entry| origins.get(&entry.session_id));
    memory_body::quoted(origin, get_full_text(&id, entry))
}

fn get_full_text(id: &str, entry: Option<MemoryEntry>) -> String {
    let Some(entry) = entry else {
        return format!("No message with id {id}.");
    };
    if truncated(entry.metadata.as_ref()) {
        return "Row is truncated but carries no disk pointer (pre-#196 capture, or a non-grok source) — the elided tail is unrecoverable."
            .to_owned();
    }
    let mut text = String::from("(row was not truncated — stored payload is complete)\n\n");
    text.push_str(&entry.content);
    if entry.tool_name.is_some()
        && let Some(tool_result) = entry.tool_result
    {
        text.push_str("\n\n");
        text.push_str(&tool_result);
    }
    text
}

fn truncated(metadata: Option<&JsonObject>) -> bool {
    metadata
        .and_then(|metadata| metadata.get("truncated"))
        .is_some_and(|value| matches!(serde_json::from_value::<bool>(value.clone()), Ok(true)))
}

async fn append_tool(
    gateway: &Gateway,
    user: &UserId,
    args: MemoryAppendToolArgs,
) -> Result<String, ApiError> {
    let entry = MemoryEntry {
        id: args.event_id,
        session_id: args.session_id,
        agent: args.agent.unwrap_or_default(),
        channel: args.channel.unwrap_or_default(),
        role: args.role,
        content: args.content,
        tool_name: args.tool_name,
        tool_args: args.tool_args,
        tool_result: args.tool_result,
        metadata: None,
        created_at: None,
    };
    gateway.store().append(user.owner(), vec![entry]).await?;
    serialize_tool(&OkReply { ok: true })
}

async fn ingest_session_tool(
    gateway: &Gateway,
    user: &UserId,
    args: MemoryIngestSessionToolArgs,
) -> Result<String, ApiError> {
    let entries = args
        .messages
        .into_iter()
        .map(|message| MemoryEntry {
            id: None,
            session_id: args.session_id.clone(),
            agent: args.agent.clone().unwrap_or_default(),
            channel: args.channel.clone().unwrap_or_default(),
            role: message.role,
            content: message.content,
            tool_name: None,
            tool_args: None,
            tool_result: None,
            metadata: None,
            created_at: None,
        })
        .collect();
    let inserted = gateway.store().append(user.owner(), entries).await?;
    let reply = CaptureResult {
        ok: true,
        conversation_id: args.session_id,
        inserted,
        skipped: 0,
    };
    serialize_tool(&reply)
}
