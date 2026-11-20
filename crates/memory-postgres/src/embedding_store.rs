use crate::summary_replace::{self, FenceError};
use async_trait::async_trait;
use kernel::memory::EmbedTable;
use kernel::memory::EmbedTarget;
use kernel::memory::EmbeddingStore;
use kernel::memory::EmbeddingStoreError;
use kernel::memory::LoadedTarget;
use kernel::memory::StoredChunk;
use kernel::memory::StoredEmbedding;
use kernel::memory::Unembeddable;
use kernel::memory::summary_reference::SummaryReferenceError;
use pgvector::HalfVector;
use sqlx::PgConnection;
use sqlx::PgPool;
use uuid::Uuid;

use crate::PostgresMemory;

const COLUMN_WIDTH: usize = 1024;

const MESSAGE_WRITE: &str = "update rh_messages
    set embedding = $2,
        content_hash = $3,
        embed_error = null,
        embed_status = null,
        embed_failures = 0
  where id = $1";

const SUMMARY_WRITE: &str = "update rh_summaries
    set embedding = $2,
        embed_error = null,
        embed_status = null,
        embed_failures = 0
  where id = $1";

const CHUNK_WRITE: &str = "update rh_message_chunks
    set embedding = $2,
        embed_error = null,
        embed_status = null,
        embed_failures = 0
  where id = $1";

const WIKI_WRITE: &str = "update rh_wiki_topics
    set embedding = $2,
        embed_error = null,
        embed_status = null,
        embed_failures = 0
  where slug = $1";

#[derive(Debug, sqlx::FromRow)]
struct MessageBody {
    content: String,
    tool_result: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct TextBody {
    content: String,
}

pub(crate) struct ChunkRow {
    index: i32,
    char_start: i32,
    char_end: i32,
    content: String,
    vector: Option<HalfVector>,
}

#[async_trait]
impl EmbeddingStore for PostgresMemory {
    async fn load(
        &self,
        target: &EmbedTarget,
    ) -> Result<Option<LoadedTarget>, EmbeddingStoreError> {
        match target.table {
            EmbedTable::Messages => load_message(&self.pool, &target.id).await,
            EmbedTable::Summaries => load_text(&self.pool, SUMMARY_LOAD, &target.id).await,
            EmbedTable::MessageChunks => load_text(&self.pool, CHUNK_LOAD, &target.id).await,
            EmbedTable::WikiTopics => load_wiki(&self.pool, &target.id).await,
        }
    }

    async fn write(&self, stored: StoredEmbedding) -> Result<(), EmbeddingStoreError> {
        match stored.target.table {
            EmbedTable::Messages => write_message(&self.pool, stored).await,
            EmbedTable::Summaries => write_summary(&self.pool, stored).await,
            EmbedTable::MessageChunks => write_uuid(&self.pool, CHUNK_WRITE, stored).await,
            EmbedTable::WikiTopics => write_wiki(&self.pool, stored).await,
        }
    }

    async fn mark_unembeddable(
        &self,
        target: &EmbedTarget,
        reason: Unembeddable,
    ) -> Result<(), EmbeddingStoreError> {
        mark_row(&self.pool, target, "unembeddable", &reason.error_text()).await
    }

    async fn record_null(
        &self,
        target: &EmbedTarget,
        error: &str,
    ) -> Result<u32, EmbeddingStoreError> {
        let count = match target.table {
            EmbedTable::Messages => bump_uuid(&self.pool, MESSAGE_NULL, target, error).await?,
            EmbedTable::Summaries => {
                let (mut tx, id, _) = guard_summary(&self.pool, target).await?;
                let count = sqlx::query_scalar(SUMMARY_NULL)
                    .bind(id)
                    .bind(error)
                    .fetch_one(&mut *tx)
                    .await
                    .map_err(|source| query_err("record null embedding", source))?;
                tx.commit()
                    .await
                    .map_err(|source| query_err("commit summary embedding", source))?;
                count
            }
            EmbedTable::MessageChunks => bump_uuid(&self.pool, CHUNK_NULL, target, error).await?,
            EmbedTable::WikiTopics => bump_slug(&self.pool, target, error).await?,
        };
        u32::try_from(count).map_err(|_| EmbeddingStoreError::FailureCount)
    }

