use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::extract::State;
use axum::routing;
use chrono::Utc;
use kernel::tenancy::Space;
use kernel::tenancy::SpaceKind;
use kernel::tenancy::TenancyError;
use kernel::tenancy::TenancyStoreError;
use kernel::tenancy::Viewer;
use kernel::tenancy::Visibility;
use kernel::tenancy::VisibilityDefault;
use wire::gateway::tenancy::MeResponse;
use wire::gateway::tenancy::SessionVisibilityRecord;
use wire::gateway::tenancy::SpaceRecord;
use wire::gateway::tenancy::TopicVisibilityRecord;
use wire::gateway::tenancy::UserSettings;
use wire::gateway::tenancy::UserSettingsUpdate;
use wire::gateway::tenancy::VisibilityUpdate;
use wire::gateway::topics::TopicSession;
use wire::session_id::SessionKey;

use super::resolve_id;
use crate::Gateway;
use crate::error::ApiError;
use crate::tenancy::UserId;

pub(super) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route(
            "/api/me",
            routing::get(me).fallback(super::super::method_not_allowed),
        )
        .route(
            "/api/me/settings",
            routing::get(settings)
                .put(save_settings)
                .fallback(super::super::method_not_allowed),
        )
        .route(
            "/api/sessions/{session}/visibility",
            routing::get(session_visibility)
                .patch(mark_session)
                .fallback(super::super::method_not_allowed),
        )
        .route(
            "/api/topics/{topic}/visibility",
            routing::patch(mark_topic).fallback(super::super::method_not_allowed),
        )
}

pub(crate) fn error(error: TenancyStoreError) -> ApiError {
    match error {
        TenancyStoreError::Invalid(TenancyError::NotMember { .. }) => {
            ApiError::Forbidden(error.to_string())
        }
        TenancyStoreError::Invalid(_) => ApiError::BadRequest(error.to_string()),
        TenancyStoreError::NotOwner(_) => ApiError::Forbidden(error.to_string()),
        TenancyStoreError::NotFound(_) => ApiError::NotFound("not found"),
        TenancyStoreError::CatalogConflict(_) => ApiError::Conflict(error.to_string()),
        TenancyStoreError::Unavailable(source) => {
            tracing::warn!("visibility store failed: {source}");
            ApiError::Unavailable("visibility store unavailable".into())
        }
    }
}

fn wanted(text: Option<&str>) -> Result<Option<Visibility>, ApiError> {
    text.map(Visibility::parse)
        .transpose()
        .map_err(|invalid| ApiError::BadRequest(invalid.to_string()))
}

const fn kind(kind: SpaceKind) -> wire::gateway::tenancy::SpaceKind {
    match kind {
        SpaceKind::Household => wire::gateway::tenancy::SpaceKind::Household,
        SpaceKind::Team => wire::gateway::tenancy::SpaceKind::Team,
        SpaceKind::Compartment => wire::gateway::tenancy::SpaceKind::Compartment,
    }
}

fn space(space: &Space) -> SpaceRecord {
    SpaceRecord {
        id: space.id.as_str().to_owned(),
        kind: kind(space.kind),
        name: space.name.clone(),
    }
}

fn user_settings(found: Option<VisibilityDefault>) -> UserSettings {
    match found {
        Some(found) => UserSettings {
            default_visibility: found.visibility.as_str().to_owned(),
            chosen: true,
            default_since: Some(found.since),
        },
        None => UserSettings {
            default_visibility: Visibility::Private.as_str().to_owned(),
            chosen: false,
            default_since: None,
        },
    }
}

fn session_record(
    session: SessionKey,
    state: kernel::tenancy::SessionVisibility,
) -> SessionVisibilityRecord {
    SessionVisibilityRecord {
        session_id: session,
        visibility: state.effective.as_str().to_owned(),
        mark: state.mark.map(|mark| mark.as_str().to_owned()),
        owner: state.owner.map(|owner| owner.as_str().to_owned()),
    }
}

