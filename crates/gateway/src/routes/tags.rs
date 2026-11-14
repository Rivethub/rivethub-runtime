use std::collections::BTreeMap;
use std::str::FromStr;
use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::extract::State;
use axum::routing;
use kernel::memory::Limit;
use kernel::memory::MemoryStoreError;
use kernel::memory::TagAdd;
use kernel::memory::TagCountsQuery;
use kernel::memory::TagDecision;
use kernel::memory::TagEntity;
use kernel::memory::TagId;
use kernel::memory::TagListQuery;
use kernel::memory::TagLiteral;
use kernel::memory::TagLookup;
use kernel::memory::TagTaxonomyDecision;
use kernel::memory::TagTaxonomyQuery;
use kernel::memory::TagTaxonomyUpsert;
use serde::Deserialize;
use uuid::Uuid;
use wire::gateway::memory::MemoryTagsAddRequest;
use wire::gateway::memory::MemoryTagsDecideRequest;
use wire::gateway::memory::MemoryTagsLookupRequest;
use wire::gateway::memory::PendingTagListResult;
use wire::gateway::memory::TagAddResult;
use wire::gateway::memory::TagCountsResult;
use wire::gateway::memory::TagDecisionResult;
use wire::gateway::memory::TagEntityType;
use wire::gateway::memory::TagListResult;
use wire::gateway::memory::TagLookupResult;
use wire::gateway::memory::TagState;
use wire::gateway::memory::TagTaxonomyDecideRequest;
use wire::gateway::memory::TagTaxonomyListResult;
use wire::gateway::memory::TagTaxonomyMergeRequest;
use wire::gateway::memory::TagTaxonomyMergeResult;
use wire::gateway::memory::TagTaxonomyUpsertRequest;
use wire::gateway::memory::TagTaxonomyUpsertResult;
use wire::session_id::SessionKey;

use crate::Gateway;
use crate::error::ApiError;
use crate::extract::Body;
use crate::extract::EntityIdParam;
use crate::extract::EntityTypeParam;
use crate::extract::Query;
use crate::extract::RawLimit;
use crate::extract::StateFilter;
use crate::extract::StateParam;
use crate::memory_body;
use crate::tenancy::UserId;

const DECIDE_IDS_MAX: usize = 1000;
const LOOKUP_KEYS_MAX: usize = 500;
const DECIDER_MAX_CHARS: usize = 120;
const LIST_LIMIT_DEFAULT: u32 = 200;
const PENDING_LIMIT_DEFAULT: u32 = 50;
const COUNTS_LIMIT_DEFAULT: u32 = 200;
const TAXONOMY_LIMIT_DEFAULT: u32 = 500;
const TAGS_LIMIT_MAX: u32 = 1000;

pub(crate) fn router() -> Router<Arc<Gateway>> {
    Router::new()
        .route("/", routing::get(list).fallback(super::method_not_allowed))
        .route(
            "/pending",
            routing::get(pending).fallback(super::method_not_allowed),
        )
        .route(
            "/counts",
            routing::get(counts).fallback(super::method_not_allowed),
        )
        .route(
            "/taxonomy",
            routing::get(taxonomy_list)
                .post(taxonomy_upsert)
                .fallback(super::method_not_allowed),
        )
        .route(
            "/decide",
            routing::post(decide).fallback(super::method_not_allowed),
        )
        .route(
            "/add",
            routing::post(add).fallback(super::method_not_allowed),
        )
        .route(
            "/lookup",
            routing::post(lookup).fallback(super::method_not_allowed),
        )
        .route(
            "/taxonomy/decide",
            routing::post(taxonomy_decide).fallback(super::method_not_allowed),
        )
        .route(
            "/taxonomy/merge",
            routing::post(taxonomy_merge).fallback(super::method_not_allowed),
        )
        .fallback(unknown)
        .layer(DefaultBodyLimit::max(super::BODY_LIMIT))
}

async fn unknown() -> ApiError {
    ApiError::NotFound("unknown tags resource")
}

#[derive(Debug, Default, Deserialize)]
struct ListParams {
    entity_type: Option<EntityTypeParam>,
    entity_id: Option<EntityIdParam>,
    key: Option<String>,
    value: Option<String>,
    state: Option<StateFilter>,
    limit: Option<RawLimit>,
}

#[derive(Debug, Default, Deserialize)]
struct KeyLimitParams {
    key: Option<String>,
    limit: Option<RawLimit>,
}

