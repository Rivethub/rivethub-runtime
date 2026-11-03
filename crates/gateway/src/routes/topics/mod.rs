mod decisions;
mod done;
mod filing;
mod map;
mod page;
mod startup;
mod visibility;

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::Path;
use axum::extract::State;
use axum::http::StatusCode;
use axum::routing;
use chrono::Utc;
use kernel::filing::Target;
use kernel::topics::Change;
use kernel::topics::NewTopic;
use kernel::topics::Placement;
use kernel::topics::PlacementSource;
use kernel::topics::Topic;
use kernel::topics::TopicChanges;
use kernel::topics::TopicId;
use kernel::topics::TopicKey;
use kernel::topics::TopicKind;
use kernel::topics::TopicName;
use kernel::topics::TopicQuery;
use kernel::topics::TopicStore;
use serde::Deserialize;
use wire::gateway::topics::PlacementReply;
use wire::gateway::topics::PlacementRequest;
use wire::gateway::topics::TopicCreateRequest;
use wire::gateway::topics::TopicList;
use wire::gateway::topics::TopicMergeRequest;
use wire::gateway::topics::TopicRecord;
use wire::gateway::topics::TopicUpdate;
use wire::session_id::SessionId;
use wire::session_id::SessionRef;

use crate::Gateway;
use crate::error::ApiError;
use crate::extract::Body;
use crate::extract::Query;
use crate::tenancy::UserId;

pub(super) use map::error as topic_error;
pub(super) use startup::Started;
pub(super) use startup::begin;
pub(crate) use startup::prepare;

const SEARCH_LIMIT_DEFAULT: usize = 10;
const SEARCH_LIMIT_MAX: usize = 50;

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route(
            "/api/topics",
            routing::get(list)
                .post(create)
                .fallback(super::method_not_allowed),
        )
        .route(
            "/api/topics/search",
            routing::get(search).fallback(super::method_not_allowed),
        )
        .route(
            "/api/topics/{topic}",
            routing::get(show)
                .patch(update)
                .delete(delete)
                .fallback(super::method_not_allowed),
        )
        .route(
            "/api/topics/{topic}/merge",
            routing::post(merge).fallback(super::method_not_allowed),
        )
        .route(
            "/api/topics/{topic}/close",
            routing::post(done::close).fallback(super::method_not_allowed),
        )
        .route(
            "/api/topics/{topic}/reopen",
            routing::post(done::reopen).fallback(super::method_not_allowed),
        )
        .route(
            "/api/topics/{topic}/decisions",
            routing::get(decisions::list)
                .post(decisions::add)
                .fallback(super::method_not_allowed),
        )
        .route(
            "/api/topics/{topic}/decisions/{decision}",
            routing::patch(decisions::edit).fallback(super::method_not_allowed),
        )
        .route(
            "/api/topics/{topic}/decisions/{decision}/supersede",
            routing::post(decisions::supersede).fallback(super::method_not_allowed),
        )
        .route(
            "/api/topics/{topic}/sessions",
            routing::get(filing::sessions).fallback(super::method_not_allowed),
        )
        .route(
            "/api/topics/{topic}/startup",
            routing::get(startup::preview).fallback(super::method_not_allowed),
        )
        .route(
            "/api/harness-sessions/{session}/startup-context",
            routing::get(startup::kept).fallback(super::method_not_allowed),
        )
        .route(
            "/api/sessions/{session}/placement",
            routing::get(placement)
                .put(place)
                .delete(unplace)
                .fallback(super::method_not_allowed),
        )
        .route(
            "/api/sessions/history",
            routing::get(done::history).fallback(super::method_not_allowed),
        )
        .route(
            "/api/sessions/{session}/history",
            routing::put(done::mark)
                .delete(done::unmark)
                .fallback(super::method_not_allowed),
        )
        .merge(filing::router())
        .merge(visibility::router())
        .merge(page::router())
        .layer(DefaultBodyLimit::max(super::BODY_LIMIT))
}