    async fn mark_permanent(
        &self,
        target: &EmbedTarget,
        error: &str,
    ) -> Result<(), EmbeddingStoreError> {
        mark_row(&self.pool, target, "failed", error).await
    }
}

const SUMMARY_LOAD: &str = "select content from rh_summaries where id = $1";
const CHUNK_LOAD: &str = "select content from rh_message_chunks where id = $1";
const MESSAGE_NULL: &str = "update rh_messages
    set embed_failures = coalesce(embed_failures, 0) + 1, embed_error = $2
  where id = $1
  returning embed_failures";
const SUMMARY_NULL: &str = "update rh_summaries
    set embed_failures = coalesce(embed_failures, 0) + 1, embed_error = $2
  where id = $1
  returning embed_failures";
const CHUNK_NULL: &str = "update rh_message_chunks
    set embed_failures = coalesce(embed_failures, 0) + 1, embed_error = $2
  where id = $1
  returning embed_failures";

fn query_err(operation: &'static str, source: sqlx::Error) -> EmbeddingStoreError {
    EmbeddingStoreError::Query {
        message: format!("{operation}: {source}"),
        source: Box::new(source),
    }
}

fn uuid_of(id: &str) -> Result<Uuid, EmbeddingStoreError> {
    Uuid::parse_str(id).map_err(|source| EmbeddingStoreError::TargetId {
        id: id.to_owned(),
        source,
    })
}

fn missing(id: &str) -> EmbeddingStoreError {
    EmbeddingStoreError::Missing { id: id.to_owned() }
}

fn one_row(updated: u64, id: &str) -> Result<(), EmbeddingStoreError> {
    if updated == 1 {
        Ok(())
    } else {
        Err(missing(id))
    }
}

fn checked_vector(values: &[f32]) -> Result<HalfVector, EmbeddingStoreError> {
    if values.len() == COLUMN_WIDTH {
        Ok(HalfVector::from_f32_slice(values))
    } else {
        Err(EmbeddingStoreError::Width {
            got: values.len(),
            expected: COLUMN_WIDTH,
        })
    }
}

fn refuse_chunks(stored: &StoredEmbedding) -> Result<(), EmbeddingStoreError> {
    if stored.chunks.is_empty() {
        Ok(())
    } else {
        Err(EmbeddingStoreError::Chunks {
            table: stored.target.table,
        })
    }
}

fn loaded(content: String, tool_result: Option<String>) -> LoadedTarget {
    LoadedTarget {
        content,
        tool_result,
    }
}

async fn load_message(
    pool: &PgPool,
    id: &str,
) -> Result<Option<LoadedTarget>, EmbeddingStoreError> {
    let uuid = uuid_of(id)?;
    let row = sqlx::query_as::<_, MessageBody>(
        "select content, tool_result from rh_messages where id = $1",
    )
    .bind(uuid)
    .fetch_optional(pool)
    .await
    .map_err(|source| query_err("load message", source))?;
    Ok(row.map(|row| loaded(row.content, row.tool_result)))
}

async fn load_text(
    pool: &PgPool,
    sql: &'static str,
    id: &str,
) -> Result<Option<LoadedTarget>, EmbeddingStoreError> {
    let uuid = uuid_of(id)?;
    let row = sqlx::query_as::<_, TextBody>(sql)
        .bind(uuid)
        .fetch_optional(pool)
        .await
        .map_err(|source| query_err("load embed target", source))?;
    Ok(row.map(|row| loaded(row.content, None)))
}

async fn load_wiki(pool: &PgPool, slug: &str) -> Result<Option<LoadedTarget>, EmbeddingStoreError> {
    let row = sqlx::query_as::<_, TextBody>(
        "select search_text as content from rh_wiki_topics where slug = $1",
    )
    .bind(slug)
    .fetch_optional(pool)
    .await
    .map_err(|source| query_err("load wiki topic", source))?;
    Ok(row.map(|row| loaded(row.content, None)))
}

async fn write_message(pool: &PgPool, stored: StoredEmbedding) -> Result<(), EmbeddingStoreError> {
    let id = uuid_of(&stored.target.id)?;
    let vector = checked_vector(&stored.vector)?;
    let chunks = prepare_chunks(&stored.chunks)?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|source| query_err("begin message embed", source))?;
    let updated = sqlx::query(MESSAGE_WRITE)
        .bind(id)
        .bind(vector)
        .bind(&stored.content_hash)
        .execute(&mut *tx)
        .await
        .map_err(|source| query_err("write message embedding", source))?;
    one_row(updated.rows_affected(), &stored.target.id)?;
    sqlx::query("delete from rh_message_chunks where message_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await
        .map_err(|source| query_err("clear message chunks", source))?;
    for chunk in &chunks {
        insert_chunk(&mut tx, id, chunk).await?;
    }
    tx.commit()
        .await
        .map_err(|source| query_err("commit message embed", source))?;
    Ok(())
}

pub(crate) fn prepare_chunks(chunks: &[StoredChunk]) -> Result<Vec<ChunkRow>, EmbeddingStoreError> {
    chunks.iter().map(prepare_chunk).collect()
}

fn prepare_chunk(chunk: &StoredChunk) -> Result<ChunkRow, EmbeddingStoreError> {
    Ok(ChunkRow {
        index: i32::try_from(chunk.index).map_err(|_| EmbeddingStoreError::ChunkIndex)?,
        char_start: i32::try_from(chunk.char_start).map_err(|_| EmbeddingStoreError::ChunkIndex)?,
        char_end: i32::try_from(chunk.char_end).map_err(|_| EmbeddingStoreError::ChunkIndex)?,
        content: chunk.content.clone(),
        vector: chunk.vector.as_deref().map(checked_vector).transpose()?,
    })
}

pub(crate) async fn insert_chunk(
    connection: &mut PgConnection,
    message_id: Uuid,
    chunk: &ChunkRow,
) -> Result<(), EmbeddingStoreError> {
    sqlx::query(
        "insert into rh_message_chunks
            (message_id, idx, char_start, char_end, content, embedding, embed_failures, owner_user_id)
         values ($1, $2, $3, $4, $5, $6, 0, (select owner_user_id from rh_messages where id = $1))",
    )
    .bind(message_id)
    .bind(chunk.index)
    .bind(chunk.char_start)
    .bind(chunk.char_end)
    .bind(&chunk.content)
    .bind(chunk.vector.clone())
    .execute(&mut *connection)
    .await
    .map_err(|source| query_err("insert message chunk", source))?;
    Ok(())
}

async fn write_uuid(
    pool: &PgPool,
    sql: &'static str,
    stored: StoredEmbedding,
) -> Result<(), EmbeddingStoreError> {
    refuse_chunks(&stored)?;
    let id = uuid_of(&stored.target.id)?;
    let vector = checked_vector(&stored.vector)?;
    let updated = sqlx::query(sql)
        .bind(id)
        .bind(vector)
        .execute(pool)
        .await
        .map_err(|source| query_err("write embedding", source))?;
    one_row(updated.rows_affected(), &stored.target.id)
}

async fn write_wiki(pool: &PgPool, stored: StoredEmbedding) -> Result<(), EmbeddingStoreError> {
    refuse_chunks(&stored)?;
    let vector = checked_vector(&stored.vector)?;
    let updated = sqlx::query(WIKI_WRITE)
        .bind(&stored.target.id)
        .bind(vector)
        .execute(pool)
        .await
        .map_err(|source| query_err("write wiki embedding", source))?;
    one_row(updated.rows_affected(), &stored.target.id)
}

async fn mark_row(
    pool: &PgPool,
    target: &EmbedTarget,
    status: &str,
    error: &str,
) -> Result<(), EmbeddingStoreError> {
    match target.table {
        EmbedTable::Messages => mark_uuid(pool, MESSAGE_MARK, target, status, error).await,
        EmbedTable::Summaries => {
            let (mut tx, id, _) = guard_summary(pool, target).await?;
            sqlx::query(SUMMARY_MARK)
                .bind(id)
                .bind(status)
                .bind(error)
                .execute(&mut *tx)
                .await
                .map_err(|source| query_err("mark summary embedding", source))?;
            tx.commit()
                .await
                .map_err(|source| query_err("commit summary embedding", source))
        }
        EmbedTable::MessageChunks => mark_uuid(pool, CHUNK_MARK, target, status, error).await,
        EmbedTable::WikiTopics => mark_slug(pool, target, status, error).await,
    }
}

const MESSAGE_MARK: &str = "update rh_messages
    set embed_status = $2, embed_error = $3, embedding = null
  where id = $1";
const SUMMARY_MARK: &str = "update rh_summaries
    set embed_status = $2, embed_error = $3, embedding = null
  where id = $1";
const CHUNK_MARK: &str = "update rh_message_chunks
    set embed_status = $2, embed_error = $3, embedding = null
  where id = $1";

async fn mark_uuid(
    pool: &PgPool,
    sql: &'static str,
    target: &EmbedTarget,
    status: &str,
    error: &str,
) -> Result<(), EmbeddingStoreError> {
    let id = uuid_of(&target.id)?;
    let updated = sqlx::query(sql)
        .bind(id)
        .bind(status)
        .bind(error)
        .execute(pool)
        .await
        .map_err(|source| query_err("mark embed target", source))?;
    one_row(updated.rows_affected(), &target.id)
}

async fn mark_slug(
    pool: &PgPool,
    target: &EmbedTarget,
    status: &str,
    error: &str,
) -> Result<(), EmbeddingStoreError> {
    let updated = sqlx::query(
        "update rh_wiki_topics
            set embed_status = $2, embed_error = $3, embedding = null
          where slug = $1",
    )
    .bind(&target.id)
    .bind(status)
    .bind(error)
    .execute(pool)
    .await
    .map_err(|source| query_err("mark wiki topic", source))?;
    one_row(updated.rows_affected(), &target.id)
}

async fn bump_uuid(
    pool: &PgPool,
    sql: &'static str,
    target: &EmbedTarget,
    error: &str,
) -> Result<i32, EmbeddingStoreError> {
    let id = uuid_of(&target.id)?;
    let count = sqlx::query_scalar::<_, i32>(sql)
        .bind(id)
        .bind(error)
        .fetch_optional(pool)
        .await
        .map_err(|source| query_err("record null embedding", source))?;
    count.ok_or_else(|| missing(&target.id))
}

async fn bump_slug(
    pool: &PgPool,
    target: &EmbedTarget,
    error: &str,
) -> Result<i32, EmbeddingStoreError> {
    let count = sqlx::query_scalar::<_, i32>(
        "update rh_wiki_topics
            set embed_failures = coalesce(embed_failures, 0) + 1, embed_error = $2
          where slug = $1
          returning embed_failures",
    )
    .bind(&target.id)
    .bind(error)
    .fetch_optional(pool)
    .await
    .map_err(|source| query_err("record null wiki embedding", source))?;
    count.ok_or_else(|| missing(&target.id))
}

async fn guard_summary<'a>(
    pool: &'a PgPool,
    target: &EmbedTarget,
) -> Result<
    (
        sqlx::Transaction<'a, sqlx::Postgres>,
        Uuid,
        summary_replace::SummaryInput,
    ),
    EmbeddingStoreError,
