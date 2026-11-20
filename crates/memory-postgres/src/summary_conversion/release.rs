use jiff::Timestamp;
use kernel::wiki::{builder::WIKI_PIPELINE_VERSION, entity_pass::ENTITY_PIPELINE_VERSION};
use sqlx::PgConnection;
use uuid::Uuid;

pub(super) async fn deliveries(
    conn: &mut PgConnection,
    conversation: Uuid,
) -> Result<(), sqlx::Error> {
    anchor_deliveries(conn, conversation).await?;
    sqlx::query(COMPLETE_EMPTY_DELIVERIES)
        .bind(conversation)
        .bind(WIKI_PIPELINE_VERSION)
        .bind(ENTITY_PIPELINE_VERSION)
        .execute(conn)
        .await?;
    Ok(())
}

async fn anchor_deliveries(conn: &mut PgConnection, conversation: Uuid) -> Result<(), sqlx::Error> {
    use kernel::memory::MemoryBrowseOrder;
    use kernel::memory::summary_order::{MessagePosition, order_messages};
    use sqlx::types::Json;
    use wire::{JsonObject, message::Role};

    #[derive(sqlx::FromRow)]
    struct Input {
        id: Uuid,
        role: Json<Role>,
        metadata: Option<Json<JsonObject>>,
        #[sqlx(try_from = "jiff_sqlx::Timestamp")]
        created_at: Timestamp,
        source_anchor: Option<Uuid>,
        fallback_anchor: Uuid,
        has_payload: bool,
    }

    let inputs: Vec<Input> = sqlx::query_as(RELEASE_INPUTS)
        .bind(conversation)
        .fetch_all(&mut *conn)
        .await?;
    let ordered = order_messages(inputs, MemoryBrowseOrder::Asc, |input| {
        MessagePosition::new(
            input.created_at,
            input.role.0,
            input.metadata.as_ref().map(|json| &json.0),
        )
    });
    let mut preceding = None;
    let (messages, anchors): (Vec<_>, Vec<_>) = ordered
        .into_iter()
        .filter_map(|input| {
            preceding = input.source_anchor.or(preceding);
            (input.source_anchor.is_some() || input.has_payload)
                .then_some((input.id, preceding.unwrap_or(input.fallback_anchor)))
        })
        .unzip();
    sqlx::query(
        "UPDATE rh_summary_deliveries d SET anchor_id = a.anchor_id, historical_backlog = false
         FROM unnest($2::uuid[], $3::uuid[]) a(message_id, anchor_id)
         WHERE d.conversation_id = $1 AND d.historical_backlog AND d.message_id = a.message_id",
    )
    .bind(conversation)
    .bind(&messages)
    .bind(&anchors)
    .execute(conn)
    .await?;
    Ok(())
}

const RELEASE_INPUTS: &str = "WITH part_order AS (
    SELECT part.id, max(root_link.ordinal) AS last_position, min(root_link.ordinal) AS first_position
    FROM rh_summaries part JOIN rh_summary_sources link ON link.summary_id = part.id
    JOIN rh_summary_sources root_link ON root_link.summary_id = part.parent_id
        AND root_link.message_id = link.message_id
    WHERE part.conversation_id = $1 AND part.kind = 'part'
    GROUP BY part.id
), anchors AS (
    SELECT DISTINCT ON (src.message_id) src.message_id, s.id
    FROM rh_summaries s JOIN rh_summary_sources src ON src.summary_id = s.id
    LEFT JOIN part_order p ON p.id = s.id
    WHERE s.conversation_id = $1 AND s.kind IN ('session','part')
    AND (s.kind = 'part' OR NOT EXISTS (SELECT 1 FROM rh_summaries part
         WHERE part.parent_id = s.id AND part.kind = 'part'))
    ORDER BY src.message_id, (s.kind = 'session'), p.last_position NULLS LAST, p.first_position NULLS LAST, s.issued_part_ordinal, s.id
), first_anchor AS (
    SELECT s.id FROM rh_summaries s LEFT JOIN part_order p ON p.id = s.id
    WHERE s.conversation_id = $1 AND s.kind IN ('session','part')
    ORDER BY (s.kind = 'session'), p.last_position NULLS LAST, p.first_position NULLS LAST, s.issued_part_ordinal, s.id
    LIMIT 1
)
SELECT m.id, to_json(m.role) AS role, CASE WHEN jsonb_typeof(m.metadata) = 'object' THEN m.metadata END AS metadata, m.created_at,
       a.id AS source_anchor, first_anchor.id AS fallback_anchor,
       NOT (m.content = '' AND coalesce(m.tool_name, '') = '' AND coalesce(m.tool_result, '') = '' AND coalesce(m.tool_args::text, 'null') IN ('null', '')) AS has_payload
FROM rh_messages m CROSS JOIN first_anchor
LEFT JOIN anchors a ON a.message_id = m.id
WHERE m.conversation_id = $1
ORDER BY m.created_at, m.id";

const COMPLETE_EMPTY_DELIVERIES: &str = "UPDATE rh_summary_deliveries d
SET completed_version = CASE d.consumer WHEN 'entity' THEN $3 ELSE $2 END, historical_backlog = false
FROM rh_messages m
WHERE d.conversation_id = $1 AND d.historical_backlog
  AND m.id = d.message_id AND m.conversation_id = d.conversation_id
  AND m.content = '' AND coalesce(m.tool_name, '') = '' AND coalesce(m.tool_result, '') = '' AND coalesce(m.tool_args::text, 'null') IN ('null', '')
  AND (EXISTS (SELECT 1 FROM rh_summaries WHERE conversation_id = $1 AND kind = 'session')
       OR EXISTS (SELECT 1 FROM rh_summary_conversion_items WHERE conversation_id = $1 AND state = 'complete'))";
