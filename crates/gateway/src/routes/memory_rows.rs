use kernel::memory::MemoryEntry;
use wire::gateway::memory::MemoryBrowseMessage;
use wire::gateway::memory::MemoryBrowseResponse;
use wire::gateway::memory::MemorySearchHit;

use crate::Gateway;
use crate::error::ApiError;
use crate::memory_body;

pub(super) async fn search_origins(
    gateway: &Gateway,
    outcome: &kernel::memory::SearchOutcome,
) -> Result<memory_body::Origins, ApiError> {
    memory_body::resolve(
        gateway.store().as_ref(),
        &outcome.expanded,
        outcome
            .results
            .iter()
            .filter_map(|hit| hit.session_id.as_deref()),
    )
    .await
}

pub(super) fn search_hit(
    result: kernel::memory::MemorySearchResult,
    origins: &memory_body::Origins,
) -> MemorySearchHit {
    let origin = result
        .session_id
        .as_deref()
        .and_then(|session| origins.get(session));
    MemorySearchHit {
        owner: origin.and_then(|origin| origin.team_owner.clone()),
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

pub(super) async fn browse_response(
    gateway: &Gateway,
    entries: Vec<MemoryEntry>,
) -> Result<MemoryBrowseResponse, ApiError> {
    let origins = memory_body::resolve(gateway.store().as_ref(), &entries, []).await?;
    Ok(MemoryBrowseResponse {
        messages: entries
            .into_iter()
            .map(|entry| browse_message(entry, &origins))
            .collect(),
    })
}

pub(super) fn browse_message(
    entry: MemoryEntry,
    origins: &memory_body::Origins,
) -> MemoryBrowseMessage {
    let origin = origins.get(&entry.session_id);
    MemoryBrowseMessage {
        owner: origin.and_then(|origin| origin.team_owner.clone()),
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
