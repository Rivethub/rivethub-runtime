use jiff::Timestamp;
use kernel::wiki::{builder::WIKI_PIPELINE_VERSION, entity_pass::ENTITY_PIPELINE_VERSION};
use rusqlite::{Connection, params};

use crate::{SqliteError, time};

use super::error;

pub(super) fn deliveries(conn: &Connection, conversation: &str) -> Result<usize, SqliteError> {
    anchor_deliveries(conn, conversation)?;
    conn.execute(
        COMPLETE_EMPTY_DELIVERIES,
        params![conversation, WIKI_PIPELINE_VERSION, ENTITY_PIPELINE_VERSION],
    )
    .map_err(error)
}

fn anchor_deliveries(conn: &Connection, conversation: &str) -> Result<(), SqliteError> {
    use crate::message::{stored_role, transcript_metadata};
    use kernel::memory::MemoryBrowseOrder;
    use kernel::memory::summary_order::{MessagePosition, order_messages};
    use wire::{JsonObject, message::Role};

    struct Input {
        id: String,
        role: Role,
        metadata: Option<JsonObject>,
        created_at: Timestamp,
        source_anchor: Option<String>,
        fallback_anchor: String,
        has_payload: bool,
    }

    let mut query = conn.prepare(RELEASE_INPUTS).map_err(error)?;
    let inputs = query
        .query_map([conversation], |row| {
            Ok((
                row.get(0)?,
                stored_role(row, 1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })
        .map_err(error)?
        .map(|row| {
            let (id, role, metadata, created_at, source_anchor, fallback_anchor, has_payload) =
                row.map_err(error)?;
            Ok(Input {
                id,
                role,
                metadata: transcript_metadata(&metadata),
                created_at: time::stored(&created_at, "created_at")?,
                source_anchor,
                fallback_anchor,
                has_payload,
            })
        })
        .collect::<Result<Vec<_>, SqliteError>>()?;
    let ordered = order_messages(inputs, MemoryBrowseOrder::Asc, |input| {
        MessagePosition::new(input.created_at, input.role, input.metadata.as_ref())
    });
    let mut update = conn
        .prepare(
            "UPDATE ros_summary_deliveries SET anchor_id = ?3, historical_backlog = 0
         WHERE conversation_id = ?1 AND message_id = ?2 AND historical_backlog",
        )
        .map_err(error)?;
    let mut preceding = None;
    for input in &ordered {
        preceding = input.source_anchor.as_deref().or(preceding);
        if input.source_anchor.is_some() || input.has_payload {
            update
                .execute(params![
                    conversation,
                    input.id,
                    preceding.unwrap_or(&input.fallback_anchor)
                ])
                .map_err(error)?;
        }
    }
    Ok(())
}

const RELEASE_INPUTS: &str = "WITH part_order AS (
    SELECT part.id, max(root_link.ordinal) AS last_position, min(root_link.ordinal) AS first_position
    FROM ros_summaries part JOIN ros_summary_sources link ON link.summary_id = part.id
    JOIN ros_summary_sources root_link ON root_link.summary_id = part.parent_id
        AND root_link.message_id = link.message_id
    WHERE part.conversation_id = ?1 AND part.kind = 'part'
    GROUP BY part.id
), anchors AS (
    SELECT src.message_id, s.id, row_number() OVER (
        PARTITION BY src.message_id ORDER BY (s.kind = 'session'), p.last_position NULLS LAST, p.first_position NULLS LAST, s.issued_part_ordinal, s.id
    ) AS rank
    FROM ros_summaries s JOIN ros_summary_sources src ON src.summary_id = s.id
    LEFT JOIN part_order p ON p.id = s.id
    WHERE s.conversation_id = ?1 AND s.kind IN ('session','part')
      AND (s.kind = 'part' OR NOT EXISTS (SELECT 1 FROM ros_summaries part
           WHERE part.parent_id = s.id AND part.kind = 'part'))
), first_anchor AS (
    SELECT s.id FROM ros_summaries s LEFT JOIN part_order p ON p.id = s.id
    WHERE s.conversation_id = ?1 AND s.kind IN ('session','part')
    ORDER BY (s.kind = 'session'), p.last_position NULLS LAST, p.first_position NULLS LAST, s.issued_part_ordinal, s.id
    LIMIT 1
)
SELECT m.id, m.role, m.metadata, m.created_at,
       a.id AS source_anchor, first_anchor.id AS fallback_anchor,
       NOT (m.content = '' AND coalesce(m.tool_name, '') = '' AND coalesce(m.tool_result, '') = '' AND coalesce(m.tool_args, 'null') IN ('null', '')) AS has_payload
FROM ros_messages m CROSS JOIN first_anchor
LEFT JOIN anchors a ON a.message_id = m.id AND a.rank = 1
WHERE m.conversation_id = ?1
ORDER BY rh_instant(m.created_at), m.id";

const COMPLETE_EMPTY_DELIVERIES: &str = "UPDATE ros_summary_deliveries AS d
SET completed_version = CASE d.consumer WHEN 'entity' THEN ?3 ELSE ?2 END, historical_backlog = 0
FROM ros_messages m
WHERE d.conversation_id = ?1 AND d.historical_backlog
  AND m.id = d.message_id AND m.conversation_id = d.conversation_id
  AND m.content = '' AND coalesce(m.tool_name, '') = '' AND coalesce(m.tool_result, '') = '' AND coalesce(m.tool_args, 'null') IN ('null', '')
  AND (EXISTS (SELECT 1 FROM ros_summaries WHERE conversation_id = ?1 AND kind = 'session')
       OR EXISTS (SELECT 1 FROM rh_summary_conversion_items WHERE conversation_id = ?1 AND state = 'complete'))";
