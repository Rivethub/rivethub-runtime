use kernel::wiki::entity_match::types_compatible;
use kernel::wiki::entity_pass::EntityStoreError;
use kernel::wiki::entity_pass::MatchCandidate;
use pgvector::HalfVector;
use uuid::Uuid;

use super::error::query_error;

use super::sql::ALIAS_LINKS;
use super::sql::EMBED_ARM;
use super::sql::NAME_ARM;
use super::sql::TRIGRAM_ARM;
use super::types::TypeRow;
use super::types::type_path;

#[derive(Debug, sqlx::FromRow)]
struct ScoreRow {
    id: Uuid,
    name: String,
    type_name: String,
    score: f64,
}

#[derive(Debug)]
pub(super) struct Scored {
    pub(super) id: Uuid,
    pub(super) name: String,
    pub(super) type_name: String,
    pub(super) score: f32,
}

#[derive(Debug, sqlx::FromRow)]
struct AliasLink {
    topic_id: Uuid,
    alias: String,
}

pub(super) fn compatible_ids(wanted: &str, rows: &[TypeRow]) -> Vec<Uuid> {
    rows.iter()
        .filter(|row| {
            types_compatible(wanted, &row.name) || types_compatible(wanted, &type_path(row, rows))
        })
        .map(|row| row.id)
        .collect()
}

pub(super) async fn name_arm(
    pool: &sqlx::PgPool,
    keys: &[String],
) -> Result<Vec<Scored>, EntityStoreError> {
    let rows = sqlx::query_as::<_, ScoreRow>(NAME_ARM)
        .bind(keys)
        .fetch_all(pool)
        .await
        .map_err(query_error)?;
    Ok(rows.into_iter().map(scored).collect())
}

pub(super) fn nonempty(names: &[String]) -> Vec<String> {
    names
        .iter()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .collect()
}

pub(super) async fn trigram_arm(
    pool: &sqlx::PgPool,
    names: &[String],
    parents: &[Uuid],
) -> Result<Vec<Scored>, EntityStoreError> {
    let rows = sqlx::query_as::<_, ScoreRow>(TRIGRAM_ARM)
        .bind(names)
        .bind(parents)
        .fetch_all(pool)
        .await
        .map_err(query_error)?;
    Ok(rows.into_iter().map(scored).collect())
}

pub(super) async fn embedding_arm(
    pool: &sqlx::PgPool,
    vector: &[f32],
    parents: &[Uuid],
) -> Result<Vec<Scored>, EntityStoreError> {
    let rows = sqlx::query_as::<_, ScoreRow>(EMBED_ARM)
        .bind(HalfVector::from_f32_slice(vector))
        .bind(parents)
        .fetch_all(pool)
        .await
        .map_err(query_error)?;
    Ok(rows.into_iter().map(scored).collect())
}

fn scored(row: ScoreRow) -> Scored {
    Scored {
        id: row.id,
        name: row.name,
        type_name: row.type_name,
        score: score32(row.score),
    }
}

fn score32(value: f64) -> f32 {
    if !value.is_finite() || value <= 0.0 {
        return 0.0;
    }
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a similarity score only needs f32 precision inside 0..=1"
    )]
    let score = value.clamp(0.0, 1.0) as f32;
    score
}

pub(super) fn combine(trigram: Vec<Scored>, embedded: Vec<Scored>) -> Vec<MatchCandidate> {
    let mut rows = Vec::new();
    for row in trigram {
        push_score(&mut rows, row, true);
    }
    for row in embedded {
        push_score(&mut rows, row, false);
    }
    rows
}

pub(super) fn push_score(rows: &mut Vec<MatchCandidate>, row: Scored, trigram: bool) {
    if let Some(slot) = rows.iter_mut().find(|kept| kept.id == row.id) {
        if trigram {
            slot.trigram = slot.trigram.max(row.score);
        } else {
            slot.embedding = slot.embedding.max(row.score);
        }
        return;
    }
    rows.push(MatchCandidate {
        id: row.id,
        name: row.name,
        type_name: row.type_name,
        aliases: Vec::new(),
        summary: String::new(),
        trigram: if trigram { row.score } else { 0.0 },
        embedding: if trigram { 0.0 } else { row.score },
    });
}

pub(super) async fn decorate(
    pool: &sqlx::PgPool,
    rows: &mut [MatchCandidate],
) -> Result<(), EntityStoreError> {
    if rows.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = rows.iter().map(|row| row.id).collect();
    let aliases = sqlx::query_as::<_, AliasLink>(ALIAS_LINKS)
        .bind(&ids)
        .fetch_all(pool)
        .await
        .map_err(query_error)?;
    let facts = super::claims::latest_values(pool, &ids).await?;
    for row in rows {
        row.aliases = aliases
            .iter()
            .filter(|link| link.topic_id == row.id)
            .map(|link| link.alias.clone())
            .collect();
        row.summary = facts.get(&row.id).cloned().unwrap_or_default();
    }
    Ok(())
}