#[derive(Debug, Default, Deserialize)]
struct TaxonomyParams {
    key: Option<String>,
    state: Option<StateParam>,
    limit: Option<RawLimit>,
}

async fn list(
    State(gateway): State<Arc<Gateway>>,
    Query(params): Query<ListParams>,
) -> Result<Json<TagListResult>, ApiError> {
    let query = TagListQuery {
        entity_type: params
            .entity_type
            .map(|entity| memory_body::tag_entity_type(entity.get())),
        entity_id: params.entity_id.map(EntityIdParam::get),
        key: params.key,
        value: params.value,
        states: params.state.map_or_else(Vec::new, |states| {
            states
                .into_vec()
                .into_iter()
                .map(memory_body::tag_state)
                .collect()
        }),
        limit: Limit::clamped(
            params.limit.map(RawLimit::get),
            LIST_LIMIT_DEFAULT,
            1..=TAGS_LIMIT_MAX,
        ),
    };
    match gateway.store().list_tags(query).await {
        Ok(tags) => Ok(Json(TagListResult {
            tags: tags.into_iter().map(memory_body::tag_wire_body).collect(),
        })),
        Err(MemoryStoreError::TagTablesMissing) => Ok(Json(TagListResult { tags: Vec::new() })),
        Err(error) => Err(error.into()),
    }
}

async fn pending(
    State(gateway): State<Arc<Gateway>>,
    Query(params): Query<KeyLimitParams>,
) -> Result<Json<PendingTagListResult>, ApiError> {
    let limit = Limit::clamped(
        params.limit.map(RawLimit::get),
        PENDING_LIMIT_DEFAULT,
        1..=TAGS_LIMIT_MAX,
    );
    match gateway.store().pending_tags(limit).await {
        Ok(tags) => Ok(Json(PendingTagListResult {
            tags: tags
                .into_iter()
                .map(memory_body::pending_tag_wire_body)
                .collect(),
        })),
        Err(MemoryStoreError::TagTablesMissing) => {
            Ok(Json(PendingTagListResult { tags: Vec::new() }))
        }
        Err(error) => Err(error.into()),
    }
}

async fn counts(
    State(gateway): State<Arc<Gateway>>,
    Query(params): Query<KeyLimitParams>,
) -> Result<Json<TagCountsResult>, ApiError> {
    let query = TagCountsQuery {
        key: params.key,
        limit: Limit::clamped(
            params.limit.map(RawLimit::get),
            COUNTS_LIMIT_DEFAULT,
            1..=TAGS_LIMIT_MAX,
        ),
    };
    match gateway.store().tag_counts(query).await {
        Ok(counts) => Ok(Json(TagCountsResult {
            counts: counts
                .into_iter()
                .map(memory_body::tag_count_body)
                .collect(),
        })),
        Err(MemoryStoreError::TagTablesMissing) => Ok(Json(TagCountsResult { counts: Vec::new() })),
        Err(error) => Err(error.into()),
    }
}

async fn taxonomy_list(
    State(gateway): State<Arc<Gateway>>,
    Query(params): Query<TaxonomyParams>,
) -> Result<Json<TagTaxonomyListResult>, ApiError> {
    let query = TagTaxonomyQuery {
        key: params.key,
        state: params
            .state
            .map(|state| memory_body::tag_state(state.get())),
        limit: Limit::clamped(
            params.limit.map(RawLimit::get),
            TAXONOMY_LIMIT_DEFAULT,
            1..=TAGS_LIMIT_MAX,
        ),
    };
    match gateway.store().list_tag_taxonomy(query).await {
        Ok(entries) => Ok(Json(TagTaxonomyListResult {
            entries: entries
                .into_iter()
                .map(memory_body::tag_taxonomy_entry_body)
                .collect(),
        })),
        Err(MemoryStoreError::TagTablesMissing) => Ok(Json(TagTaxonomyListResult {
            entries: Vec::new(),
        })),
        Err(error) => Err(error.into()),
    }
}

