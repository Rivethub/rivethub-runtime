use kernel::memory::MemoryFallback;
use kernel::memory_core::{
    FusionItem, HYBRID_MIN_CONTENT_LEN, HYBRID_RRF_K, composite_score, fusion_score, hybrid_pool,
    literal_scan, passes_fusion_gate, recency_boost, reciprocal_rank_fusion,
    should_trigram_fallback,
};

use super::{
    Candidate, Kind, Plan, SearchMode, SearchOutcome, SqliteError, bump, fetch, hit_limit,
};
use kernel::memory::{MemoryHitSource, MemoryScope};
use rusqlite::Connection;

#[derive(Debug, Clone)]
struct ArmRow {
    candidate: Candidate,
    embedding: Option<Vec<u8>>,
    score: f64,
    boost: f64,
}

impl FusionItem for ArmRow {
    fn fusion_id(&self) -> &str {
        &self.candidate.id
    }
    fn fusion_source(&self) -> MemoryHitSource {
        self.candidate.source()
    }
    fn fusion_boost(&self) -> f64 {
        self.boost
    }
}

pub(super) fn execute(
    conn: &mut Connection,
    plan: &Plan,
    vector: Option<&[f32]>,
) -> Result<SearchOutcome, SqliteError> {
    let rows = load(conn, plan)?;
    let mut fallback = None;
    let scored = match plan.mode {
        SearchMode::Hybrid => {
            let fts = fts_rows(conn, plan, &rows, true)?;
            let vectors = vector_rows(&rows, vector, plan)?;
            let trigram = if literal_scan(&plan.text) {
                literal_rows(conn, plan, &rows)?
            } else if fts.is_empty() && should_trigram_fallback(&plan.text) {
                let recovered = keyword_rows(conn, plan, &rows, SearchMode::Trigram)?;
                if !literal_scan(&plan.text) && !recovered.is_empty() {
                    fallback = Some(MemoryFallback::Trigram);
                }
                recovered
            } else {
                Vec::new()
            };
            fused(&fts, &vectors, &trigram)
        }
        SearchMode::Vector => {
            let arm = if vector.is_some() {
                vector_rows(&rows, vector, plan)?
            } else {
                fts_rows(conn, plan, &rows, false)?
            };
            finish_arm(arm, vector.is_some())?
        }
        SearchMode::Trigram | SearchMode::Regex => {
            finish_arm(keyword_rows(conn, plan, &rows, plan.mode)?, false)?
        }
        SearchMode::Fts => finish_arm(fts_rows(conn, plan, &rows, false)?, false)?,
    };
    let mut selected = scored.clone();
    if let Some(tag) = &plan.tag {
        let index = super::TagIndex::load(conn)?;
        selected.retain(|(row, _)| row.candidate.tagged(tag, &index));
    }
    selected.truncate(hit_limit(plan.limit)?);
    let reinforced =
        if plan.mode == SearchMode::Hybrid && plan.tag.is_some() && !selected.is_empty() {
            &scored
        } else {
            &selected
        };
    let reinforced: Vec<Candidate> = reinforced
        .iter()
        .map(|(row, _)| row.candidate.clone())
        .collect();
    bump(conn, &reinforced, &crate::time::stamp(plan.now))?;
    let results = selected
        .into_iter()
        .map(|(row, score)| {
            let mut result = row.candidate.into_result();
            result.relevance_score = score;
            result
        })
        .collect();
    Ok(SearchOutcome {
        results,
        fallback,
        ..SearchOutcome::default()
    })
}

fn fused(fts: &[ArmRow], vectors: &[ArmRow], trigram: &[ArmRow]) -> Vec<(ArmRow, f64)> {
    let mut ranked: Vec<_> = reciprocal_rank_fusion(&[fts, vectors, trigram], HYBRID_RRF_K)
        .into_iter()
        .map(|row| (row.item, fusion_score(row.rrf, row.item), row.arm_count))
        .collect();
    ranked.sort_by(|a, b| {
        b.1.total_cmp(&a.1)
            .then_with(|| a.0.candidate.id.cmp(&b.0.candidate.id))
    });
    let top = ranked.first().map(|row| row.1);
    ranked
        .into_iter()
        .filter(|row| top.is_some_and(|top| passes_fusion_gate(row.2, row.1, top)))
        .map(|(row, score, _)| (row.clone(), score))
        .collect()
}

