mod catalog;
mod trash;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use kernel::memory::{DeletedSession, SessionActivity, SessionDeletion};
use wire::gateway::session_delete::{
    SessionDeleteError, SessionDeleteRefused, SessionDeleteRequest, SessionDeleteResult,
    SessionDeleteResults, SessionDeleted,
};
use wire::session_id::{SessionId, SessionRef, decode_segment};

use crate::Gateway;
use crate::error::ApiError;
use crate::extract::Body;
use crate::history::{Resolved, authorize};
use crate::tenancy::UserId;

const MAX_BATCH: usize = 200;
const LIVE: &str = "session is live";
const NOT_FOUND: &str = "not found";

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route(
            "/api/sessions",
            get(catalog::list).fallback(super::method_not_allowed),
        )
        .route(
            "/api/sessions/delete",
            post(delete_many).fallback(super::method_not_allowed),
        )
        .route(
            "/api/sessions/trash",
            get(trash::trash).fallback(super::method_not_allowed),
        )
        .route(
            "/api/sessions/{session}",
            get(catalog::detail)
                .delete(delete_one)
                .fallback(super::method_not_allowed),
        )
        .route(
            "/api/sessions/{session}/keep",
            put(trash::keep)
                .delete(catalog::unkeep)
                .fallback(super::method_not_allowed),
        )
        .route(
            "/api/sessions/{session}/trash",
            post(catalog::trash).fallback(super::method_not_allowed),
        )
        .route(
            "/api/sessions/{session}/restore",
            post(catalog::restore).fallback(super::method_not_allowed),
        )
}

pub(super) async fn tombstoned(
    gateway: &Gateway,
    keys: &[String],
) -> BTreeMap<String, DateTime<Utc>> {
    let Some(deletions) = gateway.session_deletion() else {
        return BTreeMap::new();
    };
    let read = deletions.tombstones(keys).await;
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "CONTRACT B1: unreadable tombstones list every session, logged (note 75)"
        )
    )]
    let found = read.unwrap_or_else(|error| {
        tracing::warn!(%error, "session tombstones are unreadable; listing every session");
        BTreeMap::new()
    });
    found
}

pub(super) async fn trashed(gateway: &Gateway, keys: &[String]) -> BTreeSet<String> {
    let Some(deletions) = gateway.session_deletion() else {
        return BTreeSet::new();
    };
    let read = deletions.trashed(keys).await;
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "CONTRACT B1: an unreadable trash list shows every session, logged (note 127)"
        )
    )]
    let found = read.unwrap_or_else(|error| {
        tracing::warn!(%error, "session trash is unreadable; listing every session");
        BTreeMap::new()
    });
    found.into_keys().collect()
}

