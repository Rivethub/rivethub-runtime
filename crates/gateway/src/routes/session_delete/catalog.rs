use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use chrono::{DateTime, Utc};
use kernel::memory::{
    ImportOrigin, MemoryState, SessionCatalog, SessionCursor, SessionQuery, SessionRecord,
    SessionView,
};
use kernel::transcript::ListLimit;
use serde::Deserialize;
use wire::gateway::roster::resume_command;
use wire::gateway::session_catalog::SessionOriginLabel;
use wire::gateway::session_catalog::{
    SessionDetail, SessionMemoryState, SessionSummaryPage, SessionSummaryRow, SharedWith, TopicPath,
};
use wire::harness::HarnessId;
use wire::session_id::{SessionId, SessionKey};

use super::{NOT_FOUND, deletions, ensure_owned, segment_key};
use crate::Gateway;
use crate::error::ApiError;
use crate::session_access;
use crate::tenancy::UserId;

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum ViewParam {
    #[default]
    All,
    Unfiled,
    Kept,
    Trash,
}

impl ViewParam {
    const fn get(self) -> SessionView {
        match self {
            Self::All => SessionView::All,
            Self::Unfiled => SessionView::Unfiled,
            Self::Kept => SessionView::Kept,
            Self::Trash => SessionView::Trash,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ListParams {
    view: Option<ViewParam>,
    harness: Option<HarnessId>,
    topic: Option<String>,
    text: Option<String>,
    cursor: Option<String>,
    limit: Option<u32>,
    include_children: Option<bool>,
}

#[derive(Debug, Default)]
pub(crate) struct Lineage {
    pub(crate) parents: BTreeMap<String, SessionId>,
    children: BTreeMap<String, u64>,
}

pub(crate) async fn lineage(gateway: &Gateway) -> Lineage {
    let Some(lister) = gateway.session_lister() else {
        return Lineage::default();
    };
    let mut lineage = Lineage::default();
    for row in lister
        .list(&crate::history::registered(), ListLimit::most())
        .await
    {
        let Some(parent) = row.parent_session_id.as_deref() else {
            continue;
        };
        let child = SessionId::new(row.harness, &row.id).to_string();
        let parent = SessionId::new(row.harness, parent);
        *lineage.children.entry(parent.to_string()).or_default() += 1;
        lineage.parents.insert(child, parent);
    }
    lineage
}

pub(crate) fn catalog(gateway: &Gateway) -> Result<&dyn SessionCatalog, ApiError> {
    gateway
        .session_catalog()
        .ok_or_else(|| ApiError::Unavailable("session catalog unavailable".to_owned()))
}

fn encode_cursor(cursor: &SessionCursor) -> String {
    format!("{}.{}", cursor.started_at.timestamp_millis(), cursor.key)
}

fn decode_cursor(text: &str) -> Result<SessionCursor, ApiError> {
    let invalid = || ApiError::BadRequest("cursor is not one this server issued".to_owned());
    let (millis, key) = text.split_once('.').ok_or_else(invalid)?;
    let millis: i64 = millis.parse().map_err(|_| invalid())?;
    let started_at = DateTime::from_timestamp_millis(millis).ok_or_else(invalid)?;
    key.parse::<SessionKey>().map_err(|_| invalid())?;
    Ok(SessionCursor {
        started_at,
        key: key.to_owned(),
    })
}

async fn query(
    gateway: &Gateway,
    params: ListParams,
    lineage: &Lineage,
) -> Result<SessionQuery, ApiError> {
    let mut query = SessionQuery::new(params.view.unwrap_or_default().get());
    query.harness = params.harness;
    if let Some(key) = params.topic.filter(|text| !text.is_empty()) {
        let topics = gateway.topics()?;
        query.topic = Some(crate::routes::topics::resolve_id(topics.as_ref(), &key).await?);
    }
    query.text = params.text.filter(|text| !text.trim().is_empty());
    query.after = params.cursor.as_deref().map(decode_cursor).transpose()?;
    if let Some(limit) = params.limit {
        query.limit = limit;
    }
    if params.include_children != Some(true) {
        query.exclude = lineage.parents.keys().cloned().collect();
    }
    Ok(query)
}

const fn memory_state(state: MemoryState) -> SessionMemoryState {
    match state {
        MemoryState::Capturing => SessionMemoryState::Capturing,
        MemoryState::Queued => SessionMemoryState::Queued,
        MemoryState::Summarized => SessionMemoryState::Summarized,
        MemoryState::Flagged => SessionMemoryState::Flagged,
    }
}

fn label(origin: ImportOrigin) -> SessionOriginLabel {
    use kernel::memory::share::receive::Verification;
    use wire::capture::ReceiptVerification;
    let (verification, bundle_id) = match origin.receipt {
        Some(receipt) => (
            Some(match receipt.verification {
                Verification::Verified => ReceiptVerification::Verified,
                Verification::Unsigned => ReceiptVerification::Unsigned,
                Verification::UnknownKey => ReceiptVerification::UnknownKey,
            }),
            Some(receipt.bundle_id),
        ),
        None => (None, None),
    };
    SessionOriginLabel {
        author: origin.author,
        created_at: origin.created_at,
        updated_at: origin.updated_at,
        verification,
        bundle_id,
    }
}

pub(crate) fn row<K: Into<SessionKey>>(
    record: SessionRecord<K>,
    visibility: SharedWith,
    lineage: &Lineage,
) -> SessionSummaryRow {
    let id: SessionKey = record.id.into();
    let key = id.to_string();
    let parent_id = lineage.parents.get(&key).cloned();
    let child_count = lineage
        .children
        .get(&key)
        .copied()
        .filter(|count| *count > 0);
    SessionSummaryRow {
        harness: id.harness(),
        id,
        title: record.title,
        origin: record.origin.map(label),
        started_at: record.started_at,
        ended_at: record.ended_at,
        message_count: record.message_count,
        topic: record.topic.map(|topic| TopicPath {
            id: topic.id.to_string(),
            path: topic.path,
        }),
        visibility,
        kept: record.kept,
        trashed: record.trashed,
        memory_state: memory_state(record.memory),
        gist: record.gist,
        parent_id,
        child_count,
    }
}

pub(super) async fn list(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Query(params): Query<ListParams>,
) -> Result<Json<SessionSummaryPage>, ApiError> {
    let lineage = lineage(&gateway).await;
    let query = query(&gateway, params, &lineage).await?;
    let page = catalog(&gateway)?.list(&query).await?;
    let keys: Vec<SessionKey> = page.items.iter().map(|record| record.id.clone()).collect();
    let mut seen = session_access::seen(&gateway, &user, &keys).await?;
    let items = page
        .items
        .into_iter()
        .map(|record| {
            let shared_with = seen
                .remove(&record.id)
                .map_or(SharedWith::Private, |seen| seen.shared_with);
            row(record, shared_with, &lineage)
        })
        .collect();
    Ok(Json(SessionSummaryPage {
        items,
        next: page.next.as_ref().map(encode_cursor),
    }))
}

pub(super) async fn detail(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
) -> Result<Json<SessionDetail>, ApiError> {
    let key = segment_key(&segment)?;
    let lineage = lineage(&gateway).await;
    let record = catalog(&gateway)?
        .get(&key)
        .await?
        .ok_or(ApiError::NotFound(NOT_FOUND))?;
    let seen = session_access::seen_one(&gateway, &user, &key).await?;
    if !seen.readable() {
        return Err(ApiError::NotFound(NOT_FOUND));
    }
    let session = key.session();
    let cwd = record.cwd.clone();
    Ok(Json(SessionDetail {
        resume_command: session
            .and_then(|session| resume_command(session.harness(), session.native())),
        row: row(record, seen.shared_with, &lineage),
        cwd,
        changed: Vec::new(),
        entities: Vec::new(),
        tags: Vec::new(),
    }))
}

pub(super) async fn trash(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
) -> Result<StatusCode, ApiError> {
    let key = segment_key(&segment)?;
    ensure_owned(&gateway, &user, &key).await?;
    deletions(&gateway)?
        .trash_session(key.as_str(), Some(user.as_str()), Utc::now())
        .await?;
    tracing::info!(session = %key, "session moved to the trash");
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn restore(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
) -> Result<StatusCode, ApiError> {
    let key = segment_key(&segment)?;
    ensure_owned(&gateway, &user, &key).await?;
    deletions(&gateway)?.restore_session(key.as_str()).await?;
    tracing::info!(session = %key, "session restored from the trash");
    Ok(StatusCode::NO_CONTENT)
}

pub(super) async fn unkeep(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
) -> Result<StatusCode, ApiError> {
    let key = segment_key(&segment)?;
    ensure_owned(&gateway, &user, &key).await?;
    deletions(&gateway)?.unkeep_session(key.as_str()).await?;
    tracing::info!(session = %key, "session no longer kept out of the trash review");
    Ok(StatusCode::NO_CONTENT)
}
