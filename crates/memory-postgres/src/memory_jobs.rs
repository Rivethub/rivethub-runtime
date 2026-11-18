use async_trait::async_trait;
use kernel::memory::jobs::{
    ChunkWrite, MemoryJobError, MemoryJobStore, MissingWiki, ToolCallMessage, UnchunkedMessage,
    UnembeddedStore,
};
use kernel::memory::{EmbedTable, EmbedTarget, StoredChunk};
use kernel::queue::{FixedJob, JobName};
use kernel::wiki::builder::ExtractWiki;
use serde::Deserialize;
use sqlx::types::Json;
use uuid::Uuid;
use wire::JsonObject;

use crate::PostgresMemory;
use crate::graphile::GraphileQueue;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum ToolArguments {
    Object(JsonObject),
    Encoded(String),
}

#[derive(sqlx::FromRow)]
struct ToolRow {
    id: Uuid,
    tool_name: Option<String>,
    tool_args: Option<Json<ToolArguments>>,
    content: String,
}

#[derive(sqlx::FromRow)]
struct UnchunkedRow {
    id: Uuid,
    content: String,
    tool_result: Option<String>,
}

const UNCHUNKED: &str = "SELECT m.id, m.content, m.tool_result FROM (
         SELECT m.id, m.content, m.tool_result, m.created_at FROM rh_messages m
         WHERE NOT EXISTS (SELECT 1 FROM rh_message_chunks c WHERE c.message_id = m.id)
           AND COALESCE(m.embed_status, '') <> 'unembeddable'
           AND coalesce(octet_length(m.content), 0)
               + least(coalesce(octet_length(m.tool_result), 0), $2) + 1 > $1
         ORDER BY m.created_at DESC
         OFFSET 0
     ) m
     CROSS JOIN LATERAL (SELECT
         length(btrim(coalesce(m.content, ''), E' \\t\\n\\r\\f\\x0B')) AS content_chars,
         length(btrim(coalesce(m.tool_result, ''), E' \\t\\n\\r\\f\\x0B')) AS tool_chars) l
     WHERE (CASE WHEN l.content_chars = 0 THEN least(l.tool_chars, $2)
                 WHEN l.tool_chars = 0 THEN l.content_chars
                 ELSE l.content_chars + least(l.tool_chars, $2) + 1 END) > $1
     ORDER BY m.created_at DESC LIMIT $3";

const MISSING_WIKI: &str = "SELECT s.id, s.conversation_id, s.created_at FROM rh_summaries s
     LEFT JOIN rh_conversations c ON c.id = s.conversation_id
     WHERE s.kind = 'leaf'
       AND (c.id IS NULL OR c.session_key IS NULL OR c.session_key NOT LIKE 'heartbeat:%')
       AND NOT EXISTS (SELECT 1 FROM rh_wiki_extractions e WHERE e.summary_id = s.id)
       AND NOT EXISTS (SELECT 1 FROM graphile_worker._private_jobs j WHERE j.key = 'wiki-ext-' || s.id::text)
       AND NOT EXISTS (SELECT 1 FROM rh_flagged_jobs f WHERE f.job_key = 'wiki-ext-' || s.id::text)
     ORDER BY s.created_at DESC LIMIT $1";

#[async_trait]
impl MemoryJobStore for PostgresMemory {
    async fn tool_call(&self, id: Uuid) -> Result<Option<ToolCallMessage>, MemoryJobError> {
        let row = sqlx::query_as::<_, ToolRow>(
            "SELECT id, tool_name, NULLIF(tool_args, 'null'::jsonb) AS tool_args, content FROM rh_messages WHERE id = $1",
        )
        .bind(id)
        .fetch_optional(&self.pool)
        .await
        .map_err(query_error)?;
        row.map(|row| {
            Ok(ToolCallMessage {
                id: row.id,
                tool_name: row.tool_name,
                tool_args: arguments(row.tool_args)?,
                content: row.content,
            })
        })
        .transpose()
    }

    async fn write_tool_sentence(
        &self,
        id: Uuid,
        previous_content: &str,
        content: &str,
    ) -> Result<(), MemoryJobError> {
        sqlx::query("UPDATE rh_messages SET content = $2 WHERE id = $1 AND content = $3")
            .bind(id)
            .bind(content)
            .bind(previous_content)
            .execute(&self.pool)
            .await
            .map_err(query_error)?;
        Ok(())
    }

    async fn unchunked(
        &self,
        limit: u32,
        chars_per_chunk: u32,
        tool_result_cap: u32,
    ) -> Result<Vec<UnchunkedMessage>, MemoryJobError> {
        let rows = sqlx::query_as::<_, UnchunkedRow>(UNCHUNKED)
            .bind(i64::from(chars_per_chunk))
            .bind(i64::from(tool_result_cap))
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(query_error)?;
        Ok(rows
            .into_iter()
            .map(|row| UnchunkedMessage {
                id: row.id,
                content: row.content,
                tool_result: row.tool_result,
            })
            .collect())
    }

