use std::collections::BTreeSet;

use kernel::memory::summary_backend::SummaryIndexes;
use kernel::memory::summary_reference::{PreparedSummaryRecord, PreparedSummaryTree};
use rusqlite::{Connection, params};
use uuid::Uuid;

use crate::{SqliteError, summary_index, time};

use super::publish::failure;

pub(super) fn snapshot(conn: &Connection, conversation: Uuid) -> Result<(), SqliteError> {
    let sql = format!(
        "CREATE TEMP TABLE summary_old_passages AS
         SELECT p.*, s.kind, s.content AS owner_content, NOT s.index_needs_repair AND complete.id IS NOT NULL AS complete,
                ({MEMBERS}) AS members
         FROM ros_summary_passages p JOIN ros_summaries s ON s.id = p.record_id
         LEFT JOIN ros_summary_records_indexed complete ON complete.id = p.record_id
         WHERE s.conversation_id = ?1 AND p.embedding IS NOT NULL
           AND p.content = substr(s.content, p.char_start + 1, p.char_end - p.char_start)"
    );
    conn.execute(&sql, [conversation.to_string()])
        .map_err(failure)?;
    Ok(())
}

const MEMBERS: &str = "
    SELECT json_group_array(message_id) FROM (
        SELECT l.message_id FROM ros_summary_sources l WHERE l.summary_id = p.record_id
        AND (NOT EXISTS (
                  SELECT 1 FROM ros_summaries section WHERE section.parent_id = p.record_id
                  AND section.kind = 'section'
                  AND section.section_char_start < p.char_end AND p.char_start < section.section_char_end
             ) OR EXISTS (
                  SELECT 1 FROM ros_summaries section WHERE section.parent_id = p.record_id
                  AND section.kind = 'section'
                  AND section.section_char_start < p.char_end AND p.char_start < section.section_char_end
                  AND l.ordinal BETWEEN section.source_first AND section.source_last
             ))
        ORDER BY l.ordinal
    )";

pub(super) fn publish(
    conn: &Connection,
    tree: &PreparedSummaryTree,
    retained: &BTreeSet<Uuid>,
    indexes: Option<&SummaryIndexes>,
) -> Result<(), SqliteError> {
    for record in tree
        .records
        .iter()
        .filter(|record| !retained.contains(&record.id))
    {
        let Some(key) = record.index_key else {
            continue;
        };
        if record.kind.depth() == 0 {
            continue;
        }
        let index = indexes
            .map(|indexes| {
                indexes.get(&record.id).ok_or_else(|| SqliteError::Invalid {
                    message: format!("missing prepared index for summary {}", record.id),
                })
            })
            .transpose()?;
        let windows = tree.passage_windows.get(&record.id);
        if let Some(windows) = windows.filter(|windows| !windows.is_empty()) {
            for (ordinal, window) in windows.iter().enumerate() {
                draft(
                    conn,
                    record,
                    key,
                    i32::try_from(ordinal).map_err(SqliteError::Count)?,
                    window,
                )?;
            }
        } else {
            reuse_complete(conn, record, key)?;
        }
        if let Some(index) = index {
            summary_index::validate(index)?;
            for (ordinal, piece) in index.passages.iter().enumerate() {
                let updated = conn.execute(
                    "UPDATE ros_summary_passages SET embedding = ?1, earliest_at = ?2, latest_at = ?3
                     WHERE record_id = ?4 AND ordinal = ?5 AND content = ?6 AND char_start = ?7 AND char_end = ?8",
                    params![crate::vector::pack(&piece.vector), time::stamp(piece.earliest_at),
                        time::stamp(piece.latest_at), record.id.to_string(), i32::try_from(ordinal).map_err(SqliteError::Count)?, piece.content,
                        piece.chars.start, piece.chars.end],
                ).map_err(failure)?;
                if updated != 1 {
                    return Err(SqliteError::Invalid {
                        message: "missing prepared summary passage".into(),
                    });
                }
            }
        }
        summary_index::finish(conn, record.id, index.is_some())?;
    }
    conn.execute_batch("DROP TABLE summary_old_passages")
        .map_err(failure)
}

fn reuse_complete(
    conn: &Connection,
    record: &PreparedSummaryRecord,
    key: [u8; 32],
) -> Result<(), SqliteError> {
    let mut statement = conn.prepare(
        "SELECT ordinal, char_start, char_end FROM summary_old_passages
         WHERE record_id = (
            SELECT record_id FROM summary_old_passages WHERE complete AND owner_content = ?1 AND index_key = ?2
            ORDER BY (kind = 'session' AND ?3 = 'session') DESC, record_id LIMIT 1
         ) ORDER BY ordinal",
    ).map_err(failure)?;
    let rows = statement
        .query_map(
            params![record.content, key.as_slice(), record.kind.as_str()],
            |row| Ok((row.get::<_, i32>(0)?, row.get(1)?..row.get(2)?)),
        )
        .map_err(failure)?;
    for row in rows {
        let (ordinal, window) = row.map_err(failure)?;
        draft(conn, record, key, ordinal, &window)?;
    }
    Ok(())
}

fn draft(
    conn: &Connection,
    record: &PreparedSummaryRecord,
    key: [u8; 32],
    ordinal: i32,
    window: &std::ops::Range<i32>,
) -> Result<(), SqliteError> {
    let sql = format!(
        "WITH passage AS (
             SELECT s.id AS record_id, s.kind, s.earliest_at, s.latest_at,
                    ?2 AS ordinal, ?3 AS char_start, ?4 AS char_end, ?5 AS index_key,
                    substr(s.content, ?3 + 1, ?4 - ?3) AS content
             FROM ros_summaries s WHERE s.id = ?1
         ), draft AS (
             SELECT p.*, ({MEMBERS}) AS members FROM passage p
         ), matches AS (
             SELECT old.id, old.embedding, draft.kind = 'session' AND old.kind = 'session'
                       AND old.ordinal = draft.ordinal AND old.members = draft.members AS same_slot
             FROM summary_old_passages old JOIN draft ON old.index_key = draft.index_key AND old.content = draft.content
             ORDER BY same_slot DESC, old.id LIMIT 1
         )
         INSERT INTO ros_summary_passages
             (id, record_id, ordinal, char_start, char_end, content, earliest_at, latest_at, index_key, embedding)
         SELECT CASE WHEN matches.same_slot THEN matches.id ELSE ?6 END,
                d.record_id, d.ordinal, d.char_start, d.char_end, d.content,
                coalesce((SELECT m.created_at FROM ros_messages m JOIN json_each(d.members) ids
                          ON ids.value = m.id ORDER BY rh_instant(m.created_at) LIMIT 1), d.earliest_at),
                coalesce((SELECT m.created_at FROM ros_messages m JOIN json_each(d.members) ids
                          ON ids.value = m.id ORDER BY rh_instant(m.created_at) DESC LIMIT 1), d.latest_at),
                d.index_key, matches.embedding
         FROM draft d LEFT JOIN matches ON true"
    );
    conn.execute(
        &sql,
        params![
            record.id.to_string(),
            ordinal,
            window.start,
            window.end,
            key.as_slice(),
            Uuid::new_v4().to_string()
        ],
    )
    .map_err(failure)?;
    Ok(())
}