> {
    let id = uuid_of(&target.id)?;
    let mut tx = pool
        .begin()
        .await
        .map_err(|source| query_err("begin summary embedding", source))?;
    let input = summary_replace::shared(&mut tx, &[id])
        .await
        .map_err(|error| match error {
            FenceError::Reference(error) => EmbeddingStoreError::SummaryReference(error),
            FenceError::Query(error) => query_err("lock summary embedding", error),
        })?
        .pop()
        .ok_or(SummaryReferenceError::Gone(id))?;
    if input.input_key.is_some() {
        return Err(SummaryReferenceError::Stale(id).into());
    }
    Ok((tx, id, input))
}

async fn write_summary(pool: &PgPool, stored: StoredEmbedding) -> Result<(), EmbeddingStoreError> {
    refuse_chunks(&stored)?;
    let (mut tx, id, input) = guard_summary(pool, &stored.target).await?;
    if kernel::memory::summary_reference::embedding_input_hash(&input.content)
        != stored.content_hash
    {
        return Err(SummaryReferenceError::Stale(id).into());
    }
    sqlx::query(SUMMARY_WRITE)
        .bind(id)
        .bind(checked_vector(&stored.vector)?)
        .execute(&mut *tx)
        .await
        .map_err(|source| query_err("write summary embedding", source))?;
    tx.commit()
        .await
        .map_err(|source| query_err("commit summary embedding", source))
}