fn finish_arm(rows: Vec<ArmRow>, vector: bool) -> Result<Vec<(ArmRow, f64)>, SqliteError> {
    rows.into_iter()
        .map(|row| {
            let length = u32::try_from(row.candidate.content.trim().chars().count())
                .map_err(SqliteError::Count)?;
            let semantic = if vector {
                row.score
            } else {
                (f64::from(length) / 1000.0).min(1.0)
            };
            let score = composite_score(row.score, semantic, row.boost);
            Ok((row, score))
        })
        .collect()
}

fn sort_pool(rows: &mut Vec<ArmRow>, plan: &Plan) -> Result<(), SqliteError> {
    rows.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.candidate.id.cmp(&b.candidate.id))
    });
    let limit = u32::try_from(plan.limit).map_err(SqliteError::Count)?;
    let pool = hit_limit(hybrid_pool(limit))?;
    let mut messages = 0;
    let mut summaries = 0;
    rows.retain(|row| {
        let count = match row.candidate.source() {
            MemoryHitSource::Message => &mut messages,
            MemoryHitSource::Summary => &mut summaries,
        };
        *count += 1;
        *count <= pool
    });
    if plan.mode != SearchMode::Hybrid {
        return Ok(());
    }
    let (messages, summaries): (Vec<_>, Vec<_>) = std::mem::take(rows)
        .into_iter()
        .partition(|row| row.candidate.source() == MemoryHitSource::Message);
    let mut messages = messages.into_iter();
    for summary in summaries {
        if let Some(message) = messages.next() {
            rows.push(message);
        }
        rows.push(summary);
    }
    rows.extend(messages);
    Ok(())
}

fn fts_rows(
    conn: &Connection,
    plan: &Plan,
    rows: &[ArmRow],
    hybrid: bool,
) -> Result<Vec<ArmRow>, SqliteError> {
    let mut found = Vec::new();
    let wide = Plan {
        fetch_limit: hybrid_pool(u32::try_from(plan.limit).map_err(SqliteError::Count)?),
        ..plan.clone()
    };
    for hit in fetch(conn, &wide)? {
        if let Some(row) = rows.iter().find(|row| row.candidate.id == hit.id) {
            if hybrid && !eligible(row)? {
                continue;
            }
            let mut row = row.clone();
            row.score = -hit.rank;
            found.push(row);
        }
    }
    sort_pool(&mut found, plan)?;
    Ok(found)
}

fn vector_rows(
    rows: &[ArmRow],
    vector: Option<&[f32]>,
    plan: &Plan,
) -> Result<Vec<ArmRow>, SqliteError> {
    let Some(vector) = vector else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    for row in rows {
        if plan.mode == SearchMode::Hybrid && !eligible(row)? {
            continue;
        }
        if let Some(bytes) = &row.embedding {
            let score = dot(bytes, vector)?;
            let mut row = row.clone();
            row.score = score;
            found.push(row);
        }
    }
    sort_pool(&mut found, plan)?;
    Ok(found)
}

fn dot(bytes: &[u8], vector: &[f32]) -> Result<f64, SqliteError> {
    if bytes.len() != vector.len() * 4 {
        return Err(SqliteError::StoredEmbedding);
    }
    let mut score = 0.0;
    for (bytes, query) in bytes.as_chunks::<4>().0.iter().zip(vector) {
        let stored = f32::from_le_bytes(*bytes);
        if !stored.is_finite() {
            return Err(SqliteError::StoredEmbedding);
        }
        score = f64::from(stored).mul_add(f64::from(*query), score);
    }
    Ok(score)
}

