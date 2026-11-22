use jiff::Timestamp;
use std::collections::HashMap;

use kernel::wiki::entity_pass::EntityLeaf;
use kernel::wiki::entity_pass::EntityStoreError;
use kernel::wiki::entity_pass::KnownEntity;
use kernel::wiki::entity_pass::{known_document, known_keys, mention_score};
use uuid::Uuid;

use super::error::query_error;

use super::sql::KNOWN_ALIASES;
use super::sql::KNOWN_IDS;
use super::sql::KNOWN_ROWS;

#[derive(Debug, sqlx::FromRow)]
struct KnownRow {
    id: Uuid,
    name: String,
    #[sqlx(try_from = "jiff_sqlx::Timestamp")]
    created_at: Timestamp,
    type_name: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct KnownAlias {
    topic_id: Uuid,
    alias: String,
}

pub(super) async fn known_entities(
    pool: &sqlx::PgPool,
    leaves: &[EntityLeaf],
) -> Result<Vec<KnownEntity>, EntityStoreError> {
    let document = known_document(leaves);
    if document.trim().is_empty() {
        return Ok(Vec::new());
    }
    let keys = known_keys(&document);
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let ids = sqlx::query_scalar::<_, Uuid>(KNOWN_IDS)
        .persistent(false)
        .bind(&keys)
        .fetch_all(pool)
        .await
        .map_err(query_error)?;
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, KnownRow>(KNOWN_ROWS)
        .bind(&ids)
        .fetch_all(pool)
        .await
        .map_err(query_error)?;
    let mut pending = hydrate_known(pool, rows).await?;
    order_known(&mut pending, &document);
    let limit = usize::try_from(kernel::wiki::entity_pass::KNOWN_LIMIT).map_err(|error| {
        EntityStoreError::Query(Box::new(std::io::Error::other(error.to_string())))
    })?;
    pending.truncate(limit);
    Ok(pending.into_iter().map(|row| row.entity).collect())
}

struct PendingKnown {
    score: i32,
    created_at: Timestamp,
    entity: KnownEntity,
}

async fn hydrate_known(
    pool: &sqlx::PgPool,
    rows: Vec<KnownRow>,
) -> Result<Vec<PendingKnown>, EntityStoreError> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let alias_rows = sqlx::query_as::<_, KnownAlias>(KNOWN_ALIASES)
        .bind(&ids)
        .fetch_all(pool)
        .await
        .map_err(query_error)?;
    let mut facts = super::claims::known_facts(pool, &ids).await?;
    let mut aliases: HashMap<Uuid, Vec<String>> = HashMap::new();
    for row in alias_rows {
        aliases.entry(row.topic_id).or_default().push(row.alias);
    }
    Ok(rows
        .into_iter()
        .map(|row| {
            let aliases = aliases.remove(&row.id).unwrap_or_default();
            let facts = facts.remove(&row.id).unwrap_or_default();
            PendingKnown {
                score: 0,
                created_at: row.created_at,
                entity: KnownEntity {
                    id: row.id,
                    name: row.name,
                    type_name: row.type_name.unwrap_or_default(),
                    aliases,
                    facts,
                },
            }
        })
        .collect())
}

fn order_known(rows: &mut [PendingKnown], document: &str) {
    let body = document.to_lowercase();
    for row in rows.iter_mut() {
        row.score = mention_score(&body, &row.entity.name, &row.entity.aliases);
    }
    rows.sort_by(|left, right| {
        right
            .score
            .cmp(&left.score)
            .then(left.created_at.cmp(&right.created_at))
            .then(left.entity.id.cmp(&right.entity.id))
    });
}