async fn decide(
    State(gateway): State<Arc<Gateway>>,
    Body(body): Body<MemoryTagsDecideRequest>,
) -> Result<Json<TagDecisionResult>, ApiError> {
    let decision = match body.state {
        Some(TagState::Accepted) => TagDecision::Accept,
        Some(TagState::Rejected) => TagDecision::Reject,
        Some(TagState::Suggested) | None => {
            return Err(ApiError::BadRequest(
                "state must be accepted or rejected".to_owned(),
            ));
        }
    };
    let ids = body
        .ids
        .filter(|ids| !ids.is_empty())
        .ok_or_else(|| ApiError::BadRequest("ids required".to_owned()))?;
    if ids.len() > DECIDE_IDS_MAX {
        return Err(ApiError::BadRequest("at most 1000 ids".to_owned()));
    }
    let mut parsed = Vec::with_capacity(ids.len());
    for id in ids {
        let id = Uuid::from_str(&id)
            .map_err(|_| ApiError::BadRequest("ids must be uuids".to_owned()))?;
        parsed.push(TagId::from_uuid(id));
    }
    let changed = gateway
        .store()
        .decide_tags(
            parsed,
            decision,
            body.decided_by
                .map(|value| value.trim().chars().take(DECIDER_MAX_CHARS).collect()),
        )
        .await?;
    Ok(Json(TagDecisionResult { changed }))
}

async fn add(
    State(gateway): State<Arc<Gateway>>,
    user: UserId,
    Body(body): Body<MemoryTagsAddRequest>,
) -> Result<Json<TagAddResult>, ApiError> {
    let entity_type = match &body.entity_type {
        Some(raw) => enum_value::<TagEntityType>(raw, "bad entity_type")?,
        None => {
            return Err(ApiError::BadRequest(
                "entity_type and entity_id (or session_key) required".to_owned(),
            ));
        }
    };
    let entity = entity_of(&body)?;
    if entity_type == TagEntityType::Conversation {
        owned(&gateway, &user, &entity).await?;
    }
    let tag = literal_of(&body)?;
    let add = TagAdd {
        entity_type: memory_body::tag_entity_type(entity_type),
        entity,
        agent: body.agent,
        tag,
        display: body.display,
        reason: body.reason,
        decided_by: body
            .decided_by
            .map(|value| value.trim().chars().take(DECIDER_MAX_CHARS).collect()),
    };
    match gateway.store().add_tag(add).await {
        Ok(tag) => Ok(Json(TagAddResult {
            tag: memory_body::tag_wire_body(tag),
        })),
        Err(error) => Err(error.into()),
    }
}

async fn owned(gateway: &Gateway, user: &UserId, entity: &TagEntity) -> Result<(), ApiError> {
    match entity {
        TagEntity::Id(conversation) => {
            crate::session_access::change_conversation(gateway, user, *conversation).await
        }
        TagEntity::SessionKey(key) => {
            let parsed = key.parse::<SessionKey>();
            #[cfg_attr(
                dylint_lib = "rr_lints",
                expect(
                    fallback,
                    reason = "CONTRACT B1: a legacy key that is not a session key names no session with an owner (note 160), so the store tags it as before"
                )
            )]
            let checked = match parsed {
                Ok(key) => crate::session_access::change_key(gateway, user, &key).await,
                Err(_) => Ok(()),
            };
            checked
        }
    }
}

fn enum_value<T: serde::de::DeserializeOwned>(
    raw: &str,
    message: &'static str,
) -> Result<T, ApiError> {
    crate::extract::parse_enum(raw).ok_or_else(|| ApiError::BadRequest(message.to_owned()))
}

fn entity_of(body: &MemoryTagsAddRequest) -> Result<TagEntity, ApiError> {
    match (body.entity_id.as_deref(), body.session_key.as_deref()) {
        (Some(raw), _) => Uuid::from_str(raw)
            .map(TagEntity::Id)
            .map_err(|_| ApiError::BadRequest("bad entity_id".to_owned())),
        (None, Some(key)) if !key.trim().is_empty() => Ok(TagEntity::SessionKey(key.to_owned())),
        (None, _) => Err(ApiError::BadRequest(
            "entity_type and entity_id (or session_key) required".to_owned(),
        )),
    }
}

fn literal_of(body: &MemoryTagsAddRequest) -> Result<TagLiteral, ApiError> {
    match (body.tag.as_deref(), &body.key, &body.value) {
        (Some(raw), _, _) => {
            TagLiteral::parse_add(raw).map_err(|error| ApiError::BadRequest(error.to_string()))
        }
        (None, Some(key), Some(value)) if present(key) && present(value) => Ok(TagLiteral {
            key: key.clone(),
            value: value.clone(),
        }),
        (None, _, _) => Err(ApiError::BadRequest("tag required".to_owned())),
    }
}

