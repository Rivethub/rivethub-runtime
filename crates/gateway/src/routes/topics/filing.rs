use jiff::Timestamp;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::extract::State;
use axum::routing;
use kernel::filing::ConversationPlacement;
use kernel::filing::Correction;
use kernel::filing::Target;
use kernel::topics::PlacementSource;
use kernel::topics::TopicId;
use uuid::Uuid;
use wire::gateway::topics::ConversationPlacementReply;
use wire::gateway::topics::PlacementRequest;
use wire::gateway::topics::SessionSuggestion;
use wire::gateway::topics::SessionSuggestionList;
use wire::gateway::topics::TopicFiling;
use wire::gateway::topics::TopicSessionList;

use super::map;
use super::resolve_id;
use crate::Gateway;
use crate::error::ApiError;
use crate::extract::Body;

pub(super) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route(
            "/api/topics/filing",
            routing::get(all).fallback(super::super::method_not_allowed),
        )
        .route(
            "/api/sessions/suggestions",
            routing::get(suggestions).fallback(super::super::method_not_allowed),
        )
        .route(
            "/api/conversations/{id}/placement",
            routing::get(placement)
                .put(place)
                .delete(unplace)
                .fallback(super::super::method_not_allowed),
        )
}

pub(super) async fn sessions(
    State(gateway): State<Arc<Gateway>>,
    user: crate::tenancy::UserId,
    Path(key): Path<String>,
) -> Result<Json<TopicSessionList>, ApiError> {
    let topic = resolve_id(gateway.topics()?.as_ref(), &key).await?;
    let filed = gateway.filing()?.filed(topic).await.map_err(map::error)?;
    let mut sessions: Vec<_> = filed.sessions.into_iter().map(map::filed_session).collect();
    super::visibility::decorate(&gateway, &gateway.tenancy.viewer(&user), &mut sessions).await?;
    Ok(Json(TopicSessionList {
        sessions,
        conversations: filed
            .conversations
            .into_iter()
            .map(map::filed_conversation)
            .collect(),
    }))
}

async fn all(
    State(gateway): State<Arc<Gateway>>,
    user: crate::tenancy::UserId,
) -> Result<Json<TopicFiling>, ApiError> {
    let filed = gateway.filing()?.all_filed().await.map_err(map::error)?;
    let mut sessions: Vec<_> = filed.sessions.into_iter().map(map::filed_session).collect();
    super::visibility::decorate(&gateway, &gateway.tenancy.viewer(&user), &mut sessions).await?;
    Ok(Json(TopicFiling {
        sessions,
        conversations: filed
            .conversations
            .into_iter()
            .map(map::filed_conversation)
            .collect(),
        history: filed.history.into_iter().map(map::history_mark).collect(),
    }))
}

async fn suggestions(
    State(gateway): State<Arc<Gateway>>,
) -> Result<Json<SessionSuggestionList>, ApiError> {
    let found = gateway.topics()?.suggestions().await.map_err(map::error)?;
    Ok(Json(SessionSuggestionList {
        sessions: found
            .iter()
            .map(|suggestion| SessionSuggestion {
                session_id: suggestion.session.clone(),
                suggestion: map::suggestion(suggestion),
            })
            .collect(),
    }))
}

async fn placement(
    State(gateway): State<Arc<Gateway>>,
    Path(segment): Path<String>,
) -> Result<Json<ConversationPlacementReply>, ApiError> {
    let conversation = known_conversation(&gateway, &segment).await?;
    let placement = gateway
        .filing()?
        .conversation_placement(conversation)
        .await
        .map_err(map::error)?;
    Ok(Json(ConversationPlacementReply {
        placement: placement.as_ref().map(map::conversation_placement),
    }))
}

async fn place(
    State(gateway): State<Arc<Gateway>>,
    Path(segment): Path<String>,
    Body(request): Body<PlacementRequest>,
) -> Result<Json<ConversationPlacementReply>, ApiError> {
    let conversation = known_conversation(&gateway, &segment).await?;
    let topic = resolve_id(gateway.topics()?.as_ref(), &request.topic_id).await?;
    let filing = gateway.filing()?;
    let before = filing
        .conversation_placement(conversation)
        .await
        .map_err(map::error)?;
    let now = Timestamp::now();
    let placed = filing
        .place_conversation(ConversationPlacement {
            conversation,
            topic,
            by: PlacementSource::You,
            at: now,
        })
        .await
        .map_err(map::error)?;
    let filed = before.map(|placement| (placement.by, placement.topic));
    correct(
        &gateway,
        Target::Conversation(conversation),
        filed,
        Some(topic),
        now,
    )
    .await?;
    Ok(Json(ConversationPlacementReply {
        placement: Some(map::conversation_placement(&placed)),
    }))
}

async fn unplace(
    State(gateway): State<Arc<Gateway>>,
    Path(segment): Path<String>,
) -> Result<Json<ConversationPlacementReply>, ApiError> {
    let conversation = known_conversation(&gateway, &segment).await?;
    let removed = gateway
        .filing()?
        .unplace_conversation(conversation)
        .await
        .map_err(map::error)?;
    let filed = removed.map(|placement| (placement.by, placement.topic));
    correct(
        &gateway,
        Target::Conversation(conversation),
        filed,
        None,
        Timestamp::now(),
    )
    .await?;
    Ok(Json(ConversationPlacementReply { placement: None }))
}

pub(super) async fn correct(
    gateway: &Gateway,
    target: Target,
    filed: Option<(PlacementSource, TopicId)>,
    to: Option<TopicId>,
    at: Timestamp,
) -> Result<(), ApiError> {
    if let Some(correction) = Correction::of(target, filed, to, at) {
        gateway
            .filing()?
            .correct(correction)
            .await
            .map_err(map::error)?;
    }
    Ok(())
}

async fn known_conversation(gateway: &Gateway, segment: &str) -> Result<Uuid, ApiError> {
    let conversation = Uuid::parse_str(segment)
        .map_err(|_| ApiError::BadRequest(format!("{segment} is not a conversation id")))?;
    match gateway
        .filing()?
        .subject(conversation)
        .await
        .map_err(map::error)?
    {
        Some(_) => Ok(conversation),
        None => Err(ApiError::Missing(format!("unknown conversation {segment}"))),
    }
}
