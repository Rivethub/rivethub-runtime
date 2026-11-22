use half::f16;
use kernel::memory::{MemoryScope, SummaryPassage};
use kernel::memory_core::{HYBRID_MIN_CONTENT_LEN, recency_boost};
use rusqlite::{Connection, Row, params};

use super::{Candidate, Kind, Plan, SqliteError, TagIndex, ranked::ArmRow};
use crate::time;

const OPERATION: &str = "summary passage search";

const SELECT: &str = "SELECT p.id, p.record_id, p.ordinal, p.content, p.latest_at,
            c.session_key, c.channel, s.access_count";

const SCOPE: &str = "JOIN ros_summaries s ON s.id = p.record_id
         JOIN ros_conversations c ON c.id = s.conversation_id
         WHERE s.kind IN ('session', 'part') AND NOT s.index_needs_repair
           AND (?1 IS NULL OR c.agent = ?1)
           AND (?2 IS NULL OR rh_instant(p.latest_at) >= ?2)
           AND (?3 IS NULL OR rh_instant(p.latest_at) < ?3)
           AND NOT EXISTS (SELECT 1 FROM rh_session_trash tr WHERE tr.session_key = c.session_key)";

#[derive(Clone, Copy)]
pub(super) enum Match {
    Fts { hybrid: bool },
    Trigram,
    Regex,
}

pub(super) fn lexical(
    conn: &Connection,
    plan: &Plan,
    matching: Match,
) -> Result<Vec<ArmRow>, SqliteError> {
    let (score, join, filter, term) = match matching {
        Match::Fts { hybrid } => {
            let Some(pattern) = super::fts::fts_and(&plan.text) else {
                return Ok(Vec::new());
            };
            (
                "-ros_summary_passages_fts.rank",
                "JOIN ros_summary_passages_fts ON ros_summary_passages_fts.rowid = p.rowid",
                "AND ros_summary_passages_fts MATCH ?4 AND length(trim(p.content)) >= ?5",
                (pattern, if hybrid { HYBRID_MIN_CONTENT_LEN } else { 0 }),
            )
        }
        Match::Trigram => (
            "(SELECT avg(instr(rh_literal_lower(p.content), token.value) > 0)
              FROM json_each(?4) AS token)",
            "",
            "AND passage_score > 0",
            (trigram_tokens(plan)?, 0),
        ),
        Match::Regex => ("1.0", "", "AND p.content REGEXP ?4", (plan.text.clone(), 0)),
    };
    if term.0.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "{SELECT}, {score} AS passage_score FROM ros_summary_passages p {join} {SCOPE} {filter}"
    );
    let mut parameters: Vec<&dyn rusqlite::ToSql> =
        vec![&plan.agent, &plan.since, &plan.before, &term.0];
    if matches!(matching, Match::Fts { .. }) {
        parameters.push(&term.1);
    }
    load(conn, plan, &sql, parameters.as_slice(), |row| {
        row.get(8).map_err(SqliteError::query(OPERATION))
    })
}

fn trigram_tokens(plan: &Plan) -> Result<String, SqliteError> {
    let tokens =
        kernel::memory_core::literal_tokens(&plan.text, plan.mode != super::SearchMode::Trigram);
    if tokens.is_empty() {
        return Ok(String::new());
    }
    let folded: Vec<_> = tokens
        .iter()
        .map(|token| super::functions::literal_lower(token))
        .collect();
    serde_json::to_string(&folded).map_err(|source| SqliteError::Encode {
        column: "passage search tokens",
        source,
    })
}

pub(super) fn vectors(
    conn: &Connection,
    plan: &Plan,
    query: &[f32],
) -> Result<Vec<ArmRow>, SqliteError> {
    let sql = format!(
        "{SELECT}, p.embedding FROM ros_summary_passages p {SCOPE}
          AND p.embedding IS NOT NULL AND p.index_key = s.index_key
          AND (?4 IS NULL OR s.index_key = ?4)"
    );
    let key = plan.summary_index_key.as_ref().map(<[_; 32]>::as_slice);
    let query: Vec<_> = query.iter().map(|value| half_precision(*value)).collect();
    let norm = query.iter().map(|value| value * value).sum::<f64>();
    load(
        conn,
        plan,
        &sql,
        params![plan.agent, plan.since, plan.before, key],
        |row| {
            let vector = row.get_ref(8).map_err(SqliteError::query(OPERATION))?;
            let bytes = vector.as_blob().map_err(|_| SqliteError::StoredEmbedding)?;
            cosine(bytes, &query, norm)
        },
    )
}

fn half_precision(value: f32) -> f64 {
    f64::from(f16::from_f32(value).to_f32())
}

fn cosine(bytes: &[u8], query: &[f64], query_norm: f64) -> Result<f64, SqliteError> {
    let (packed, remainder) = bytes.as_chunks::<4>();
    if !remainder.is_empty() || packed.len() != query.len() || !query_norm.is_finite() {
        return Err(SqliteError::StoredEmbedding);
    }
    let (mut dot, mut norm) = (0.0, 0.0);
    for (value, target) in packed.iter().zip(query) {
        let value = half_precision(f32::from_le_bytes(*value));
        if !value.is_finite() {
            return Err(SqliteError::StoredEmbedding);
        }
        dot = value.mul_add(*target, dot);
        norm = value.mul_add(value, norm);
    }
    if norm == 0.0 || query_norm == 0.0 {
        return Ok(0.0);
    }
    Ok((dot / (norm * query_norm).sqrt()).clamp(-1.0, 1.0))
}

fn load(
    conn: &Connection,
    plan: &Plan,
    sql: &str,
    parameters: impl rusqlite::Params,
    score: impl Fn(&Row<'_>) -> Result<f64, SqliteError>,
) -> Result<Vec<ArmRow>, SqliteError> {
    if plan.scope == MemoryScope::Messages {
        return Ok(Vec::new());
    }
    let tags = plan
        .tag
        .as_ref()
        .map(|_| TagIndex::load(conn))
        .transpose()?;
    let mut statement = conn.prepare(sql).map_err(SqliteError::query(OPERATION))?;
    let mut rows = statement
        .query(parameters)
        .map_err(SqliteError::query(OPERATION))?;
    let mut found = Vec::new();
    while let Some(row) = rows.next().map_err(SqliteError::query(OPERATION))? {
        let (stamp, access, mut candidate) =
            candidate(row, plan).map_err(SqliteError::query(OPERATION))?;
        if let Some((tag, tags)) = plan.tag.as_ref().zip(tags.as_ref())
            && !candidate.tagged(tag, tags)
        {
            continue;
        }
        let score = score(row)?;
        candidate.rank = -score;
        candidate.created_at = time::stored(&stamp, "passage source time")?;
        let days = (plan.now - candidate.created_at).as_secs_f64() / 86_400.0;
        found.push(ArmRow {
            candidate,
            score,
            boost: recency_boost(days, access, 0.6),
            embedding: None,
        });
    }
    Ok(found)
}

fn candidate(row: &Row<'_>, plan: &Plan) -> rusqlite::Result<(String, u32, Candidate)> {
    let channel: String = row.get(6)?;
    let key: String = row.get(5)?;
    Ok((
        row.get(4)?,
        row.get(7)?,
        Candidate {
            passage: Some(SummaryPassage {
                id: row.get(0)?,
                ordinal: row.get(2)?,
            }),
            id: row.get(1)?,
            content: row.get(3)?,
            created_at: plan.now,
            rank: 0.0,
            kind: Kind::Summary {
                session_id: Some(kernel::filing::session_key(Some(&channel), &key)),
            },
        },
    ))
}
