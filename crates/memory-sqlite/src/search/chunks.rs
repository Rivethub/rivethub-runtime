use std::cmp::Ordering;
use std::collections::BTreeMap;

use kernel::memory::MemoryChunkMatch;
use kernel::memory::MemoryScope;
use rusqlite::Connection;

use super::Kind;
use super::Plan;
use super::SqliteError;
use super::ranked::ArmRow;
use super::ranked::cosine;

struct Span {
    index: u32,
    total: u32,
    char_start: u32,
    char_end: u32,
    content: String,
    embedding: Vec<u8>,
}

pub(super) fn merge(
    conn: &Connection,
    rows: &[ArmRow],
    found: &mut Vec<ArmRow>,
    vector: &[f32],
    plan: &Plan,
) -> Result<(), SqliteError> {
    if !matches!(plan.scope, MemoryScope::Messages | MemoryScope::Both) {
        return Ok(());
    }
    let pool = usize::try_from(kernel::memory_core::chunk_pool(
        u32::try_from(plan.limit).map_err(SqliteError::Count)?,
    ))
    .map_err(SqliteError::Count)?;
    let best = best_spans(conn, vector, plan, pool)?;
    for (message_id, chosen) in best {
        let Some(parent) = message_parent(rows, &message_id) else {
            continue;
        };
        apply(found, parent, &chosen, vector)?;
    }
    Ok(())
}

fn message_parent<'a>(rows: &'a [ArmRow], message_id: &str) -> Option<&'a ArmRow> {
    rows.iter().find(|row| {
        row.candidate.id == message_id && matches!(row.candidate.kind, Kind::Message { .. })
    })
}

fn apply(
    found: &mut Vec<ArmRow>,
    parent: &ArmRow,
    chosen: &Chosen,
    vector: &[f32],
) -> Result<(), SqliteError> {
    if let Some(row) = found.iter_mut().find(|row| {
        row.candidate.id == parent.candidate.id
            && matches!(row.candidate.kind, Kind::Message { .. })
    }) {
        if row.score.total_cmp(&chosen.score).is_le() {
            row.score = chosen.score;
            row.candidate.chunk = Some(chosen.chunk.clone());
        }
        return Ok(());
    }
    let (score, chunk) = kept_score(parent, chosen, vector)?;
    let mut row = parent.clone();
    row.score = score;
    row.candidate.chunk = chunk;
    found.push(row);
    Ok(())
}

fn kept_score(
    parent: &ArmRow,
    chosen: &Chosen,
    vector: &[f32],
) -> Result<(f64, Option<MemoryChunkMatch>), SqliteError> {
    let Some(bytes) = &parent.embedding else {
        return Ok((chosen.score, Some(chosen.chunk.clone())));
    };
    let parent_score = cosine(bytes, vector)?;
    if parent_score.total_cmp(&chosen.score).is_gt() {
        return Ok((parent_score, None));
    }
    Ok((chosen.score, Some(chosen.chunk.clone())))
}

struct Chosen {
    score: f64,
    index: u32,
    chunk: MemoryChunkMatch,
}

const CHUNK_CANDIDATES: &str = "SELECT c.message_id, c.idx, c.char_start, c.char_end, c.content, c.embedding, \
        (SELECT count(*) FROM rh_message_chunks all_chunks \
          WHERE all_chunks.message_id = c.message_id) \
   FROM rh_message_chunks c \
   JOIN ros_messages m ON m.id = c.message_id \
  WHERE c.embedding IS NOT NULL \
    AND (?1 IS NULL OR m.agent = ?1) \
    AND (?2 IS NULL OR rh_instant(m.created_at) >= ?2) \
    AND (?3 IS NULL OR rh_instant(m.created_at) < ?3) \
    AND (?4 IS NULL OR m.role = ?4) \
    AND (?5 IS NULL OR m.tool_name = ?5) \
    AND (?6 IS NULL OR m.role NOT IN (SELECT value FROM json_each(?6))) \
    AND NOT EXISTS (SELECT 1 FROM rh_session_trash tr \
          JOIN ros_conversations tc ON tc.session_key = tr.session_key \
         WHERE tc.id = m.conversation_id) \
    AND (?7 IS NULL OR m.conversation_id IN ( \
          SELECT entity_id FROM ros_tags \
           WHERE key = ?8 AND value = ?9 AND state = 'accepted' \
             AND entity_type = 'conversation'))";