pub(super) fn hidden(
    deleted: &BTreeMap<String, DateTime<Utc>>,
    key: &str,
    updated_at: Option<DateTime<Utc>>,
) -> bool {
    deleted
        .get(key)
        .is_some_and(|deleted_at| updated_at.is_none_or(|updated_at| updated_at <= *deleted_at))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Refusal {
    Live,
    NotFound,
}

fn deletions(gateway: &Gateway) -> Result<&dyn SessionDeletion, ApiError> {
    gateway
        .session_deletion()
        .ok_or_else(|| ApiError::Unavailable("session deletion unavailable".to_owned()))
}

fn segment_session(segment: &str) -> Result<SessionId, ApiError> {
    if let Ok(session) = segment.parse::<SessionId>() {
        return Ok(session);
    }
    let decoded =
        decode_segment(segment).map_err(|error| ApiError::BadRequest(error.to_string()))?;
    match decoded.session {
        SessionRef::Canonical(session) => Ok(session),
        SessionRef::Bare(_) => Err(ApiError::BadRequest(
            "canonical session required".to_owned(),
        )),
    }
}

fn resolved(session: &SessionId) -> Resolved {
    Resolved {
        session: session.clone(),
        harness: session.harness(),
        native: session.native().to_owned(),
        redirected_to: None,
    }
}

fn live(gateway: &Gateway, session: &SessionId) -> bool {
    gateway
        .terminal()
        .is_some_and(|terminal| terminal.is_live(session.native()))
}

async fn ended_at_rest(
    gateway: &Gateway,
    user: &UserId,
    session: &SessionId,
) -> Result<bool, ApiError> {
    match gateway.terminal() {
        Some(terminal) if terminal.chat_at_rest(session.native()) => {
            terminal.remove_session(user, session.as_str()).await?;
            Ok(!live(gateway, session))
        }
        _ => Ok(false),
    }
}

async fn visible(gateway: &Gateway, user: &UserId, session: &SessionId) -> Result<bool, ApiError> {
    match authorize(gateway, user, &resolved(session)).await {
        Ok(()) => Ok(true),
        Err(ApiError::SessionForbidden) => Ok(false),
        Err(error) => Err(error),
    }
}

async fn known(
    gateway: &Gateway,
    deletions: &dyn SessionDeletion,
    session: &SessionId,
) -> Result<bool, ApiError> {
    if let Some(lister) = gateway.session_lister()
        && lister
            .describe(session.harness(), session.native(), None)
            .await?
            .is_some()
    {
        return Ok(true);
    }
    let key = session.as_str().to_owned();
    let activity = deletions
        .session_activity(std::slice::from_ref(&key))
        .await?;
    Ok(activity.get(&key).is_some_and(SessionActivity::recorded))
}

async fn erase(
    gateway: &Gateway,
    user: &UserId,
    session: &SessionId,
) -> Result<Result<DeletedSession, Refusal>, ApiError> {
    let deletions = deletions(gateway)?;
    if !visible(gateway, user, session).await? {
        return Ok(Err(Refusal::NotFound));
    }
    if live(gateway, session) && !ended_at_rest(gateway, user, session).await? {
        return Ok(Err(Refusal::Live));
    }
    if !known(gateway, deletions, session).await? {
        return Ok(Err(Refusal::NotFound));
    }
    let deleted = deletions
        .delete_session(session.as_str(), Some(user.as_str()), Utc::now())
        .await?;
    tracing::info!(
        session = %session,
        conversations = deleted.conversations,
        messages = deleted.messages,
        summaries = deleted.summaries,
        "session deleted"
    );
    Ok(Ok(deleted))
}

fn deleted_reply(session: SessionId, deleted: DeletedSession) -> SessionDeleted {
    SessionDeleted {
        ok: true,
        session,
        conversations: deleted.conversations,
        messages: deleted.messages,
        summaries: deleted.summaries,
    }
}

async fn delete_one(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
) -> Result<Json<SessionDeleted>, ApiError> {
    let session = segment_session(&segment)?;
    match erase(&gateway, &user, &session).await? {
        Ok(deleted) => Ok(Json(deleted_reply(session, deleted))),
        Err(Refusal::Live) => Err(ApiError::Conflict(LIVE.to_owned())),
        Err(Refusal::NotFound) => Err(ApiError::NotFound(NOT_FOUND)),
    }
}

fn refused(session: SessionId, error: SessionDeleteError) -> SessionDeleteResult {
    SessionDeleteResult::Refused(SessionDeleteRefused {
        session,
        ok: false,
        error,
    })
}

async fn delete_many(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Body(request): Body<SessionDeleteRequest>,
) -> Result<Json<SessionDeleteResults>, ApiError> {
    deletions(&gateway)?;
    let mut seen = BTreeSet::new();
    let sessions: Vec<SessionId> = request
        .sessions
        .into_iter()
        .filter(|session| seen.insert(session.clone()))
        .collect();
    if sessions.is_empty() || sessions.len() > MAX_BATCH {
        return Err(ApiError::BadRequest(format!(
            "sessions must name 1 to {MAX_BATCH} sessions"
        )));
    }
    let mut results = Vec::with_capacity(sessions.len());
    for session in sessions {
        let outcome = erase(&gateway, &user, &session).await;
        let result = match outcome {
            Ok(Ok(deleted)) => SessionDeleteResult::Deleted(deleted_reply(session, deleted)),
            Ok(Err(Refusal::Live)) => refused(session, SessionDeleteError::Live),
            Ok(Err(Refusal::NotFound)) | Err(ApiError::BadRequest(_)) => {
                refused(session, SessionDeleteError::NotFound)
            }
            Err(error) => {
                tracing::warn!(session = %session, %error, "session delete failed");
                refused(session, SessionDeleteError::Failed)
            }
        };
        results.push(result);
    }
    Ok(Json(SessionDeleteResults { results }))
}
