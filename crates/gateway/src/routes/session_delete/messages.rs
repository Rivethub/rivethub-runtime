use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use kernel::memory::{MemoryEntry, MessageCursor, MessagesQuery};
use serde::Deserialize;
use wire::gateway::session_catalog::{SessionMessage, SessionMessagesPage};

use super::NOT_FOUND;
use super::catalog::catalog;
use crate::Gateway;
use crate::error::ApiError;
use crate::tenancy::UserId;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MessagesParams {
    cursor: Option<String>,
    limit: Option<u32>,
}

fn message(entry: MemoryEntry) -> Result<SessionMessage, ApiError> {
    let id = entry.id.as_deref().ok_or(ApiError::Internal)?;
    let id = uuid::Uuid::parse_str(id).map_err(|_| ApiError::Internal)?;
    Ok(SessionMessage {
        id,
        role: entry.role,
        agent: entry.agent,
        content: entry.content,
        tool_name: entry.tool_name,
        tool_args: entry.tool_args,
        tool_result: entry.tool_result,
        created_at: entry.created_at.ok_or(ApiError::Internal)?,
    })
}

pub(super) async fn messages(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
    Query(params): Query<MessagesParams>,
) -> Result<Json<SessionMessagesPage>, ApiError> {
    let key = crate::session_address::resolve(&gateway, &segment).await?;
    let catalog = catalog(&gateway)?;
    let readable = crate::session_access::seen_one(&gateway, &user, &key)
        .await?
        .readable();
    if !readable || catalog.get(&key).await?.is_none() {
        return Err(ApiError::NotFound(NOT_FOUND));
    }
    let after = params
        .cursor
        .as_deref()
        .map(|text| {
            MessageCursor::decode(text).map_err(|error| ApiError::BadRequest(error.to_string()))
        })
        .transpose()?;
    let page = catalog
        .messages(&key, &MessagesQuery::new(after, params.limit))
        .await?;
    let quoted_from = page
        .items
        .iter()
        .find_map(|entry| entry.provenance.as_ref())
        .map(crate::memory_body::provenance_body);
    Ok(Json(SessionMessagesPage {
        session: key,
        total: page.total,
        next: page.next.as_ref().map(MessageCursor::encode),
        quoted_from,
        items: page
            .items
            .into_iter()
            .map(message)
            .collect::<Result<_, _>>()?,
    }))
}
