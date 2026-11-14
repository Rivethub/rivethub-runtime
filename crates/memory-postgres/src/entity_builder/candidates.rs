use kernel::wiki::entity_match::identity_keys;
use kernel::wiki::entity_match::names_same_entity;
use kernel::wiki::entity_pass::CandidateQuery;
use kernel::wiki::entity_pass::EntityStoreError;
use kernel::wiki::entity_pass::MatchCandidate;
use kernel::wiki::entity_pass::compact_key;
use pgvector::HalfVector;
use uuid::Uuid;

use super::error::query_error;

use super::sql::ALIAS_LINKS;
use super::sql::EMBED_ARM;
use super::sql::MARKER_ARM;
use super::sql::NAME_ARM;
use super::sql::PAGE_TEXT;
use super::sql::TRIGRAM_ARM;

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
) -> Result<Vec<Scored>, EntityStoreError> {
    let rows = sqlx::query_as::<_, ScoreRow>(TRIGRAM_ARM)
        .bind(names)
        .fetch_all(pool)
        .await
        .map_err(query_error)?;
    Ok(rows.into_iter().map(scored).collect())
}

pub(super) async fn embedding_arm(
    pool: &sqlx::PgPool,
    vector: &[f32],
) -> Result<Vec<Scored>, EntityStoreError> {
    let rows = sqlx::query_as::<_, ScoreRow>(EMBED_ARM)
        .bind(HalfVector::from_f32_slice(vector))
        .fetch_all(pool)
        .await
        .map_err(query_error)?;
    Ok(rows.into_iter().map(scored).collect())
}

pub(super) async fn marker_arm(
    pool: &sqlx::PgPool,
    markers: &[String],
) -> Result<Vec<Scored>, EntityStoreError> {
    let rows = sqlx::query_as::<_, ScoreRow>(MARKER_ARM)
        .bind(markers)
        .fetch_all(pool)
        .await
        .map_err(query_error)?;
    Ok(rows.into_iter().map(scored).collect())
}

pub(super) async fn gather(
    pool: &sqlx::PgPool,
    query: &CandidateQuery,
) -> Result<Vec<MatchCandidate>, EntityStoreError> {
    let names = nonempty(&query.names);
    let keys = identity_of(&names, &query.markers);
    let trigram = if names.is_empty() {
        Vec::new()
    } else {
        trigram_arm(pool, &names).await?
    };
    let embedded = match query
        .embedding
        .as_deref()
        .filter(|vector| vector.len() == 1024)
    {
        Some(vector) => embedding_arm(pool, vector).await?,
        None => Vec::new(),
    };
    let mut rows = combine(trigram, embedded);
    if !keys.is_empty() {
        for row in name_arm(pool, &keys).await? {
            push_score(&mut rows, row, true);
        }
    }
    let markers = page_markers(&query.markers);
    if !markers.is_empty() {
        for row in marker_arm(pool, &markers).await? {
            push_score(&mut rows, row, true);
        }
    }
    rows.retain(|row| names_same_entity(&query.type_name, &row.type_name));
    decorate(pool, &mut rows).await?;
    Ok(rows)
}

fn identity_of(names: &[String], markers: &[String]) -> Vec<String> {
    let mut keys = Vec::new();
    for name in names {
        keys.extend(identity_keys(name));
    }
    for marker in markers {
        let key = compact_key(marker);
        if key.chars().count() >= 3 {
            keys.push(key);
        }
    }
    keys.sort();
    keys.dedup();
    keys
}

fn page_markers(markers: &[String]) -> Vec<String> {
    let mut kept = Vec::new();
    for marker in markers {
        let marker = marker.trim();
        if marker.chars().count() < 3 || kept.iter().any(|item: &String| item == marker) {
            continue;
        }
        kept.push(marker.to_owned());
    }
    kept
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
    let pages = sqlx::query_as::<_, PageText>(PAGE_TEXT)
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
        let page = pages.iter().find(|item| item.topic_id == row.id);
        let (state, article) = match page {
            Some(page) => (page.current_state.as_str(), page.article.as_str()),
            None => ("", ""),
        };
        let fact = match facts.get(&row.id) {
            Some(value) => value.as_str(),
            None => "",
        };
        row.summary = clip_page(state, article, fact);
    }
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct PageText {
    topic_id: Uuid,
    current_state: String,
    article: String,
}

fn clip_page(state: &str, article: &str, facts: &str) -> String {
    let mut parts = Vec::new();
    for part in [state.trim(), article.trim(), facts.trim()] {
        if !part.is_empty() {
            parts.push(part);
        }
    }
    parts.join("\n").chars().take(800).collect()
}
