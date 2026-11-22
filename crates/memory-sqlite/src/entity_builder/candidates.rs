use std::collections::{HashMap, HashSet};

use half::f16;
use jiff::Timestamp;
use kernel::wiki::entity_match::{identity_keys, names_same_entity};
use kernel::wiki::entity_pass::{CandidateQuery, EntityStoreError, MatchCandidate, compact_key};
use rusqlite::{Connection, OptionalExtension as _, params};
use uuid::Uuid;

use super::identity::{self, EntityRow};
use super::{claims, error, query, rows, stamp, uuid};
use crate::trigrams::Scorer;
use crate::vector;

const DIMENSIONS: usize = 1024;
const TRIGRAM_LIMIT: usize = 20;
const EMBEDDING_LIMIT: usize = 10;
const EXACT_LIMIT: usize = 48;

pub(super) fn gather(
    conn: &Connection,
    input: &CandidateQuery,
) -> Result<Vec<MatchCandidate>, EntityStoreError> {
    let names: Vec<_> = input
        .names
        .iter()
        .map(|name| name.trim())
        .filter(|name| !name.is_empty())
        .collect();
    let keys = keys(&names, &input.markers);
    let entities: Vec<_> = identity::entities(conn)?
        .into_iter()
        .filter(|row| row.type_name.is_some())
        .collect();
    let mut ordered = Vec::new();
    if !names.is_empty() {
        let scorer = Scorer::new().map_err(error)?;
        let ranked = entities
            .iter()
            .map(|row| (row, trigram(row, &names, scorer)))
            .collect();
        for (row, score) in top(ranked, TRIGRAM_LIMIT) {
            push(&mut ordered, row, score, true);
        }
    }
    if let Some(vector) = input
        .embedding
        .as_deref()
        .filter(|values| values.len() == DIMENSIONS)
    {
        for (row, score) in embedded(conn, &entities, vector)? {
            push(&mut ordered, row, score, false);
        }
    }
    for row in entities
        .iter()
        .filter(|row| identity::exact_name(row, &keys))
        .take(EXACT_LIMIT)
    {
        push(&mut ordered, row, 1.0, true);
    }
    let hits = markers(conn, &input.markers)?;
    for row in entities
        .iter()
        .filter(|row| hits.contains(&row.id))
        .take(EXACT_LIMIT)
    {
        push(&mut ordered, row, 1.0, true);
    }
    ordered.retain(|row| names_same_entity(&input.type_name, &row.type_name));
    for row in &mut ordered {
        row.summary = context(conn, row.id)?;
    }
    Ok(ordered)
}

fn keys(names: &[&str], markers: &[String]) -> Vec<String> {
    let mut keys: Vec<_> = names.iter().flat_map(|name| identity_keys(name)).collect();
    keys.extend(
        markers
            .iter()
            .map(|marker| compact_key(marker))
            .filter(|key| key.chars().count() >= 3),
    );
    keys.sort();
    keys.dedup();
    keys
}

fn trigram(row: &EntityRow, terms: &[&str], scorer: Scorer) -> f32 {
    terms
        .iter()
        .flat_map(|term| {
            std::iter::once(&row.name)
                .chain(row.aliases.iter().map(|alias| &alias.name))
                .map(move |name| scorer.score(term, name))
        })
        .fold(0.0, f32::max)
}

fn top(mut rows: Vec<(&EntityRow, f32)>, limit: usize) -> Vec<(&EntityRow, f32)> {
    rows.sort_by(|(left, a), (right, b)| b.total_cmp(a).then(left.id.cmp(&right.id)));
    rows.truncate(limit);
    rows
}

fn push(rows: &mut Vec<MatchCandidate>, row: &EntityRow, score: f32, trigram: bool) {
    let score = if score.is_finite() {
        score.clamp(0.0, 1.0)
    } else {
        0.0
    };
    if let Some(slot) = rows.iter_mut().find(|kept| kept.id == row.id) {
        if trigram {
            slot.trigram = slot.trigram.max(score);
        } else {
            slot.embedding = slot.embedding.max(score);
        }
    } else {
        rows.push(MatchCandidate {
            id: row.id,
            name: row.name.clone(),
            type_name: row.type_name.clone().unwrap_or_default(),
            aliases: row.aliases.iter().map(|alias| alias.name.clone()).collect(),
            summary: String::new(),
            trigram: if trigram { score } else { 0.0 },
            embedding: if trigram { 0.0 } else { score },
        });
    }
}

fn markers(conn: &Connection, markers: &[String]) -> Result<HashSet<Uuid>, EntityStoreError> {
    let markers: HashSet<_> = markers
        .iter()
        .map(|value| value.trim().to_lowercase())
        .filter(|value| value.chars().count() >= 3)
        .collect();
    if markers.is_empty() {
        return Ok(HashSet::new());
    }
    let markers = serde_json::to_string(&markers).map_err(error)?;
    rows(conn,
        "SELECT f.topic_id FROM rh_entity_facts f JOIN rh_entity_fact_claims c ON c.fact_id = f.id
         WHERE EXISTS (SELECT 1 FROM json_each(?1) q WHERE instr(rh_entity_lower(c.value), q.value) > 0)
         UNION
         SELECT p.topic_id FROM rh_topic_pages p JOIN ros_wiki_topics w ON w.slug = p.page_slug
         WHERE p.topic_id IS NOT NULL AND EXISTS (SELECT 1 FROM json_each(?1) q
            WHERE instr(rh_entity_lower(w.current_state), q.value) > 0
               OR instr(rh_entity_lower(substr(w.article, 1, 4000)), q.value) > 0)",
        [markers], |row| row.get::<_, String>(0),
    )?.into_iter().map(|id| uuid(&id)).collect::<Result<_, _>>()
}

