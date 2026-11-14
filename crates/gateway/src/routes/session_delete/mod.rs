pub(crate) mod catalog;
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
use wire::session_id::{SessionId, SessionKey};

use crate::Gateway;
use crate::error::ApiError;
use crate::extract::Body;
use crate::history::Resolved;
use crate::session_access::{self, Seen};
use crate::tenancy::UserId;

const MAX_BATCH: usize = 200;
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
    NotFound,
    NotOwner,
}

fn deletions(gateway: &Gateway) -> Result<&dyn SessionDeletion, ApiError> {
    gateway
        .session_deletion()
        .ok_or_else(|| ApiError::Unavailable("session deletion unavailable".to_owned()))
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
    recorded(deletions, session.as_str()).await
}

async fn recorded(deletions: &dyn SessionDeletion, key: &str) -> Result<bool, ApiError> {
    let keys = [key.to_owned()];
    let activity = deletions.session_activity(&keys).await?;
    Ok(activity.get(key).is_some_and(SessionActivity::recorded))
}

async fn reached(
    gateway: &Gateway,
    user: &UserId,
    deletions: &dyn SessionDeletion,
    key: &SessionKey,
) -> Result<Option<Seen>, ApiError> {
    let seen = session_access::seen_one(gateway, user, key).await?;
    if !seen.readable() {
        return Ok(None);
    }
    let stored = match key {
        SessionKey::Harness(session) => known(gateway, deletions, session).await?,
        SessionKey::Import(_) => recorded(deletions, key.as_str()).await?,
    };
    Ok(stored.then_some(seen))
}

fn resolved(session: &SessionId) -> Resolved {
    Resolved {
        session: session.clone(),
        harness: session.harness(),
        native: session.native().to_owned(),
        redirected_to: None,
    }
}

async fn ensure_owned(gateway: &Gateway, user: &UserId, key: &SessionKey) -> Result<(), ApiError> {
    reached(gateway, user, deletions(gateway)?, key)
        .await?
        .ok_or(ApiError::NotFound(NOT_FOUND))?
        .change()
}

async fn erase(
    gateway: &Gateway,
    user: &UserId,
    session: &SessionKey,
) -> Result<Result<DeletedSession, Refusal>, ApiError> {
    let deletions = deletions(gateway)?;
    match reached(gateway, user, deletions, session).await? {
        None => return Ok(Err(Refusal::NotFound)),
        Some(seen) if seen.change().is_err() => return Ok(Err(Refusal::NotOwner)),
        Some(_) => {}
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

fn deleted_reply(session: SessionKey, deleted: DeletedSession) -> SessionDeleted {
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
    let session = crate::session_address::resolve(&gateway, &segment).await?;
    match erase(&gateway, &user, &session).await? {
        Ok(deleted) => Ok(Json(deleted_reply(session, deleted))),
        Err(Refusal::NotFound) => Err(ApiError::NotFound(NOT_FOUND)),
        Err(Refusal::NotOwner) => Err(ApiError::Forbidden(
            "only the person who owns this session can delete it".to_owned(),
        )),
    }
}

fn refused(session: SessionKey, error: SessionDeleteError) -> SessionDeleteResult {
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
    let sessions: Vec<SessionKey> = request
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
            Ok(Err(Refusal::NotFound)) | Err(ApiError::BadRequest(_)) => {
                refused(session, SessionDeleteError::NotFound)
            }
            Ok(Err(Refusal::NotOwner)) => refused(session, SessionDeleteError::NotOwner),
            Err(error) => {
                tracing::warn!(session = %session, %error, "session delete failed");
                refused(session, SessionDeleteError::Failed)
            }
        };
        results.push(result);
    }
    Ok(Json(SessionDeleteResults { results }))
}
