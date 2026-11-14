use std::sync::Arc;

use axum::Json;
use axum::extract::Path;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::DateTime;
use chrono::Utc;
use kernel::topics::HistoryMark;
use wire::gateway::topics::SessionHistoryList;
use wire::gateway::topics::SessionHistoryMark;
use wire::gateway::topics::TopicRecord;

use super::map;
use super::resolve_id;
use crate::Gateway;
use crate::error::ApiError;

pub(super) async fn close(
    State(gateway): State<Arc<Gateway>>,
    Path(key): Path<String>,
) -> Result<Json<TopicRecord>, ApiError> {
    let now = Utc::now();
    closed(&gateway, &key, Some(now), now).await
}

pub(super) async fn reopen(
    State(gateway): State<Arc<Gateway>>,
    Path(key): Path<String>,
) -> Result<Json<TopicRecord>, ApiError> {
    closed(&gateway, &key, None, Utc::now()).await
}

async fn closed(
    gateway: &Gateway,
    key: &str,
    at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Result<Json<TopicRecord>, ApiError> {
    let store = gateway.topics()?;
    let id = resolve_id(store.as_ref(), key).await?;
    let topic = store.set_closed(id, at, now).await.map_err(map::error)?;
    Ok(Json(map::record(topic)))
}

pub(super) async fn history(
    State(gateway): State<Arc<Gateway>>,
) -> Result<Json<SessionHistoryList>, ApiError> {
    let marks = gateway.topics()?.history().await.map_err(map::error)?;
    Ok(Json(SessionHistoryList {
        sessions: marks.into_iter().map(map::history_mark).collect(),
    }))
}

pub(super) async fn mark(
    State(gateway): State<Arc<Gateway>>,
    Path(segment): Path<String>,
) -> Result<Json<SessionHistoryMark>, ApiError> {
    let session = crate::session_address::resolve_captured(&gateway, &segment).await?;
    let kept = gateway
        .topics()?
        .mark_history(HistoryMark {
            session,
            at: Utc::now(),
        })
        .await
        .map_err(map::error)?;
    Ok(Json(map::history_mark(kept)))
}

pub(super) async fn unmark(
    State(gateway): State<Arc<Gateway>>,
    Path(segment): Path<String>,
) -> Result<StatusCode, ApiError> {
    let session = crate::session_address::resolve_captured(&gateway, &segment).await?;
    gateway
        .topics()?
        .unmark_history(&session)
        .await
        .map_err(map::error)?;
    Ok(StatusCode::NO_CONTENT)
}