pub(crate) fn register(conn: &Connection) -> Result<(), crate::SqliteError> {
    use rusqlite::functions::FunctionFlags;
    conn.create_scalar_function(
        "rh_entity_lower",
        1,
        FunctionFlags::SQLITE_UTF8 | FunctionFlags::SQLITE_DETERMINISTIC,
        |ctx| Ok(ctx.get_raw(0).as_str()?.to_lowercase()),
    )
    .map_err(crate::SqliteError::query("register entity matching"))
}

fn context(conn: &Connection, id: Uuid) -> Result<String, EntityStoreError> {
    let page: Option<(String, String)> = conn.query_row(
        "SELECT w.current_state, substr(w.article, 1, 400)
         FROM rh_topic_pages p JOIN ros_wiki_topics w ON w.slug = p.page_slug WHERE p.topic_id = ?1",
        [id.to_string()], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(query)?;
    let (state, article) = page.unwrap_or_default();
    let facts = claims::current(conn, id)?;
    let fact = facts
        .facts
        .iter()
        .max_by(|left, right| {
            left.as_of
                .cmp(&right.as_of)
                .then(right.attribute.cmp(&left.attribute))
        })
        .map_or("", |fact| &fact.value);
    Ok([state.trim(), article.trim(), fact.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .chars()
        .take(800)
        .collect())
}

fn half_values(values: &[f32]) -> Result<Vec<f32>, EntityStoreError> {
    let values: Vec<_> = values
        .iter()
        .map(|value| f16::from_f32(*value).to_f32())
        .collect();
    if values.iter().any(|value| !value.is_finite()) {
        return Err(error(std::io::Error::other(
            "entity embedding is not finite in half precision",
        )));
    }
    Ok(values)
}

pub(super) fn remember(
    conn: &Connection,
    id: Uuid,
    values: &[f32],
    now: Timestamp,
) -> Result<(), EntityStoreError> {
    if values.len() != DIMENSIONS {
        return Ok(());
    }
    let data = vector::pack(&half_values(values)?);
    conn.execute(
        "INSERT INTO rh_entity_embeddings (entity_id, embedding, embedded_at, owner_user_id)
         SELECT id, ?2, ?3, coalesce(owner_user_id, '') FROM rh_topics WHERE id = ?1
         ON CONFLICT (entity_id) DO UPDATE SET embedding = excluded.embedding,
             embedded_at = excluded.embedded_at",
        params![id.to_string(), data, stamp(now)],
    )
    .map_err(query)?;
    Ok(())
}

fn embedded<'a>(
    conn: &Connection,
    entities: &'a [EntityRow],
    values: &[f32],
) -> Result<Vec<(&'a EntityRow, f32)>, EntityStoreError> {
    let values = half_values(values)?;
    let mut saved: HashMap<_, _> = rows(
        conn,
        "SELECT entity_id, embedding FROM rh_entity_embeddings",
        [],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
    )?
    .into_iter()
    .collect();
    let mut scored = Vec::new();
    for entity in entities {
        let Some(bytes) = saved.remove(&entity.id.to_string()) else {
            continue;
        };
        let embedding = vector::unpack(&bytes)
            .map_err(|()| error(std::io::Error::other("invalid stored entity embedding")))?;
        scored.push((entity, cosine_distance(&values, &embedding)));
    }
    scored.sort_by(|(left, a), (right, b)| a.total_cmp(b).then(left.id.cmp(&right.id)));
    scored.truncate(EMBEDDING_LIMIT);
    Ok(scored
        .into_iter()
        .map(|(row, distance)| {
            #[expect(clippy::cast_possible_truncation)]
            let score = (1.0 - distance) as f32;
            (row, score)
        })
        .collect())
}

fn cosine_distance(left: &[f32], right: &[f32]) -> f64 {
    let sums = if vector_lanes_available() {
        lane_products(left, right)
    } else {
        left.iter()
            .zip(right)
            .fold(Products::default(), |sums, (&a, &b)| {
                sums + Products {
                    dot: a * b,
                    left: a * a,
                    right: b * b,
                }
            })
    };
    let score = f64::from(sums.dot) / (f64::from(sums.left) * f64::from(sums.right)).sqrt();
    if score.is_finite() {
        1.0 - score.clamp(-1.0, 1.0)
    } else {
        f64::INFINITY
    }
}

#[derive(Clone, Copy, Default)]
struct Products {
    dot: f32,
    left: f32,
    right: f32,
}

impl std::ops::Add for Products {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self {
            dot: self.dot + rhs.dot,
            left: self.left + rhs.left,
            right: self.right + rhs.right,
        }
    }
}

fn lane_products(left: &[f32], right: &[f32]) -> Products {
    let mut lanes = [Products::default(); 8];
    for (left, right) in left.as_chunks::<8>().0.iter().zip(right.as_chunks::<8>().0) {
        for ((&a, &b), lane) in left.iter().zip(right).zip(&mut lanes) {
            lane.dot = a.mul_add(b, lane.dot);
            lane.left = a.mul_add(a, lane.left);
            lane.right = b.mul_add(b, lane.right);
        }
    }
    lanes
        .into_iter()
        .fold(Products::default(), std::ops::Add::add)
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn vector_lanes_available() -> bool {
    std::arch::is_x86_feature_detected!("avx")
        && std::arch::is_x86_feature_detected!("f16c")
        && std::arch::is_x86_feature_detected!("fma")
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
fn vector_lanes_available() -> bool {
    false
}