async fn lookup(
    State(gateway): State<Arc<Gateway>>,
    Body(body): Body<MemoryTagsLookupRequest>,
) -> Result<Json<TagLookupResult>, ApiError> {
    let session_keys = body
        .session_keys
        .filter(|keys| !keys.is_empty())
        .ok_or_else(|| ApiError::BadRequest("session_keys required".to_owned()))?;
    if session_keys.len() > LOOKUP_KEYS_MAX {
        return Err(ApiError::BadRequest("at most 500 session_keys".to_owned()));
    }
    let mut states = Vec::new();
    if let Some(raw_states) = &body.states {
        for raw in raw_states {
            states.push(memory_body::tag_state(enum_value::<TagState>(
                raw,
                "bad state",
            )?));
        }
    }
    let lookup = TagLookup {
        session_keys,
        states,
    };
    match gateway.store().lookup_tags(lookup).await {
        Ok(sessions) => Ok(Json(TagLookupResult {
            sessions: sessions
                .into_iter()
                .map(|(key, tags)| {
                    (
                        key,
                        tags.into_iter().map(memory_body::tag_wire_body).collect(),
                    )
                })
                .collect(),
        })),
        Err(MemoryStoreError::TagTablesMissing) => Ok(Json(TagLookupResult {
            sessions: BTreeMap::new(),
        })),
        Err(error) => Err(error.into()),
    }
}

async fn taxonomy_upsert(
    State(gateway): State<Arc<Gateway>>,
    Body(body): Body<TagTaxonomyUpsertRequest>,
) -> Result<Json<TagTaxonomyUpsertResult>, ApiError> {
    let required = || ApiError::BadRequest("key and value required".to_owned());
    let key = body
        .key
        .as_deref()
        .filter(|key| present(key))
        .ok_or_else(required)?;
    let value = body
        .value
        .as_deref()
        .filter(|value| present(value))
        .ok_or_else(required)?;
    let upsert = TagTaxonomyUpsert {
        key: key.to_owned(),
        value: value.to_owned(),
        display: body.display.clone(),
        parent_value: body.parent_value.clone(),
        aliases: body.aliases.clone().unwrap_or_default(),
        reason: body.reason.clone(),
    };
    match gateway.store().upsert_tag_taxonomy(upsert).await {
        Ok(entry) => Ok(Json(TagTaxonomyUpsertResult {
            entry: memory_body::tag_taxonomy_entry_body(entry),
        })),
        Err(error) => Err(error.into()),
    }
}

async fn taxonomy_decide(
    State(gateway): State<Arc<Gateway>>,
    Body(body): Body<TagTaxonomyDecideRequest>,
) -> Result<Json<TagDecisionResult>, ApiError> {
    let decision = match body.state {
        Some(TagState::Accepted) => TagDecision::Accept,
        Some(TagState::Rejected) => TagDecision::Reject,
        Some(TagState::Suggested) | None => {
            return Err(ApiError::BadRequest(
                "state must be accepted or rejected".to_owned(),
            ));
        }
    };
    let entries = body
        .entries
        .filter(|entries| !entries.is_empty())
        .ok_or_else(|| ApiError::BadRequest("entries required".to_owned()))?;
    let changed = gateway
        .store()
        .decide_tag_taxonomy(TagTaxonomyDecision {
            entries: entries
                .into_iter()
                .map(memory_body::tag_entry_pair)
                .collect(),
            decision,
            decided_by: body
                .decided_by
                .map(|value| value.trim().chars().take(DECIDER_MAX_CHARS).collect()),
        })
        .await?;
    Ok(Json(TagDecisionResult { changed }))
}

async fn taxonomy_merge(
    State(gateway): State<Arc<Gateway>>,
    Body(body): Body<TagTaxonomyMergeRequest>,
) -> Result<Json<TagTaxonomyMergeResult>, ApiError> {
    let merge = (
        body.key.as_deref().filter(|value| present(value)),
        body.from.as_deref().filter(|value| present(value)),
        body.into.as_deref().filter(|value| present(value)),
    );
    let (Some(key), Some(from), Some(into)) = merge else {
        return Err(ApiError::BadRequest(
            "key, from and into required".to_owned(),
        ));
    };
    let result = gateway
        .store()
        .merge_tag_taxonomy(kernel::memory::TagTaxonomyMerge {
            key: key.to_owned(),
            from: from.to_owned(),
            into: into.to_owned(),
        })
        .await?;
    Ok(Json(memory_body::tag_taxonomy_merge_result_body(result)))
}

fn present(value: &str) -> bool {
    !value.trim().is_empty()
}
