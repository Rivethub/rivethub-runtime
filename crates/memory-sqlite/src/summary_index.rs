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
           AND (index_needs_repair OR NOT EXISTS (SELECT 1 FROM ros_summary_records_indexed WHERE id = s.id) OR index_key IS NOT ?1)
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
        "SELECT content, input_key FROM ros_summaries s WHERE s.id = ?1 AND kind IN ('session', 'part')
           AND (index_needs_repair OR NOT EXISTS (SELECT 1 FROM ros_summary_records_indexed WHERE id = s.id) OR index_key IS NOT ?2)",
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
    expected: &SummaryRepair,
    index: &SummaryIndex,
) -> Result<(), SqliteError> {
    validate(index)?;
    let tx = immediate(conn).map_err(error)?;
    let updated = tx
        .execute(
            "UPDATE ros_summaries AS s SET index_key = ?1
         WHERE id = ?2 AND input_key = ?3 AND content = ?4 AND kind IN ('session', 'part')
           AND (index_needs_repair OR index_key IS NOT ?1
               OR NOT EXISTS (SELECT 1 FROM ros_summary_records_indexed WHERE id = s.id))",
            params![
                key.as_slice(),
                expected.id.to_string(),
                expected.input_key,
                expected.content
            ],
        )
        .map_err(error)?;
    if updated == 0 {
        return Ok(());
    }
    tx.execute(
        "DELETE FROM ros_summary_passages WHERE record_id = ?1",
        [expected.id.to_string()],
    )
    .map_err(error)?;
    insert(&tx, key, expected.id, index)?;
    finish(&tx, expected.id, true)?;
    tx.commit().map_err(error)
}

pub(crate) fn validate(index: &SummaryIndex) -> Result<(), SqliteError> {
    index
        .validate_width()
        .map_err(|error| SqliteError::Invalid {
            message: error.to_string(),
        })?;
    if index
        .passages
        .iter()
        .any(|piece| piece.latest_at < piece.earliest_at)
    {
        return Err(SqliteError::Invalid {
            message: "summary passage source time is inverted".into(),
        });
    }
    if index
        .passages
        .iter()
        .flat_map(|piece| &piece.vector)
        .any(|value| !half::f16::from_f32(*value).is_finite())
    {
        return Err(SqliteError::Invalid {
            message: "summary passage vector exceeds half precision range".into(),
        });
    }
    Ok(())
}

fn insert(
    conn: &Connection,
    key: [u8; 32],
    id: Uuid,
    index: &SummaryIndex,
) -> Result<(), SqliteError> {
    let id = id.to_string();
    let mut statement = conn.prepare(
        "INSERT INTO ros_summary_passages (id, record_id, ordinal, char_start, char_end, content,
            earliest_at, latest_at, index_key, embedding) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
    ).map_err(error)?;
    for (ordinal, piece) in index.passages.iter().enumerate() {
        statement
            .execute(params![
                Uuid::new_v4().to_string(),
                id,
                i32::try_from(ordinal).map_err(SqliteError::Count)?,
                piece.chars.start,
                piece.chars.end,
                piece.content,
                time::stamp(piece.earliest_at),
                time::stamp(piece.latest_at),
                key.as_slice(),
                crate::vector::pack(&piece.vector)
            ])
            .map_err(error)?;
    }
    Ok(())
}

pub(crate) fn finish(conn: &Connection, id: Uuid, required: bool) -> Result<(), SqliteError> {
    let updated = conn
        .execute(
            "UPDATE ros_summaries SET index_needs_repair = 0
         WHERE id = ?1 AND id IN (SELECT id FROM ros_summary_records_indexed WHERE id = ?1)",
            [id.to_string()],
        )
        .map_err(error)?;
    if required && updated != 1 {
        return Err(SqliteError::Invalid {
            message: format!("incomplete index for summary {id}"),
        });
    }
    Ok(())
}
