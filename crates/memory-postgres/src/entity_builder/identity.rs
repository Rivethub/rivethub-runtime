use chrono::DateTime;
use chrono::Utc;
use kernel::wiki::entity_match::canonical_key;
use kernel::wiki::entity_match::cross_names_match;
use kernel::wiki::entity_match::identity_keys;
use kernel::wiki::entity_match::names_same_entity;
use kernel::wiki::entity_match::types_compatible;
use kernel::wiki::entity_pass::BindHit;
use kernel::wiki::entity_pass::BindRequest;
use kernel::wiki::entity_pass::EntityStoreError;
use kernel::wiki::entity_pass::PreparedEntity;
use kernel::wiki::entity_pass::SlugOccupant;
use kernel::wiki::entity_pass::attribute_key;
use kernel::wiki::entity_pass::choose_slug;
use kernel::wiki::entity_pass::pick_existing;
use sqlx::Postgres;
use sqlx::Transaction;
use uuid::Uuid;

use super::error::query_error;

use super::sql::CLAIMS_NAME;
use super::sql::IDENTITY_HITS;
use super::types::TypeRow;
use super::types::remember_type;
use super::types::retype_entity;

#[derive(Debug, sqlx::FromRow)]
struct HitRow {
    id: Uuid,
}

#[derive(Debug, sqlx::FromRow)]
struct NameHit {
    id: Uuid,
    parent_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    type_name: String,
}

#[derive(Debug, sqlx::FromRow)]
struct OccupiedSlug {
    slug: String,
}

pub(super) struct Resolved {
    pub(super) id: Uuid,
}

pub(super) struct ResolveAt<'a> {
    pub(super) kind: &'a TypeRow,
    pub(super) types: &'a [TypeRow],
    pub(super) owner: &'a str,
    pub(super) summary: Option<Uuid>,
    pub(super) started: DateTime<Utc>,
    pub(super) now: DateTime<Utc>,
}

pub(super) async fn resolve(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedEntity,
    at: ResolveAt<'_>,
) -> Result<Resolved, EntityStoreError> {
    let hits = identity_hits(tx, &identity_keys(&prepared.name)).await?;
    let named: Vec<BindHit> = hits
        .iter()
        .filter(|hit| names_same_entity(&prepared.type_name, &hit.type_name))
        .filter_map(|hit| {
            hit.parent_id.map(|type_id| BindHit {
                id: hit.id,
                type_id,
                created_at: hit.created_at,
            })
        })
        .collect();
    let id = if let Some(hit) = oldest_hit(&named) {
        hit.id
    } else {
        let referred = referred(tx, prepared.entity_ref).await?;
        let ref_exists =
            ref_is_compatible(tx, referred.as_ref(), &prepared.type_name, &prepared.name).await?;
        let request = BindRequest {
            entity_ref: prepared.entity_ref,
            ref_exists,
            type_id: Some(at.kind.id),
        };
        if let Some(id) = pick_existing(&request, &[]) {
            id
        } else {
            insert_entity(tx, prepared, at.kind, at.owner, at.started, at.now)
                .await?
                .id
        }
    };
    let resolved = Resolved { id };
    if let Some(summary) = at.summary {
        remember_type(tx, id, summary, &prepared.type_name, at.started, at.owner).await?;
        retype_entity(tx, id, at.types).await?;
    }
    Ok(resolved)
}

fn oldest_hit(hits: &[BindHit]) -> Option<&BindHit> {
    hits.iter().min_by_key(|hit| (hit.created_at, hit.id))
}

async fn referred(
    tx: &mut Transaction<'_, Postgres>,
    id: Option<Uuid>,
) -> Result<Option<HitRow>, EntityStoreError> {
    let Some(id) = id else {
        return Ok(None);
    };
    sqlx::query_as::<_, HitRow>("SELECT id FROM rh_topics WHERE id = $1 AND kind = 'entity'")
        .bind(id)
        .fetch_optional(&mut **tx)
        .await
        .map_err(query_error)
}

async fn identity_hits(
    tx: &mut Transaction<'_, Postgres>,
    keys: &[String],
) -> Result<Vec<NameHit>, EntityStoreError> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    sqlx::query_as::<_, NameHit>(IDENTITY_HITS)
        .bind(keys)
        .fetch_all(&mut **tx)
        .await
        .map_err(query_error)
}