fn keyword_rows(
    conn: &Connection,
    plan: &Plan,
    rows: &[ArmRow],
    mode: SearchMode,
) -> Result<Vec<ArmRow>, SqliteError> {
    let mut found = Vec::new();
    for (source, sql) in match mode {
        SearchMode::Regex => [
            (
                MemoryHitSource::Message,
                "SELECT id, 1.0 FROM ros_messages WHERE content REGEXP ?1 OR tool_result REGEXP ?1",
            ),
            (
                MemoryHitSource::Summary,
                "SELECT id, 1.0 FROM ros_summaries WHERE content REGEXP ?1",
            ),
        ],
        SearchMode::Hybrid | SearchMode::Fts | SearchMode::Vector | SearchMode::Trigram => [
            (
                MemoryHitSource::Message,
                "SELECT id, CASE WHEN tool_result IS NULL THEN rh_similarity(content, ?1) ELSE max(rh_similarity(content, ?1), rh_similarity(tool_result, ?1)) END AS score FROM rh_messages_trigram WHERE score > 0.3",
            ),
            (
                MemoryHitSource::Summary,
                "SELECT id, rh_similarity(content, ?1) AS score FROM rh_summaries_trigram WHERE score > 0.3",
            ),
        ],
    } {
        let mut statement = conn
            .prepare(sql)
            .map_err(SqliteError::query("keyword search"))?;
        let matches = statement
            .query_map([&plan.text], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, f64>(1)?))
            })
            .map_err(SqliteError::query("keyword search"))?;
        for hit in matches {
            let (id, score) = hit.map_err(SqliteError::query("keyword search"))?;
            if let Some(row) = rows
                .iter()
                .find(|row| row.candidate.id == id && row.candidate.source() == source)
            {
                let mut row = row.clone();
                row.score = score;
                found.push(row);
            }
        }
    }
    sort_pool(&mut found, plan)?;
    Ok(found)
}

fn literal_rows(
    conn: &Connection,
    plan: &Plan,
    rows: &[ArmRow],
) -> Result<Vec<ArmRow>, SqliteError> {
    let pattern = format!("\"{}\"", plan.text.replace('"', "\"\""));
    let mut found = Vec::new();
    for (table, tool_result) in [
        ("rh_messages_trigram", true),
        ("rh_summaries_trigram", false),
    ] {
        let short = plan.text.chars().count() < 3;
        let sql = if short {
            let tool = if tool_result {
                " OR instr(lower(tool_result), lower(?1)) > 0"
            } else {
                ""
            };
            format!("SELECT id FROM {table} WHERE instr(lower(content), lower(?1)) > 0{tool}")
        } else {
            format!("SELECT id FROM {table} WHERE {table} MATCH ?1")
        };
        let mut statement = conn
            .prepare(&sql)
            .map_err(SqliteError::query("literal search"))?;
        let ids = statement
            .query_map([if short { &plan.text } else { &pattern }], |row| {
                row.get::<_, String>(0)
            })
            .map_err(SqliteError::query("literal search"))?;
        for id in ids {
            let id = id.map_err(SqliteError::query("literal search"))?;
            if let Some(row) = rows.iter().find(|row| row.candidate.id == id) {
                found.push(row.clone());
            }
        }
    }
    sort_pool(&mut found, plan)?;
    Ok(found)
}

fn load(conn: &Connection, plan: &Plan) -> Result<Vec<ArmRow>, SqliteError> {
    let mut rows = Vec::new();
    if matches!(plan.scope, MemoryScope::Both | MemoryScope::Messages) {
        rows.extend(load_table(conn, plan, true)?);
    }
    if matches!(plan.scope, MemoryScope::Both | MemoryScope::Summaries) {
        rows.extend(load_table(conn, plan, false)?);
    }
    if plan.mode != SearchMode::Hybrid
        && let Some(tag) = &plan.tag
    {
        let index = super::TagIndex::load(conn)?;
        rows.retain(|row| row.candidate.tagged(tag, &index));
    }
    Ok(rows)
}