fn best_spans(
    conn: &Connection,
    vector: &[f32],
    plan: &Plan,
    pool: usize,
) -> Result<BTreeMap<String, Chosen>, SqliteError> {
    let mut statement = conn
        .prepare(CHUNK_CANDIDATES)
        .map_err(SqliteError::query("chunk search"))?;
    let filter = ChunkFilter::from_plan(plan);
    let role = plan.role.map(|role| role.to_string());
    let params: Vec<&dyn rusqlite::ToSql> = vec![
        &plan.agent,
        &plan.since,
        &plan.before,
        &role,
        &plan.tool_name,
        &plan.exclude_roles,
        &filter.present,
        &filter.key,
        &filter.value,
    ];
    let rows = statement
        .query_map(params.as_slice(), read_span)
        .map_err(SqliteError::query("chunk search"))?;
    let mut nearest = Nearest {
        pool,
        rows: Vec::new(),
    };
    for row in rows {
        let (message_id, span) = row.map_err(SqliteError::query("chunk search"))?;
        nearest.consider(near_of(message_id, span, vector)?);
    }
    Ok(nearest.into_best())
}

struct ChunkFilter {
    present: Option<i64>,
    key: String,
    value: String,
}

impl ChunkFilter {
    fn from_plan(plan: &Plan) -> Self {
        match &plan.tag {
            Some(tag) => Self {
                present: Some(1_i64),
                key: tag.key.clone(),
                value: tag.value.clone(),
            },
            None => Self {
                present: None,
                key: String::new(),
                value: String::new(),
            },
        }
    }
}

struct Near {
    message_id: String,
    score: f64,
    index: u32,
    chunk: MemoryChunkMatch,
}

struct Nearest {
    pool: usize,
    rows: Vec<Near>,
}

impl Nearest {
    fn consider(&mut self, near: Near) {
        if self.rows.len() < self.pool {
            self.rows.push(near);
            return;
        }
        let Some(at) = worst_index(&self.rows) else {
            return;
        };
        let Some(slot) = self.rows.get_mut(at) else {
            return;
        };
        if worse(slot, &near).is_gt() {
            *slot = near;
        }
    }

    fn into_best(self) -> BTreeMap<String, Chosen> {
        let mut best = BTreeMap::new();
        for near in self.rows {
            let chosen = Chosen {
                score: near.score,
                index: near.index,
                chunk: near.chunk,
            };
            let replace = best
                .get(&near.message_id)
                .is_none_or(|current| outranks(&chosen, current));
            if replace {
                best.insert(near.message_id, chosen);
            }
        }
        best
    }
}

fn near_of(message_id: String, span: Span, vector: &[f32]) -> Result<Near, SqliteError> {
    Ok(Near {
        score: cosine(&span.embedding, vector)?,
        index: span.index,
        chunk: MemoryChunkMatch {
            index: span.index,
            total: span.total,
            char_start: span.char_start,
            char_end: span.char_end,
            content: span.content,
        },
        message_id,
    })
}

fn worst_index(rows: &[Near]) -> Option<usize> {
    rows.iter()
        .enumerate()
        .max_by(|(_, left), (_, right)| worse(left, right))
        .map(|(index, _)| index)
}

fn worse(left: &Near, right: &Near) -> Ordering {
    right
        .score
        .total_cmp(&left.score)
        .then_with(|| left.index.cmp(&right.index))
        .then_with(|| left.message_id.cmp(&right.message_id))
}

fn outranks(next: &Chosen, current: &Chosen) -> bool {
    match next.score.total_cmp(&current.score) {
        Ordering::Greater => true,
        Ordering::Equal => next.index < current.index,
        Ordering::Less => false,
    }
}

fn read_span(row: &rusqlite::Row<'_>) -> rusqlite::Result<(String, Span)> {
    Ok((
        row.get(0)?,
        Span {
            index: fit(row.get(1)?)?,
            total: fit(row.get(6)?)?,
            char_start: fit(row.get(2)?)?,
            char_end: fit(row.get(3)?)?,
            content: row.get(4)?,
            embedding: row.get(5)?,
        },
    ))
}

fn fit(value: i64) -> rusqlite::Result<u32> {
    u32::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}
