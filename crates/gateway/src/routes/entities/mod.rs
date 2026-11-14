mod article;
pub(super) mod map;

use std::str::FromStr as _;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::Path;
use axum::extract::State;
use axum::routing;
use chrono::DateTime;
use chrono::SubsecRound as _;
use chrono::Utc;
use kernel::facts::Claim;
use kernel::facts::FactId;
use kernel::facts::FactKey;
use kernel::facts::FactValue;
use kernel::facts::NewClaim;
use kernel::facts::Said;
use kernel::facts::facts_as_of;
use kernel::topics::Topic;
use kernel::topics::TopicId;
use kernel::topics::TopicKey;
use kernel::topics::TopicKind;
use serde::Deserialize;
use wire::gateway::entities::EntityArticle;
use wire::gateway::entities::EntitySummary;
use wire::gateway::entities::EntityTypeInfo;
use wire::gateway::entities::Fact;
use wire::gateway::entities::FactEdit;

use crate::Gateway;
use crate::error::ApiError;
use crate::extract::Body;
use crate::extract::Query;
use crate::tenancy::UserId;

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route(
            "/api/entities/types",
            routing::get(types).fallback(super::method_not_allowed),
        )
        .route(
            "/api/entities",
            routing::get(list).fallback(super::method_not_allowed),
        )
        .route(
            "/api/entities/{entity}",
            routing::get(show).fallback(super::method_not_allowed),
        )
        .route(
            "/api/entities/{entity}/facts",
            routing::put(set_fact).fallback(super::method_not_allowed),
        )
        .layer(DefaultBodyLimit::max(super::BODY_LIMIT))
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct ListParams {
    #[serde(rename = "type")]
    entity_type: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct ShowParams {
    as_of: Option<DateTime<Utc>>,
}

pub(super) struct Filed {
    id: TopicId,
    pub(super) key: String,
    pub(super) name: String,
}

struct Known {
    topics: Vec<Topic>,
}

impl Known {
    async fn load(gateway: &Gateway) -> Result<Self, ApiError> {
        let topics: Vec<Topic> = gateway
            .topics()?
            .list()
            .await
            .map_err(super::topics::topic_error)?
            .into_iter()
            .map(|listing| listing.topic)
            .collect();
        Ok(Self { topics })
    }

    fn filed(&self, entity: &Topic) -> Option<Filed> {
        let mut parent = entity.parent;
        for _ in 0..32 {
            let id = parent?;
            let topic = self.topics.iter().find(|topic| topic.id == id)?;
            if topic.kind == TopicKind::Type {
                return Some(Filed {
                    id: topic.id,
                    key: topic.slug.as_str().to_owned(),
                    name: topic.name.as_str().to_owned(),
                });
            }
            parent = topic.parent;
        }
        None
    }

    fn entities(&self) -> impl Iterator<Item = (&Topic, Filed)> {
        self.topics
            .iter()
            .filter(|topic| topic.kind == TopicKind::Entity)
            .filter_map(|topic| self.filed(topic).map(|kind| (topic, kind)))
    }

    fn title(&self, id: TopicId) -> Option<&str> {
        self.topics
            .iter()
            .find(|topic| topic.id == id)
            .map(|topic| topic.name.as_str())
    }
}

async fn list(
    State(gateway): State<Arc<Gateway>>,
    Query(params): Query<ListParams>,
) -> Result<Json<Vec<EntitySummary>>, ApiError> {
    let known = Known::load(&gateway).await?;
    let links = gateway.facts()?.session_links().await.map_err(map::error)?;
    let wanted = params
        .entity_type
        .as_deref()
        .filter(|kind| !kind.is_empty());
    let mut entities: Vec<EntitySummary> = known
        .entities()
        .filter(|(_, kind)| wanted.is_none_or(|slug| slug == kind.key))
        .map(|(topic, kind)| summary(topic, &kind, &links))
        .collect();
    entities.sort_by_key(|entity| entity.title.to_lowercase());
    Ok(Json(entities))
}

async fn types(State(gateway): State<Arc<Gateway>>) -> Result<Json<Vec<EntityTypeInfo>>, ApiError> {
    let known = Known::load(&gateway).await?;
    let mut counts = std::collections::BTreeMap::<TopicId, u64>::new();
    for (_, kind) in known.entities() {
        *counts.entry(kind.id).or_default() += 1;
    }
    let mut listed: Vec<EntityTypeInfo> = known
        .topics
        .iter()
        .filter(|topic| topic.kind == TopicKind::Type)
        .map(|topic| EntityTypeInfo {
            key: topic.slug.as_str().to_owned(),
            type_name: topic.name.as_str().to_owned(),
            count: match counts.get(&topic.id) {
                Some(count) => *count,
                None => 0,
            },
        })
        .collect();
    listed.sort_by(|left, right| {
        left.type_name
            .to_lowercase()
            .cmp(&right.type_name.to_lowercase())
            .then(left.key.cmp(&right.key))
    });
    Ok(Json(listed))
}

fn summary(topic: &Topic, kind: &Filed, links: &[kernel::facts::SessionLink]) -> EntitySummary {
    EntitySummary {
        id: topic.id.to_string(),
        title: topic.name.as_str().to_owned(),
        entity_type: kind.key.clone(),
        type_name: kind.name.clone(),
        session_count: links.iter().filter(|link| link.entity == topic.id).count(),
    }
}

async fn show(
    State(gateway): State<Arc<Gateway>>,
    Path(key): Path<String>,
    user: UserId,
    Query(params): Query<ShowParams>,
) -> Result<Json<EntityArticle>, ApiError> {
    let known = Known::load(&gateway).await?;
    let (entity, kind) = resolve(&gateway, &known, &key).await?;
    let viewer = gateway.tenancy.viewer(&user);
    Ok(Json(
        article::build(&gateway, &known, &entity, &kind, &viewer, params.as_of).await?,
    ))
}

async fn set_fact(
    State(gateway): State<Arc<Gateway>>,
    Path(key): Path<String>,
    user: UserId,
    Body(edit): Body<FactEdit>,
) -> Result<Json<Fact>, ApiError> {
    let known = Known::load(&gateway).await?;
    let (entity, _) = resolve(&gateway, &known, &key).await?;
    let store = gateway.facts()?;
    let fact_key = FactKey::new(&edit.key).map_err(map::error)?;
    let value = FactValue::new(&edit.value).map_err(map::error)?;
    if let Some(id) = &edit.id {
        let claims = store.claims(entity.id).await.map_err(map::error)?;
        named(&claims, id, &fact_key)?;
    }
    let now = Utc::now().trunc_subsecs(3);
    store
        .assert_facts(&[NewClaim {
            entity: entity.id,
            key: fact_key.clone(),
            value,
            said: Said::You {
                by: user.owner().clone(),
                pinned: edit.pinned,
            },
            owner: Some(user.owner().clone()),
            at: now,
        }])
        .await
        .map_err(map::error)?;
    gateway.sessions().topic_changed(&entity.id);
    let claims = store.claims(entity.id).await.map_err(map::error)?;
    let state = facts_as_of(&claims, None)
        .into_iter()
        .find(|fact| fact.key == fact_key)
        .ok_or_else(|| ApiError::Unavailable("the fact was not kept".into()))?;
    let titles = article::titles(&gateway, std::slice::from_ref(&state)).await?;
    Ok(Json(map::fact(&state, &titles)))
}

fn named(claims: &[Claim], id: &str, key: &FactKey) -> Result<(), ApiError> {
    let missing = || ApiError::Missing(format!("unknown fact {id}"));
    let id = FactId::from_str(id).map_err(|_| missing())?;
    let found = claims
        .iter()
        .find(|claim| claim.fact == id)
        .ok_or_else(missing)?;
    if found.key == *key {
        Ok(())
    } else {
        Err(ApiError::Conflict(format!(
            "fact {id} is \"{}\", not \"{}\"",
            found.key.as_str(),
            key.as_str()
        )))
    }
}

async fn resolve(gateway: &Gateway, known: &Known, key: &str) -> Result<(Topic, Filed), ApiError> {
    let missing = || ApiError::Missing(format!("unknown entity {key}"));
    let id = TopicId::from_str(key).map_err(|_| missing())?;
    let topic = gateway
        .topics()?
        .find(&TopicKey::Id(id))
        .await
        .map_err(super::topics::topic_error)?
        .filter(|topic| topic.kind == TopicKind::Entity)
        .ok_or_else(missing)?;
    let kind = known.filed(&topic).ok_or_else(missing)?;
    Ok((topic, kind))
}
