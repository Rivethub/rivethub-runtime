mod previews;

use std::sync::Arc;

use axum::extract::{Path, RawQuery, State};
use axum::routing::{get, put};
use axum::{Json, Router};
use chrono::DateTime;
use kernel::transcript::{ListLimit, SessionRenameError, StoreSession, TranscriptWindow};
use wire::gateway::sessions::{
    HarnessSessionResponse, HarnessSessionTitleRequest, HarnessSessionTranscriptResponse,
    HarnessToolOutput,
};
use wire::harness::{HarnessCode, HarnessId, HarnessSessionList, HarnessSessionSummary};
use wire::session_id::SessionId;

use crate::Gateway;
use crate::error::ApiError;
use crate::extract::Body;
use crate::history::{Resolved, authorize, store_summary};
use crate::tenancy::UserId;

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route("/api/harnesses/{harness}/sessions", get(plane))
        .route(
            "/api/harness-sessions/previews",
            get(previews::previews).fallback(super::method_not_allowed),
        )
        .route(
            "/api/harness-sessions/{session}",
            get(summary).fallback(super::method_not_allowed),
        )
        .route(
            "/api/harness-sessions/{session}/transcript",
            get(transcript),
        )
        .route(
            "/api/harness-sessions/{session}/title",
            put(rename).fallback(super::method_not_allowed),
        )
        .route(
            "/api/harness-sessions/{session}/tools/{tool}/output",
            get(tool_output),
        )
}

pub(super) fn first_param(query: Option<&str>, name: &str) -> Result<Option<String>, ApiError> {
    let Some(query) = query else {
        return Ok(None);
    };
    for pair in query.split('&').filter(|pair| !pair.is_empty()) {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if super::notices::form_text(key)? == name {
            return super::notices::form_text(value).map(Some);
        }
    }
    Ok(None)
}

async fn listed(gateway: &Gateway, harnesses: &[HarnessId], limit: ListLimit) -> Vec<StoreSession> {
    let rows = match gateway.session_lister() {
        Some(lister) => lister.list(harnesses, limit).await,
        None => Vec::new(),
    };
    let keys: Vec<String> = rows
        .iter()
        .map(|row| SessionId::new(row.harness, &row.id).as_str().to_owned())
        .collect();
    let deleted = super::session_delete::tombstoned(gateway, &keys).await;
    let trashed = super::session_delete::trashed(gateway, &keys).await;
    rows.into_iter()
        .zip(keys)
        .filter(|(row, key)| {
            !trashed.contains(key)
                && !super::session_delete::hidden(
                    &deleted,
                    key,
                    DateTime::from_timestamp_millis(row.updated_at),
                )
        })
        .map(|(row, _)| row)
        .collect()
}

fn row_id(row: &StoreSession) -> &str {
    &row.id
}

fn row_parent(row: &StoreSession) -> Option<&str> {
    row.parent_session_id.as_deref()
}

pub(super) fn visible_rows(
    gateway: &Gateway,
    user: &UserId,
    rows: Vec<StoreSession>,
) -> Vec<StoreSession> {
    let registry = gateway.sessions();
    registry.stamp_nested(&rows, row_id, row_parent);
    registry.filter(user, rows, row_id, row_parent)
}

async fn plane(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(harness): Path<String>,
) -> Result<Json<HarnessSessionList>, ApiError> {
    let harness = harness
        .parse::<HarnessId>()
        .map_err(|_| ApiError::Missing(format!("unknown harness: {harness}")))?;
    if gateway.session_lister().is_none() {
        return Err(ApiError::Code(
            HarnessCode::CapabilityUnsupported,
            format!("{harness} does not list sessions"),
        ));
    }
    let rows = listed(&gateway, &[harness], ListLimit::per_harness_route()).await;
    let rows = visible_rows(&gateway, &user, rows);
    let mut sessions = Vec::with_capacity(rows.len());
    for row in &rows {
        sessions.push(store_summary(&gateway, row).await);
    }
    Ok(Json(HarnessSessionList { sessions }))
}

async fn summary(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
) -> Result<Json<HarnessSessionResponse>, ApiError> {
    let resolved = gateway.history().resolve(&gateway, &segment).await?;
    authorize(&gateway, &user, &resolved).await?;
    let summary = described(&gateway, &resolved).await?;
    Ok(Json(HarnessSessionResponse {
        summary,
        switches: None,
        redirected_to: resolved.redirected_to,
        running_elsewhere: None,
    }))
}

