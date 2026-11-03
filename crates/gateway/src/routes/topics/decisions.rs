use std::str::FromStr as _;
use std::sync::Arc;

use axum::Json;
use axum::extract::Path;
use axum::extract::State;
use axum::http::StatusCode;
use chrono::DateTime;
use chrono::SubsecRound as _;
use chrono::Utc;
use kernel::topics::Decision;
use kernel::topics::DecisionId;
use kernel::topics::DecisionText;
use kernel::topics::topic_decision;
use wire::gateway::topics::DecisionCreateRequest;
use wire::gateway::topics::DecisionSupersedeRequest;
use wire::gateway::topics::DecisionSuperseded;
use wire::gateway::topics::DecisionUpdate;
use wire::gateway::topics::TopicDecision;
use wire::gateway::topics::TopicDecisionList;

use super::map;
use super::resolve_id;
use crate::Gateway;
use crate::error::ApiError;
use crate::extract::Body;
use crate::tenancy::UserId;

pub(super) async fn list(
    State(gateway): State<Arc<Gateway>>,
    Path(key): Path<String>,
) -> Result<Json<TopicDecisionList>, ApiError> {
    let topic = resolve_id(gateway.topics()?.as_ref(), &key).await?;
    let decisions = gateway
        .decisions()?
        .decisions(topic)
        .await
        .map_err(map::error)?;
    Ok(Json(TopicDecisionList {
        decisions: decisions.into_iter().map(map::decision).collect(),
    }))
}

pub(super) async fn add(
    State(gateway): State<Arc<Gateway>>,
    Path(key): Path<String>,
    user: UserId,
    Body(request): Body<DecisionCreateRequest>,
) -> Result<(StatusCode, Json<TopicDecision>), ApiError> {
    let topic = resolve_id(gateway.topics()?.as_ref(), &key).await?;
    let text = DecisionText::new(&request.text).map_err(map::error)?;
    let added = Decision::new(topic, text, user.as_str(), request.session_id, now());
    gateway
        .decisions()?
        .keep_decisions(std::slice::from_ref(&added))
        .await
        .map_err(map::error)?;
    gateway.sessions().topic_changed(&topic);
    Ok((StatusCode::CREATED, Json(map::decision(added))))
}

pub(super) async fn edit(
    State(gateway): State<Arc<Gateway>>,
    Path((key, id)): Path<(String, String)>,
    Body(request): Body<DecisionUpdate>,
) -> Result<Json<TopicDecision>, ApiError> {
    let store = gateway.decisions()?;
    let current = found(&gateway, &key, &id).await?;
    let text = DecisionText::new(&request.text).map_err(map::error)?;
    let edited = current.edit(text, now()).map_err(map::error)?;
    store
        .keep_decisions(std::slice::from_ref(&edited))
        .await
        .map_err(map::error)?;
    gateway.sessions().topic_changed(&edited.topic);
    Ok(Json(map::decision(edited)))
}

pub(super) async fn supersede(
    State(gateway): State<Arc<Gateway>>,
    Path((key, id)): Path<(String, String)>,
    user: UserId,
    Body(request): Body<DecisionSupersedeRequest>,
) -> Result<Json<DecisionSuperseded>, ApiError> {
    let store = gateway.decisions()?;
    let current = found(&gateway, &key, &id).await?;
    let now = now();
    let replacement = match request.text.as_deref() {
        Some(text) => Some(Decision::new(
            current.topic,
            DecisionText::new(text).map_err(map::error)?,
            user.as_str(),
            request.session_id,
            now,
        )),
        None => None,
    };
    let superseded = current
        .supersede(replacement.as_ref(), now)
        .map_err(map::error)?;
    let kept: Vec<Decision> = replacement
        .iter()
        .cloned()
        .chain([superseded.clone()])
        .collect();
    store.keep_decisions(&kept).await.map_err(map::error)?;
    gateway.sessions().topic_changed(&superseded.topic);
    Ok(Json(DecisionSuperseded {
        superseded: map::decision(superseded),
        replacement: replacement.map(map::decision),
    }))
}

async fn found(gateway: &Gateway, key: &str, id: &str) -> Result<Decision, ApiError> {
    let topic = resolve_id(gateway.topics()?.as_ref(), key).await?;
    let id = DecisionId::from_str(id)
        .map_err(|_| ApiError::Missing(format!("unknown decision {id}")))?;
    topic_decision(gateway.decisions()?.as_ref(), topic, id)
        .await
        .map_err(map::error)
}

fn now() -> DateTime<Utc> {
    Utc::now().trunc_subsecs(3)
}
