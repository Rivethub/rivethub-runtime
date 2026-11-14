use std::collections::HashSet;

use chrono::DateTime;
use chrono::Utc;
use kernel::wiki::entity_pass::EntityLeaf;
use kernel::wiki::entity_pass::EntityStoreError;
use kernel::wiki::entity_pass::TranscriptMessage;
use serde_json::Value;
use uuid::Uuid;

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
    created_at: DateTime<Utc>,
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            stringly,
            reason = "a transcript speaker is harness text, not the worker Role enum"
        )
    )]
    role: String,
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
    let summary_ids: Vec<Uuid> = leaves.iter().map(|leaf| leaf.id).collect();
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
        if covered.contains(&leaf.id) {
            continue;
        }
        let Some(conversation) = leaf.conversation_id else {
            continue;
        };
        let ids = sqlx::query_scalar::<_, Uuid>(WINDOW_MESSAGES)
            .bind(conversation)
            .bind(leaf.start)
            .bind(leaf.end)
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
    Ok(rows.into_iter().map(message_from).collect())
}

fn message_from(row: TranscriptRow) -> TranscriptMessage {
    let tool_name = row.tool_name.filter(|name| !name.trim().is_empty());
    let tool_args = row.tool_args.filter(|value| !value.is_null());
    let tool_result = row.tool_result.filter(|text| !text.is_empty());
    TranscriptMessage {
        id: row.id,
        at: row.created_at,
        role: row.role,
        tool_name,
        content: row.content,
        tool_args,
        tool_result,
    }
}