    async fn write_chunks(
        &self,
        message_id: Uuid,
        chunks: Vec<StoredChunk>,
    ) -> Result<ChunkWrite, MemoryJobError> {
        let rows = crate::embedding_store::prepare_chunks(&chunks)
            .map_err(|error| MemoryJobError::Query(Box::new(error)))?;
        let mut tx = self.pool.begin().await.map_err(query_error)?;
        let unchunked = sqlx::query_scalar::<_, bool>(
            "SELECT NOT EXISTS (SELECT 1 FROM rh_message_chunks c WHERE c.message_id = m.id)
             FROM rh_messages m WHERE m.id = $1 FOR UPDATE",
        )
        .bind(message_id)
        .fetch_optional(&mut *tx)
        .await
        .map_err(query_error)?;
        if unchunked != Some(true) {
            return Ok(ChunkWrite::Skipped);
        }
        for row in &rows {
            crate::embedding_store::insert_chunk(&mut tx, message_id, row)
                .await
                .map_err(|error| MemoryJobError::Query(Box::new(error)))?;
        }
        tx.commit().await.map_err(query_error)?;
        Ok(ChunkWrite::Written)
    }

    async fn unembedded_chunks(
        &self,
        message_id: Uuid,
    ) -> Result<Vec<EmbedTarget>, MemoryJobError> {
        let ids = sqlx::query_scalar::<_, String>(
            "SELECT id::text FROM rh_message_chunks
             WHERE message_id = $1 AND embedding IS NULL ORDER BY idx",
        )
        .bind(message_id)
        .fetch_all(&self.pool)
        .await
        .map_err(query_error)?;
        Ok(ids
            .into_iter()
            .map(|id| EmbedTarget {
                table: EmbedTable::MessageChunks,
                id,
            })
            .collect())
    }

    async fn revive_dead_wiki(&self, limit: u32) -> Result<u64, MemoryJobError> {
        GraphileQueue::new(self.pool.clone())
            .reschedule_dead(&JobName::Fixed(FixedJob::ExtractWiki), limit, 10)
            .await
            .map_err(|error| MemoryJobError::Query(Box::new(error)))
    }

    async fn missing_wiki(&self, limit: u32) -> Result<Vec<MissingWiki>, MemoryJobError> {
        let rows = sqlx::query_as::<_, (Uuid, Option<Uuid>, jiff_sqlx::Timestamp)>(MISSING_WIKI)
            .bind(i64::from(limit))
            .fetch_all(&self.pool)
            .await
            .map_err(query_error)?;
        Ok(rows
            .into_iter()
            .map(|(summary_id, conversation_id, summarized_at)| MissingWiki {
                job: ExtractWiki {
                    summary_id,
                    conversation_id,
                },
                summarized_at: summarized_at.to_jiff(),
            })
            .collect())
    }
}

#[async_trait]
impl UnembeddedStore for PostgresMemory {
    async fn unembedded(
        &self,
        table: EmbedTable,
        limit: u32,
    ) -> Result<Vec<EmbedTarget>, MemoryJobError> {
        let result = unembedded(self, table, limit).await;
        match result {
            Err(error)
                if matches!(table, EmbedTable::WikiTopics | EmbedTable::MessageChunks)
                    && crate::Error::relation_missing(&error) =>
            {
                Err(MemoryJobError::MissingTable {
                    table,
                    source: Box::new(error),
                })
            }
            result => result.map_err(query_error),
        }
    }
}

fn arguments(args: Option<Json<ToolArguments>>) -> Result<JsonObject, MemoryJobError> {
    let args = match args {
        Some(Json(ToolArguments::Object(args))) => args,
        Some(Json(ToolArguments::Encoded(text))) if !text.trim().is_empty() => {
            serde_json::from_str(&text)
                .map_err(|error| MemoryJobError::Arguments(Box::new(error)))?
        }
        Some(Json(ToolArguments::Encoded(_))) | None => JsonObject::default(),
    };
    Ok(args)
}

async fn unembedded(
    store: &PostgresMemory,
    table: EmbedTable,
    limit: u32,
) -> Result<Vec<EmbedTarget>, sqlx::Error> {
    let (relation, id, predicate) = match table {
        EmbedTable::Messages => (
            "rh_messages",
            "id",
            "(length(content) > 20 OR length(tool_result) > 20)",
        ),
        EmbedTable::Summaries => ("rh_summaries", "id", "length(content) > 20"),
        EmbedTable::WikiTopics => ("rh_wiki_topics", "slug", "length(search_text) > 20"),
        EmbedTable::MessageChunks => ("rh_message_chunks", "id", "btrim(content) <> ''"),
    };
    let mut connection = store.pool.acquire().await?;
    if table != EmbedTable::MessageChunks {
        let mut heal = sqlx::QueryBuilder::<sqlx::Postgres>::new("UPDATE ");
        heal.push(relation).push(" SET embed_status = NULL, embed_error = NULL, embed_failures = 0 WHERE embedding IS NULL AND embed_status = 'failed' AND embed_error = ")
            .push_bind("Embedding returned null").push(" AND ").push(predicate);
        heal.build().execute(&mut *connection).await?;
    }
    let mut select = sqlx::QueryBuilder::<sqlx::Postgres>::new("SELECT ");
    select
        .push(id)
        .push("::text FROM ")
        .push(relation)
        .push(" WHERE embedding IS NULL AND embed_status IS NULL AND ")
        .push(predicate)
        .push(" ORDER BY created_at DESC LIMIT ")
        .push_bind(i64::from(limit));
    let ids = select
        .build_query_scalar::<String>()
        .fetch_all(&mut *connection)
        .await?;
    Ok(ids
        .into_iter()
        .map(|id| EmbedTarget { table, id })
        .collect())
}

fn query_error(error: sqlx::Error) -> MemoryJobError {
    MemoryJobError::Query(Box::new(error))
}
