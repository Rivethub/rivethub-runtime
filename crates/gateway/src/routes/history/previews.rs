use std::collections::BTreeSet;
use std::sync::Arc;

use axum::Json;
use axum::extract::{RawQuery, State};
use kernel::preview::PreviewKind;
use wire::gateway::sessions::{SessionPreview, SessionPreviewKind, SessionPreviewList};
use wire::session_id::{SessionId, encode_segment};

use crate::Gateway;
use crate::error::ApiError;
use crate::history::{Need, authorize};
use crate::tenancy::UserId;

const MOST: usize = 50;

pub(super) async fn previews(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    RawQuery(query): RawQuery,
) -> Result<Json<SessionPreviewList>, ApiError> {
    let sessions = requested(query.as_deref())?;
    let found = futures::future::join_all(
        sessions
            .into_iter()
            .map(|session| preview(&gateway, &user, session)),
    )
    .await;
    Ok(Json(SessionPreviewList {
        previews: found.into_iter().flatten().collect(),
    }))
}

fn requested(query: Option<&str>) -> Result<BTreeSet<SessionId>, ApiError> {
    let mut sessions = BTreeSet::new();
    for pair in query
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
    {
        let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
        if super::super::notices::form_text(key)? != "session" {
            continue;
        }
        let session = super::super::notices::form_text(value)?
            .parse::<SessionId>()
            .map_err(|_| ApiError::BadRequest("canonical session required".into()))?;
        sessions.insert(session);
    }
    if sessions.len() > MOST {
        return Err(ApiError::BadRequest(format!(
            "at most {MOST} sessions at a time"
        )));
    }
    Ok(sessions)
}

async fn preview(gateway: &Gateway, user: &UserId, session: SessionId) -> Option<SessionPreview> {
    let history = gateway.history();
    let resolved = kept(
        &session,
        history
            .resolve_canonical(gateway, &encode_segment(&session))
            .await,
    )?;
    kept(
        &session,
        authorize(gateway, user, &resolved, Need::Read).await,
    )?;
    let shown = kept(&session, history.preview(gateway, &resolved).await)??;
    Some(SessionPreview {
        session_id: resolved.session,
        kind: kind(shown.kind),
        text: shown.text,
        at: shown.at,
    })
}

fn kept<T>(session: &SessionId, read: Result<T, ApiError>) -> Option<T> {
    match read {
        Ok(value) => Some(value),
        Err(error) => {
            tracing::debug!(%session, %error, "no preview for this session");
            None
        }
    }
}

const fn kind(kind: PreviewKind) -> SessionPreviewKind {
    match kind {
        PreviewKind::Running => SessionPreviewKind::Running,
        PreviewKind::Asking => SessionPreviewKind::Asking,
        PreviewKind::Approval => SessionPreviewKind::Approval,
        PreviewKind::Said => SessionPreviewKind::Said,
    }
}