pub(super) async fn resolve(store: &dyn TopicStore, key: &str) -> Result<Topic, ApiError> {
    let parsed = TopicKey::parse(key).map_err(map::error)?;
    store
        .find(&parsed)
        .await
        .map_err(map::error)?
        .ok_or_else(|| ApiError::Missing(format!("unknown topic {key}")))
}

pub(super) async fn resolve_id(store: &dyn TopicStore, key: &str) -> Result<TopicId, ApiError> {
    Ok(resolve(store, key).await?.id)
}

pub(super) fn canonical(segment: &str) -> Result<SessionId, ApiError> {
    let decoded = wire::session_id::decode_segment(segment)
        .map_err(|error| ApiError::BadRequest(error.to_string()))?;
    match decoded.session {
        SessionRef::Canonical(session) => Ok(session),
        SessionRef::Bare(_) => Err(ApiError::BadRequest("canonical session required".into())),
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Shown {
    #[default]
    Work,
    Type,
    Entity,
    All,
}

impl Shown {
    fn shows(self, listing: &kernel::topics::TopicListing) -> bool {
        let kind = listing.topic.kind;
        match self {
            Self::All => true,
            Self::Work => kind == TopicKind::Work,
            Self::Type => kind == TopicKind::Type,
            Self::Entity => kind == TopicKind::Entity,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ListParams {
    kind: Shown,
}

async fn list(
    State(gateway): State<Arc<Gateway>>,
    Query(params): Query<ListParams>,
) -> Result<Json<TopicList>, ApiError> {
    let listings = gateway.topics()?.list().await.map_err(map::error)?;
    Ok(Json(TopicList {
        topics: listings
            .into_iter()
            .filter(|listing| params.kind.shows(listing))
            .map(map::summary)
            .collect(),
    }))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct SearchParams {
    q: String,
    limit: Option<usize>,
    kind: Shown,
}

async fn search(
    State(gateway): State<Arc<Gateway>>,
    Query(params): Query<SearchParams>,
) -> Result<Json<TopicList>, ApiError> {
    let limit = match params.limit {
        Some(limit) => limit.clamp(1, SEARCH_LIMIT_MAX),
        None => SEARCH_LIMIT_DEFAULT,
    };
    let store = gateway.topics()?;
    let listings = store.list().await.map_err(map::error)?;
    let redirects = store.redirects().await.map_err(map::error)?;
    let shown = listings
        .into_iter()
        .filter(|listing| params.kind.shows(listing))
        .collect();
    let hits = kernel::topics::rank(shown, &redirects, &TopicQuery::new(&params.q), limit);
    Ok(Json(TopicList {
        topics: hits.into_iter().map(map::summary).collect(),
    }))
}

async fn show(
    State(gateway): State<Arc<Gateway>>,
    Path(key): Path<String>,
) -> Result<Json<TopicRecord>, ApiError> {
    let topic = resolve(gateway.topics()?.as_ref(), &key).await?;
    Ok(Json(map::record(topic)))
}

async fn create(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Body(request): Body<TopicCreateRequest>,
) -> Result<(StatusCode, Json<TopicRecord>), ApiError> {
    let store = gateway.topics()?;
    let parent = match request.parent.as_deref() {
        Some(key) => Some(resolve_id(store.as_ref(), key).await?),
        None => None,
    };
    let kind = map::kind(request.kind);
    let new = NewTopic {
        kind,
        name: TopicName::new(&request.name).map_err(map::error)?,
        aliases: map::aliases(&request.aliases)?,
        parent,
        directory: request.directory.map(map::root),
        owner: (kind != TopicKind::Type).then(|| user.owner().clone()),
    };
    let topic = Topic::create(TopicId::random(), new, Utc::now()).map_err(map::error)?;
    let created = store.create(topic).await.map_err(map::error)?;
    Ok((StatusCode::CREATED, Json(map::record(created))))
}

async fn update(
    State(gateway): State<Arc<Gateway>>,
    Path(key): Path<String>,
    Body(request): Body<TopicUpdate>,
) -> Result<Json<TopicRecord>, ApiError> {
    let store = gateway.topics()?;
    let id = resolve_id(store.as_ref(), &key).await?;
    let parent = match request.parent {
        None => Change::Keep,
        Some(None) => Change::Clear,
        Some(Some(parent)) => Change::Set(resolve_id(store.as_ref(), &parent).await?),
    };
    let directory = match request.directory {
        None => Change::Keep,
        Some(None) => Change::Clear,
        Some(Some(directory)) => Change::Set(map::root(directory)),
    };
    let changes = TopicChanges {
        name: request
            .name
            .as_deref()
            .map(TopicName::new)
            .transpose()
            .map_err(map::error)?,
        aliases: request.aliases.as_deref().map(map::aliases).transpose()?,
        parent,
        directory,
    };
    let updated = store
        .update(id, changes, Utc::now())
        .await
        .map_err(map::error)?;
    gateway.sessions().topic_changed(&updated.id);
    Ok(Json(map::record(updated)))
}

async fn delete(
    State(gateway): State<Arc<Gateway>>,
    Path(key): Path<String>,
) -> Result<StatusCode, ApiError> {
    let store = gateway.topics()?;
    let id = resolve_id(store.as_ref(), &key).await?;
    store.delete(id).await.map_err(map::error)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn merge(
    State(gateway): State<Arc<Gateway>>,
    Path(key): Path<String>,
    Body(request): Body<TopicMergeRequest>,
) -> Result<Json<TopicRecord>, ApiError> {
    let store = gateway.topics()?;
    let source = resolve_id(store.as_ref(), &key).await?;
    let into = resolve_id(store.as_ref(), &request.into).await?;
    let merged = store
        .merge(source, into, Utc::now())
        .await
        .map_err(map::error)?;
    Ok(Json(map::record(merged)))
}

async fn placement(
    State(gateway): State<Arc<Gateway>>,
    Path(segment): Path<String>,
) -> Result<Json<PlacementReply>, ApiError> {
    let session = canonical(&segment)?;
    let store = gateway.topics()?;
    let current = store.placement(&session).await.map_err(map::error)?;
    let suggestion = match current {
        Some(_) => None,
        None => store.suggestion(&session).await.map_err(map::error)?,
    };
    Ok(Json(PlacementReply {
        placement: current.map(map::placement),
        suggestion: suggestion.as_ref().map(map::suggestion),
    }))
}

async fn place(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Path(segment): Path<String>,
    Body(request): Body<PlacementRequest>,
) -> Result<Json<PlacementReply>, ApiError> {
    let session = canonical(&segment)?;
    let store = gateway.topics()?;
    let topic = resolve_id(store.as_ref(), &request.topic_id).await?;
    let before = store.placement(&session).await.map_err(map::error)?;
    let now = Utc::now();
    let outcome = store
        .place(Placement {
            session: session.clone(),
            topic,
            by: PlacementSource::You,
            at: now,
            owner: Some(user.owner().clone()),
        })
        .await
        .map_err(map::error)?;
    let filed = before.map(|placement| (placement.by, placement.topic));
    filing::correct(&gateway, Target::Session(session), filed, Some(topic), now).await?;
    let placement = map::placement(outcome.into_placement());
    if let Some(host) = gateway.plugins() {
        host.publish(&wire::plugin::process::PluginEvent::SessionPlaced {
            session_id: placement.session_id.clone(),
            topic_id: placement.topic_id.clone(),
            by: placement.by,
        });
    }
    Ok(Json(PlacementReply {
        placement: Some(placement),
        suggestion: None,
    }))
}

async fn unplace(
    State(gateway): State<Arc<Gateway>>,
    Path(segment): Path<String>,
) -> Result<Json<PlacementReply>, ApiError> {
    let session = canonical(&segment)?;
    let removed = gateway
        .topics()?
        .unplace(&session)
        .await
        .map_err(map::error)?;
    let filed = removed.map(|placement| (placement.by, placement.topic));
    filing::correct(&gateway, Target::Session(session), filed, None, Utc::now()).await?;
    Ok(Json(PlacementReply {
        placement: None,
        suggestion: None,
    }))
}