async fn described(
    gateway: &Gateway,
    resolved: &Resolved,
) -> Result<HarnessSessionSummary, ApiError> {
    let described = match gateway.session_lister() {
        Some(lister) => {
            lister
                .describe(resolved.harness, &resolved.native, None)
                .await?
        }
        None => None,
    };
    match described {
        Some(row) => Ok(store_summary(gateway, &row).await),
        None => Err(ApiError::Missing(format!(
            "unknown session {}",
            resolved.session
        ))),
    }
}

async fn transcript(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
    RawQuery(query): RawQuery,
) -> Result<Json<HarnessSessionTranscriptResponse>, ApiError> {
    let window = transcript_window(query.as_deref())?;
    let resolved = gateway.history().resolve(&gateway, &segment).await?;
    authorize(&gateway, &user, &resolved).await?;
    let read = gateway
        .history()
        .transcript(&gateway, &resolved, window)
        .await?;
    Ok(Json(HarnessSessionTranscriptResponse {
        session_id: resolved.session,
        harness_id: resolved.harness,
        turns: crate::history::body::turns(read.read.turns),
        context_window: read.stamp.window,
        compact_at: read.stamp.compact_at,
        context_source: read.stamp.source,
        redirected_to: resolved.redirected_to,
        earlier: read.read.earlier,
        context_used: read.read.context,
    }))
}

fn transcript_window(query: Option<&str>) -> Result<TranscriptWindow, ApiError> {
    let offset = |name: &str| -> Result<Option<u64>, ApiError> {
        first_param(query, name)?
            .map(|value| {
                value
                    .parse::<u64>()
                    .map_err(|_| ApiError::BadRequest(format!("{name} must be a byte offset")))
            })
            .transpose()
    };
    match (offset("before")?, offset("from")?) {
        (Some(_), Some(_)) => Err(ApiError::BadRequest(
            "a transcript read takes before or from, not both".to_owned(),
        )),
        (Some(before), None) => Ok(TranscriptWindow::Before(before)),
        (None, Some(from)) => Ok(TranscriptWindow::From(from)),
        (None, None) => Ok(TranscriptWindow::Latest),
    }
}

async fn rename(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
    Body(request): Body<HarnessSessionTitleRequest>,
) -> Result<Json<HarnessSessionResponse>, ApiError> {
    let title = request.title.trim();
    if title.is_empty() {
        return Err(ApiError::Code(
            HarnessCode::BadRequest,
            "title must not be empty".to_owned(),
        ));
    }
    let resolved = gateway.history().resolve(&gateway, &segment).await?;
    authorize(&gateway, &user, &resolved).await?;
    let lister = gateway
        .session_lister()
        .ok_or_else(|| ApiError::Unavailable("harness session stores unavailable".to_owned()))?;
    let row = lister
        .rename(resolved.harness, &resolved.native, title)
        .await
        .map_err(|error| match error {
            SessionRenameError::Unsupported(harness) => ApiError::Code(
                HarnessCode::CapabilityUnsupported,
                format!("{harness} sessions cannot be renamed from RivetHub"),
            ),
            SessionRenameError::Missing => {
                ApiError::Missing(format!("unknown session {}", resolved.session))
            }
            SessionRenameError::Write { .. } => ApiError::Unavailable(error.to_string()),
            SessionRenameError::Store(error) => ApiError::Transcript(error),
        })?;
    let summary = store_summary(&gateway, &row).await;
    Ok(Json(HarnessSessionResponse {
        summary,
        switches: None,
        redirected_to: resolved.redirected_to,
        running_elsewhere: None,
    }))
}

async fn tool_output(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path((segment, tool)): Path<(String, String)>,
) -> Result<Json<HarnessToolOutput>, ApiError> {
    let resolved = gateway
        .history()
        .resolve_canonical(&gateway, &segment)
        .await?;
    authorize(&gateway, &user, &resolved).await?;
    Ok(Json(
        gateway
            .history()
            .tool_output(&gateway, &resolved, &tool)
            .await
            .map(crate::history::body::harness_tool_output_body)?,
    ))
}