async fn insert_entity(
    tx: &mut Transaction<'_, Postgres>,
    prepared: &PreparedEntity,
    kind: &TypeRow,
    owner: &str,
    started: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<Resolved, EntityStoreError> {
    let base = attribute_key(&prepared.name).unwrap_or_else(|| kind.slug.clone());
    for _attempt in 0..3 {
        let Some(slug) = slug_available(tx, &base, &kind.slug).await? else {
            return Err(EntityStoreError::SlugTaken(prepared.name.clone()));
        };
        let id = Uuid::new_v4();
        let inserted = sqlx::query(
            "INSERT INTO rh_topics (id, kind, slug, name, parent_id, created_at, updated_at, owner_user_id)
             VALUES ($1, 'entity', $2, $3, $4, $5, $6, $7)",
        )
        .bind(id)
        .bind(&slug)
        .bind(&prepared.name)
        .bind(kind.id)
        .bind(started)
        .bind(now)
        .bind(owner)
        .execute(&mut **tx)
        .await;
        match inserted {
            Ok(_) => return Ok(Resolved { id }),
            Err(error) => {
                let wrapped = query_error(error);
                if is_unique(&wrapped) {
                    continue;
                }
                return Err(wrapped);
            }
        }
    }
    Err(EntityStoreError::SlugTaken(prepared.name.clone()))
}

fn is_unique(error: &EntityStoreError) -> bool {
    let EntityStoreError::Query(source) = error else {
        return false;
    };
    source.downcast_ref::<sqlx::Error>().is_some_and(|error| {
        error
            .as_database_error()
            .is_some_and(|db| db.code().as_deref() == Some("23505"))
    })
}

async fn slug_available(
    tx: &mut Transaction<'_, Postgres>,
    base: &str,
    type_slug: &str,
) -> Result<Option<String>, EntityStoreError> {
    let mut candidates = vec![base.to_owned()];
    if !type_slug.is_empty() && type_slug != base {
        candidates.push(format!("{base}-{type_slug}"));
        candidates.push(format!("{type_slug}-{base}"));
    }
    let topics =
        sqlx::query_as::<_, OccupiedSlug>("SELECT slug FROM rh_topics WHERE slug = ANY($1)")
            .bind(&candidates)
            .fetch_all(&mut **tx)
            .await
            .map_err(query_error)?;
    let pages = sqlx::query_scalar::<_, String>(
        "SELECT slug FROM rh_wiki_topics WHERE slug = ANY($1)
         UNION
         SELECT local_slug FROM rh_wiki_topics WHERE local_slug = ANY($1)",
    )
    .bind(&candidates)
    .fetch_all(&mut **tx)
    .await
    .map_err(query_error)?;
    Ok(choose_slug(base, type_slug, |candidate| {
        let taken = topics.iter().any(|row| row.slug == candidate)
            || pages.iter().any(|slug| slug == candidate);
        if taken {
            SlugOccupant::Other
        } else {
            SlugOccupant::Free
        }
    }))
}

pub(super) async fn write_aliases(
    tx: &mut Transaction<'_, Postgres>,
    entity: Uuid,
    prepared: &PreparedEntity,
) -> Result<(), EntityStoreError> {
    let mut names = prepared.aliases.clone();
    names.push(prepared.name.clone());
    let own = canonical_key(&prepared.name);
    for name in names {
        let Some(key) = attribute_key(&name) else {
            continue;
        };
        let folded = canonical_key(&name);
        if folded != own && claims_other_name(tx, entity, &folded).await? {
            tracing::info!(
                target: "entity_alias",
                %entity,
                alias = %name,
                "alias matches another entity name"
            );
            continue;
        }
        sqlx::query(
            "INSERT INTO rh_topic_aliases (topic_id, alias, alias_key, kind)
             VALUES ($1, $2, $3, 'entity')
             ON CONFLICT (kind, alias_key) DO NOTHING",
        )
        .bind(entity)
        .bind(name.trim())
        .bind(key)
        .execute(&mut **tx)
        .await
        .map_err(query_error)?;
    }
    Ok(())
}

async fn claims_other_name(
    tx: &mut Transaction<'_, Postgres>,
    entity: Uuid,
    key: &str,
) -> Result<bool, EntityStoreError> {
    if key.is_empty() {
        return Ok(false);
    }
    sqlx::query_scalar(CLAIMS_NAME)
        .bind(entity)
        .bind(key)
        .fetch_one(&mut **tx)
        .await
        .map_err(query_error)
}

async fn ref_is_compatible(
    tx: &mut Transaction<'_, Postgres>,
    hit: Option<&HitRow>,
    type_name: &str,
    observed_name: &str,
) -> Result<bool, EntityStoreError> {
    let Some(hit) = hit else {
        return Ok(false);
    };
    let row = sqlx::query_as::<_, RefKind>(
        "SELECT e.name, ty.name AS type_name
           FROM rh_topics e
           JOIN rh_topics ty ON ty.id = e.parent_id
          WHERE e.id = $1",
    )
    .bind(hit.id)
    .fetch_optional(&mut **tx)
    .await
    .map_err(query_error)?;
    Ok(row.is_some_and(|row| {
        types_compatible(type_name, &row.type_name)
            || cross_names_match(type_name, observed_name, &row.type_name, &row.name)
    }))
}

#[derive(Debug, sqlx::FromRow)]
struct RefKind {
    name: String,
    type_name: String,
}