async fn me(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
) -> Result<Json<MeResponse>, ApiError> {
    let viewer = gateway.tenancy.viewer(&user);
    let found = gateway
        .tenancy_store()?
        .default_visibility(user.owner())
        .await
        .map_err(error)?;
    Ok(Json(MeResponse {
        user: user.as_str().to_owned(),
        spaces: gateway
            .tenancy
            .spaces()
            .iter()
            .filter(|candidate| viewer.spaces().contains(&candidate.id))
            .map(space)
            .collect(),
        settings: user_settings(found),
    }))
}

async fn settings(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
) -> Result<Json<UserSettings>, ApiError> {
    let found = gateway
        .tenancy_store()?
        .default_visibility(user.owner())
        .await
        .map_err(error)?;
    Ok(Json(user_settings(found)))
}

async fn save_settings(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Json(update): Json<UserSettingsUpdate>,
) -> Result<Json<UserSettings>, ApiError> {
    let visibility = Visibility::parse(&update.default_visibility)
        .map_err(|invalid| ApiError::BadRequest(invalid.to_string()))?;
    gateway
        .tenancy
        .viewer(&user)
        .may_share(&visibility)
        .map_err(|refused| ApiError::Forbidden(refused.to_string()))?;
    let saved = gateway
        .tenancy_store()?
        .set_default_visibility(user.owner(), &visibility, Utc::now())
        .await
        .map_err(error)?;
    Ok(Json(user_settings(Some(saved))))
}

async fn session_visibility(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
) -> Result<Json<SessionVisibilityRecord>, ApiError> {
    let key = crate::session_address::resolve(&gateway, &segment).await?;
    let viewer = gateway.tenancy.viewer(&user);
    let mut found = gateway
        .tenancy_store()?
        .session_visibility(&viewer, std::slice::from_ref(&key))
        .await
        .map_err(error)?;
    let state = found
        .remove(&key)
        .ok_or(ApiError::NotFound("session not found"))?;
    Ok(Json(session_record(key, state)))
}

async fn mark_session(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
    Json(update): Json<VisibilityUpdate>,
) -> Result<Json<SessionVisibilityRecord>, ApiError> {
    let key = crate::session_address::resolve(&gateway, &segment).await?;
    let mark = wanted(update.visibility.as_deref())?;
    crate::session_access::change_key(&gateway, &user, &key).await?;
    let viewer = gateway.tenancy.viewer(&user);
    let state = gateway
        .tenancy_store()?
        .mark_session(&viewer, &key, mark.as_ref(), Utc::now())
        .await
        .map_err(error)?;
    Ok(Json(session_record(key, state)))
}

async fn mark_topic(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(key): Path<String>,
    Json(update): Json<VisibilityUpdate>,
) -> Result<Json<TopicVisibilityRecord>, ApiError> {
    let topic = resolve_id(gateway.topics()?.as_ref(), &key).await?;
    let mark = wanted(update.visibility.as_deref())?;
    let viewer = gateway.tenancy.viewer(&user);
    let state = gateway
        .tenancy_store()?
        .mark_topic(&viewer, topic, mark.as_ref())
        .await
        .map_err(error)?;
    Ok(Json(TopicVisibilityRecord {
        topic_id: state.topic.to_string(),
        visibility: state.effective.as_str().to_owned(),
        mark: state.mark.map(|mark| mark.as_str().to_owned()),
    }))
}

pub(super) async fn decorate(
    gateway: &Gateway,
    viewer: &Viewer,
    sessions: &mut [TopicSession],
) -> Result<(), ApiError> {
    let keys: Vec<SessionKey> = sessions
        .iter()
        .filter(|session| session.visibility.is_none())
        .map(|session| SessionKey::from(&session.placement.session_id))
        .collect();
    if keys.is_empty() {
        return Ok(());
    }
    let Some(store) = gateway.tenancy_store.as_ref() else {
        return Ok(());
    };
    let found = store
        .session_visibility(viewer, &keys)
        .await
        .map_err(error)?;
    for session in sessions {
        if let Some(state) = found.get(&SessionKey::from(&session.placement.session_id)) {
            session.visibility = Some(state.effective.as_str().to_owned());
            session.visibility_mark = state.mark.as_ref().map(|mark| mark.as_str().to_owned());
        }
    }
    Ok(())
}
