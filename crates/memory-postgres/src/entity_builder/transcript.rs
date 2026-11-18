use jiff::Timestamp;
use std::collections::HashSet;

use kernel::memory::MemoryBrowseOrder;
use kernel::memory::summary_order::{MessagePosition, order_messages};
use kernel::wiki::entity_pass::EntityLeaf;
use kernel::wiki::entity_pass::EntityStoreError;
use kernel::wiki::entity_pass::TranscriptMessage;
use serde_json::Value;
use sqlx::types::Json;
use uuid::Uuid;
use wire::JsonObject;
use wire::message::Role;

use super::error::query_error;

use super::sql::SOURCE_LINKS;
use super::sql::TRANSCRIPT_MESSAGES;
use super::sql::WINDOW_MESSAGES;

#[derive(Debug, sqlx::FromRow)]
struct SourceLink {
    summary_id: Uuid,
    message_id: Uuid,
}

#[derive(Debug, sqlx::FromRow)]
struct TranscriptRow {
    id: Uuid,
    conversation_id: Uuid,
    metadata: Option<Json<JsonObject>>,
    #[sqlx(try_from = "jiff_sqlx::Timestamp")]
    created_at: Timestamp,
    role: Json<Role>,
    tool_name: Option<String>,
    content: String,
    tool_args: Option<Value>,
    tool_result: Option<String>,
}

pub(super) async fn load_transcript(
    pool: &sqlx::PgPool,
    leaves: &[EntityLeaf],
) -> Result<Vec<TranscriptMessage>, EntityStoreError> {
    if leaves.is_empty() {
        return Ok(Vec::new());
    }
    let summary_ids: Vec<Uuid> = leaves
        .iter()
        .filter(|leaf| leaf.delivery.is_none())
        .map(|leaf| leaf.id)
        .collect();
    let links = sqlx::query_as::<_, SourceLink>(SOURCE_LINKS)
        .bind(&summary_ids)
        .fetch_all(pool)
        .await
        .map_err(query_error)?;
    let mut covered = HashSet::new();
    let mut wanted = HashSet::new();
    for link in &links {
        covered.insert(link.summary_id);
        wanted.insert(link.message_id);
    }
    for leaf in leaves {
        if let Some(delivery) = &leaf.delivery {
            wanted.extend(delivery.pending.iter().copied());
            continue;
        }
        if covered.contains(&leaf.id) {
            continue;
        }
        let Some(conversation) = leaf.conversation_id else {
            continue;
        };
        let ids = sqlx::query_scalar::<_, Uuid>(WINDOW_MESSAGES)
            .bind(conversation)
            .bind(crate::time::BindTime::bind_time(leaf.start))
            .bind(crate::time::BindTime::bind_time(leaf.end))
            .fetch_all(pool)
            .await
            .map_err(query_error)?;
        wanted.extend(ids);
    }
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let ids: Vec<Uuid> = wanted.into_iter().collect();
    let rows = sqlx::query_as::<_, TranscriptRow>(TRANSCRIPT_MESSAGES)
        .bind(&ids)
        .fetch_all(pool)
        .await
        .map_err(query_error)?;
    let rows = order_messages(rows, MemoryBrowseOrder::Asc, |row| {
        MessagePosition::new(
            row.created_at,
            row.role.0,
            row.metadata.as_ref().map(|json| &json.0),
        )
        .in_conversation(row.conversation_id)
    });
    Ok(rows.into_iter().map(message_from).collect())
}

fn message_from(row: TranscriptRow) -> TranscriptMessage {
    let tool_name = row.tool_name.filter(|name| !name.trim().is_empty());
    let tool_args = row.tool_args.filter(|value| !value.is_null());
    let tool_result = row.tool_result.filter(|text| !text.is_empty());
    TranscriptMessage {
        id: row.id,
        at: row.created_at,
        role: row.role.0.to_string(),
        tool_name,
        content: row.content,
        tool_args,
        tool_result,
    }
}
