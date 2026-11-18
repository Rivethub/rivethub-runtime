use kernel::memory::{
    summary_backend::{SummaryIndex, SummaryRepair},
    summary_reference::SummarySectionBounds,
};
use rusqlite::{Connection, OptionalExtension as _, params};
use uuid::Uuid;

use crate::{
    SqliteError,
    summary_backend::error,
    summary_replace::{immediate, publish::stored_uuid},
    time,
};

pub(crate) fn marked_indexes(
    conn: &Connection,
    key: [u8; 32],
    limit: u32,
) -> Result<Vec<Uuid>, SqliteError> {
    let mut statement = conn.prepare(
        "SELECT s.id FROM ros_summaries s WHERE kind IN ('session', 'part')
           AND (index_needs_repair OR embedding IS NULL OR index_key IS NOT ?1)
           AND NOT EXISTS (SELECT 1 FROM ros_jobs j WHERE j.job_key = 'embed-ros_summaries-' || s.id)
         ORDER BY s.id LIMIT ?2"
    ).map_err(error)?;
    let ids = statement
        .query_map(params![key.as_slice(), limit], |row| {
            row.get::<_, String>(0)
        })
        .map_err(error)?;
    ids.map(|id| stored_uuid(id.map_err(error)?)).collect()
}

pub(crate) fn marked_index(
    conn: &mut Connection,
    id: Uuid,
    key: [u8; 32],
) -> Result<Option<SummaryRepair>, SqliteError> {
    let tx = conn.transaction().map_err(error)?;
    let row = tx.query_row(
        "SELECT content, input_key FROM ros_summaries WHERE id = ?1 AND kind IN ('session', 'part')
           AND (index_needs_repair OR embedding IS NULL OR index_key IS NOT ?2)",
        params![id.to_string(), key.as_slice()], |row| Ok((row.get(0)?, row.get(1)?))
    ).optional().map_err(error)?;
    let Some((content, input_key)) = row else {
        return Ok(None);
    };
    let rows = tx.prepare(
        "SELECT l.ordinal, m.created_at FROM ros_summary_sources l JOIN ros_messages m ON m.id = l.message_id
         WHERE l.summary_id = ?1 ORDER BY l.ordinal"
    ).and_then(|mut statement| statement.query_map([id.to_string()], |row| Ok((row.get(0)?, row.get::<_, String>(1)?)))?
        .collect::<Result<Vec<_>, _>>()).map_err(error)?;
    let sources = rows
        .into_iter()
        .map(|(ordinal, at)| Ok((ordinal, time::stored(&at, "summary source time")?)))
        .collect::<Result<_, SqliteError>>()?;
    let sections = tx
        .prepare(
            "SELECT source_first, source_last, range_verified, section_char_start, section_char_end
         FROM ros_summaries WHERE parent_id = ?1 AND kind = 'section' ORDER BY summary_ordinal",
        )
        .and_then(|mut statement| {
            statement
                .query_map([id.to_string()], |row| {
                    Ok(SummarySectionBounds {
                        first: row.get(0)?,
                        last: row.get(1)?,
                        verified: row.get(2)?,
                        char_start: row.get(3)?,
                        char_end: row.get(4)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(error)?;
    tx.commit().map_err(error)?;
    Ok(Some(SummaryRepair {
        id,
        content,
        input_key,
        sources,
        sections,
    }))
}

pub(crate) fn replace_index(
    conn: &mut Connection,
    key: [u8; 32],
    expected: SummaryRepair,
    index: SummaryIndex,
) -> Result<(), SqliteError> {
    let tx = immediate(conn).map_err(error)?;
    tx.execute(
        "UPDATE ros_summaries SET embedding = ?1, index_key = ?2, index_needs_repair = 0
         WHERE id = ?3 AND input_key = ?4 AND content = ?5 AND kind IN ('session', 'part')
           AND (index_needs_repair OR embedding IS NULL OR index_key IS NOT ?2)",
        params![
            crate::vector::pack(&index.pooled),
            key.as_slice(),
            expected.id.to_string(),
            expected.input_key,
            expected.content
        ],
    )
    .map_err(error)?;
    tx.commit().map_err(error)
}