const MESSAGE_CANDIDATES: &str = "SELECT id, content, created_at, conversation_id, role, agent, embedding, access_count,
         CASE WHEN role = 'system' THEN 0.5 WHEN tool_name IS NOT NULL THEN 0.3 WHEN role = 'user' THEN 0.7 ELSE 0.6 END,
         (SELECT c.session_key FROM ros_conversations c WHERE c.id = m.conversation_id)
         FROM ros_messages m WHERE (?1 IS NULL OR agent = ?1) AND (?2 IS NULL OR created_at >= ?2) AND (?3 IS NULL OR created_at < ?3)
         AND (?4 IS NULL OR role = ?4) AND (?5 IS NULL OR tool_name = ?5)
         AND (?6 IS NULL OR role NOT IN (SELECT value FROM json_each(?6)))
         AND NOT EXISTS (SELECT 1 FROM rh_session_trash tr JOIN ros_conversations tc ON tc.session_key = tr.session_key WHERE tc.id = m.conversation_id)";

const SUMMARY_CANDIDATES: &str = "SELECT id, content, created_at, conversation_id, NULL, NULL, embedding, access_count, 0.6,
         (SELECT c.session_key FROM ros_conversations c WHERE c.id = s.conversation_id)
         FROM ros_summaries s WHERE (?1 IS NULL OR EXISTS (SELECT 1 FROM ros_conversations c WHERE c.id = s.conversation_id AND c.agent = ?1))
         AND (?2 IS NULL OR created_at >= ?2) AND (?3 IS NULL OR created_at < ?3)
         AND NOT EXISTS (SELECT 1 FROM rh_session_trash tr JOIN ros_conversations tc ON tc.session_key = tr.session_key WHERE tc.id = s.conversation_id)";

fn load_table(conn: &Connection, plan: &Plan, message: bool) -> Result<Vec<ArmRow>, SqliteError> {
    let sql = if message {
        MESSAGE_CANDIDATES
    } else {
        SUMMARY_CANDIDATES
    };
    let mut statement = conn
        .prepare(sql)
        .map_err(SqliteError::query("search candidates"))?;
    let role = plan.role.map(|role| role.to_string());
    let filters: Vec<&dyn rusqlite::ToSql> = if message {
        vec![
            &plan.agent,
            &plan.since,
            &plan.before,
            &role,
            &plan.tool_name,
            &plan.exclude_roles,
        ]
    } else {
        vec![&plan.agent, &plan.since, &plan.before]
    };
    let mapped = statement
        .query_map(filters.as_slice(), |row| {
            let kind = if message {
                Kind::Message {
                    role: crate::message::stored_role(row, 4)?,
                    agent: row.get(5)?,
                    conversation_id: row.get(3)?,
                    session_id: row.get(9)?,
                }
            } else {
                Kind::Summary {
                    conversation_id: row.get(3)?,
                    session_id: row.get(9)?,
                }
            };
            Ok((
                row.get::<_, String>(2)?,
                row.get::<_, u32>(7)?,
                row.get::<_, f64>(8)?,
                ArmRow {
                    candidate: Candidate {
                        id: row.get(0)?,
                        content: row.get(1)?,
                        created_at: plan.now,
                        rank: 0.0,
                        kind,
                    },
                    embedding: row.get(6)?,
                    score: 0.0,
                    boost: 0.0,
                },
            ))
        })
        .map_err(SqliteError::query("search candidates"))?;
    mapped
        .map(|result| {
            let (stamp, access, importance, mut row) =
                result.map_err(SqliteError::query("search candidates"))?;
            row.candidate.created_at = crate::time::stored(&stamp, "created_at")?;
            let duration = plan.now - row.candidate.created_at;
            let seconds = duration
                .abs()
                .to_std()
                .map_err(|_| SqliteError::Clock)?
                .as_secs_f64();
            let age = if duration < chrono::TimeDelta::zero() {
                -seconds
            } else {
                seconds
            } / 86400.0;
            row.boost = recency_boost(age, access, importance);
            Ok(row)
        })
        .collect()
}

fn eligible(row: &ArmRow) -> Result<bool, SqliteError> {
    let minimum = usize::try_from(HYBRID_MIN_CONTENT_LEN).map_err(SqliteError::Count)?;
    Ok(matches!(
        row.candidate.kind,
        Kind::Message {
            role: wire::message::Role::Tool,
            ..
        }
    ) || row.candidate.content.trim().chars().count() >= minimum)
}
