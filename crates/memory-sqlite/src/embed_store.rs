use async_trait::async_trait;
use kernel::memory::EmbedModelMismatch;
use kernel::memory::EmbedTable;
use kernel::memory::EmbedTarget;
use kernel::memory::EmbeddingStore;
use kernel::memory::EmbeddingStoreError;
use kernel::memory::LoadedTarget;
use kernel::memory::StoredEmbedding;
use kernel::memory::Unembeddable;
use rusqlite::OptionalExtension as _;
use rusqlite::TransactionBehavior;
use rusqlite::params_from_iter;
use rusqlite::types::Value;
use uuid::Uuid;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::meta;
use crate::meta::MetaKey;
use kernel::memory::summary_reference::SummaryReferenceError;

use crate::chunk_rows;

#[async_trait]
impl EmbeddingStore for SqliteStore {
    async fn load(
        &self,
        target: &EmbedTarget,
    ) -> Result<Option<LoadedTarget>, EmbeddingStoreError> {
        let target = target.clone();
        self.with_conn(move |conn| load_target(conn, &target))
            .await
            .map_err(embed_err)
    }

    async fn write(&self, stored: StoredEmbedding) -> Result<(), EmbeddingStoreError> {
        if stored.target.table != EmbedTable::Messages && !stored.chunks.is_empty() {
            return Err(EmbeddingStoreError::Chunks {
                table: stored.target.table,
            });
        }
        let model = self.embed_model.clone();
        let now = self.now();
        self.with_conn(move |conn| {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(SqliteError::query("write embedding"))?;
            guard_summary(&tx, &stored.target, Some(&stored.content_hash))?;
            write_target(&tx, &stored, model.as_deref(), now)?;
            tx.commit().map_err(SqliteError::query("write embedding"))
        })
        .await
        .map_err(embed_err)
    }

    async fn mark_unembeddable(
        &self,
        target: &EmbedTarget,
        reason: Unembeddable,
    ) -> Result<(), EmbeddingStoreError> {
        let target = target.clone();
        let error = reason.error_text();
        self.with_conn(move |conn| {
            guarded(conn, &target, |tx| {
                mark_target(tx, &target, "unembeddable", &error)
            })
        })
        .await
        .map_err(embed_err)
    }

    async fn record_null(
        &self,
        target: &EmbedTarget,
        error: &str,
    ) -> Result<u32, EmbeddingStoreError> {
        let target = target.clone();
        let error = error.to_owned();
        self.with_conn(move |conn| guarded(conn, &target, |tx| bump_failure(tx, &target, &error)))
            .await
            .map_err(embed_err)
    }

    async fn mark_permanent(
        &self,
        target: &EmbedTarget,
        error: &str,
    ) -> Result<(), EmbeddingStoreError> {
        let target = target.clone();
        let error = error.to_owned();
        self.with_conn(move |conn| {
            guarded(conn, &target, |tx| {
                mark_target(tx, &target, "failed", &error)
            })
        })
        .await
        .map_err(embed_err)
    }
}

fn load_target(
    conn: &rusqlite::Connection,
    target: &EmbedTarget,
) -> Result<Option<LoadedTarget>, SqliteError> {
    match target.table {
        EmbedTable::Messages => load_message(conn, &target.id),
        EmbedTable::Summaries => load_text(conn, "ros_summaries", &target.id, true),
        EmbedTable::WikiTopics => load_text(conn, "ros_wiki_topics", &target.id, false),
        EmbedTable::MessageChunks => load_chunk(conn, &target.id),
    }
}

impl SqliteStore {
    pub async fn embed_mismatch(&self) -> Result<Option<EmbedModelMismatch>, SqliteError> {
        let Some(configured) = self.embed_model.clone() else {
            return Ok(None);
        };
        self.with_conn(move |conn| {
            Ok(meta::read(conn, MetaKey::EmbedModel)?
                .filter(|stored| *stored != configured)
                .map(|stored| EmbedModelMismatch { stored, configured }))
        })
        .await
    }
}

fn write_target(
    conn: &rusqlite::Connection,
    stored: &StoredEmbedding,
    model: Option<&str>,
    now: jiff::Timestamp,
) -> Result<(), SqliteError> {
    if stored.target.table != EmbedTable::MessageChunks {
        require_width(conn, stored.vector.len())?;
    }
    if let Some(model) = model {
        require_model(conn, model, stored.vector.len())?;
    }
    let bytes = crate::vector::pack(&stored.vector);
    let updated = match stored.target.table {
        EmbedTable::Messages | EmbedTable::Summaries => exec_id(
            conn,
            &format!(
                "UPDATE {} SET embedding = ?2, embed_error = NULL, embed_status = 'done', \
                 embed_failures = 0 WHERE id = ?1",
                table_name(stored.target.table)
            ),
            &stored.target.id,
            Some(bytes),
        )?,
        EmbedTable::WikiTopics => exec_slug(
            conn,
            "UPDATE ros_wiki_topics SET embedding = ?2, embed_error = NULL, embed_status = 'done', \
             embed_failures = 0 WHERE slug = ?1",
            &stored.target.id,
            Some(bytes),
        )?,
        EmbedTable::MessageChunks => {
            chunk_rows::write_embedding(conn, &uuid_text(&stored.target.id)?, &bytes)?
        }
    };
    one_row(updated, &stored.target.id)?;
    if stored.target.table == EmbedTable::Messages {
        chunk_rows::replace(conn, &uuid_text(&stored.target.id)?, &stored.chunks, now)?;
    }
    Ok(())
}

fn mark_target(
    conn: &rusqlite::Connection,
    target: &EmbedTarget,
    status: &str,
    error: &str,
) -> Result<(), SqliteError> {
    let updated = match target.table {
        EmbedTable::Messages | EmbedTable::Summaries => conn
            .execute(
                &format!(
                    "UPDATE {} SET embed_status = ?2, embed_error = ?3, embedding = NULL WHERE id = ?1",
                    table_name(target.table)
                ),
                params_from_iter([
                    Value::Text(identity(target)?),
                    Value::Text(status.to_owned()),
                    Value::Text(error.to_owned()),
                ]),
            )
            .map_err(SqliteError::query("mark embedding"))?,
        EmbedTable::WikiTopics => conn
            .execute(
                "UPDATE ros_wiki_topics SET embed_status = ?2, embed_error = ?3, embedding = NULL \
                 WHERE slug = ?1",
                params_from_iter([
                    Value::Text(identity(target)?),
                    Value::Text(status.to_owned()),
                    Value::Text(error.to_owned()),
                ]),
            )
            .map_err(SqliteError::query("mark embedding"))?,
        EmbedTable::MessageChunks => {
            chunk_rows::mark(conn, &uuid_text(&target.id)?, status, error)?
        }
    };
    one_row(updated, &target.id)
}

fn bump_failure(
    conn: &rusqlite::Connection,
    target: &EmbedTarget,
    error: &str,
) -> Result<u32, SqliteError> {
    let count = match target.table {
        EmbedTable::MessageChunks => chunk_rows::bump(conn, &uuid_text(&target.id)?, error)?,
        EmbedTable::Messages | EmbedTable::Summaries => failure_row(
            conn,
            &format!(
                "UPDATE {} SET embed_failures = embed_failures + 1, embed_error = ?2 \
                  WHERE id = ?1 RETURNING embed_failures",
                table_name(target.table)
            ),
            target,
            error,
        )?,
        EmbedTable::WikiTopics => failure_row(
            conn,
            "UPDATE ros_wiki_topics SET embed_failures = embed_failures + 1, embed_error = ?2 \
              WHERE slug = ?1 RETURNING embed_failures",
            target,
            error,
        )?,
    };
    match count {
        Some(count) => u32::try_from(count).map_err(SqliteError::Count),
        None => Err(SqliteError::MissingHit {
            id: target.id.clone(),
        }),
    }
}

fn failure_row(
    conn: &rusqlite::Connection,
    sql: &str,
    target: &EmbedTarget,
    error: &str,
) -> Result<Option<i64>, SqliteError> {
    conn.query_row(
        sql,
        params_from_iter([
            Value::Text(identity(target)?),
            Value::Text(error.to_owned()),
        ]),
        |row| row.get(0),
    )
    .optional()
    .map_err(SqliteError::query("record null embedding"))
}

fn load_chunk(conn: &rusqlite::Connection, id: &str) -> Result<Option<LoadedTarget>, SqliteError> {
    Ok(
        chunk_rows::load(conn, &uuid_text(id)?)?.map(|content| LoadedTarget {
            content,
            tool_result: None,
        }),
    )
}

fn load_message(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<Option<LoadedTarget>, SqliteError> {
    let id = uuid_text(id)?;
    conn.query_row(
        "SELECT content, tool_result FROM ros_messages WHERE id = ?1",
        params_from_iter([Value::Text(id)]),
        |row| {
            Ok(LoadedTarget {
                content: row.get(0)?,
                tool_result: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(SqliteError::query("load message"))
}

fn load_text(
    conn: &rusqlite::Connection,
    table: &str,
    id: &str,
    uuid: bool,
) -> Result<Option<LoadedTarget>, SqliteError> {
    let column = if uuid { "id" } else { "slug" };
    let content = if uuid { "content" } else { "search_text" };
    let id = if uuid { uuid_text(id)? } else { id.to_owned() };
    let sql = format!("SELECT {content} FROM {table} WHERE {column} = ?1");
    conn.query_row(&sql, params_from_iter([Value::Text(id)]), |row| {
        Ok(LoadedTarget {
            content: row.get(0)?,
            tool_result: None,
        })
    })
    .optional()
    .map_err(SqliteError::query("load embed target"))
}

fn require_model(
    conn: &rusqlite::Connection,
    model: &str,
    width: usize,
) -> Result<(), SqliteError> {
    match meta::read(conn, MetaKey::EmbedModel)? {
        Some(stored) if stored == model => Ok(()),
        Some(stored) => Err(SqliteError::EmbedModel {
            stored,
            configured: model.to_owned(),
        }),
        None => {
            meta::record_absent(conn, MetaKey::EmbedModel, model)?;
            meta::record_absent(conn, MetaKey::EmbedDims, &width.to_string())
        }
    }
}

pub(crate) fn require_width(conn: &rusqlite::Connection, got: usize) -> Result<(), SqliteError> {
    let Some(expected) = recorded_width(conn)? else {
        return Ok(());
    };
    same_width(got, expected)
}

pub(crate) fn require_chunk_width(
    conn: &rusqlite::Connection,
    message_id: Option<&str>,
    got: usize,
) -> Result<(), SqliteError> {
    let Some(expected) = established_width(conn, message_id)? else {
        return Ok(());
    };
    same_width(got, expected)
}

fn established_width(
    conn: &rusqlite::Connection,
    message_id: Option<&str>,
) -> Result<Option<usize>, SqliteError> {
    if let Some(message_id) = message_id
        && let Some(width) = parent_width(conn, message_id)?
    {
        return Ok(Some(width));
    }
    if let Some(width) = recorded_width(conn)? {
        return Ok(Some(width));
    }
    stored_sample(conn)
}

fn stored_sample(conn: &rusqlite::Connection) -> Result<Option<usize>, SqliteError> {
    if let Some(width) = sample_width(
        conn,
        "SELECT length(embedding) FROM ros_messages WHERE embedding IS NOT NULL LIMIT 1",
    )? {
        return Ok(Some(width));
    }
    if let Some(width) = sample_width(
        conn,
        "SELECT length(embedding) FROM ros_summaries WHERE embedding IS NOT NULL LIMIT 1",
    )? {
        return Ok(Some(width));
    }
    if let Some(width) = sample_width(
        conn,
        "SELECT length(embedding) FROM rh_message_chunks WHERE embedding IS NOT NULL LIMIT 1",
    )? {
        return Ok(Some(width));
    }
    sample_width(
        conn,
        "SELECT length(embedding) FROM ros_wiki_topics WHERE embedding IS NOT NULL LIMIT 1",
    )
}

fn parent_width(
    conn: &rusqlite::Connection,
    message_id: &str,
) -> Result<Option<usize>, SqliteError> {
    let length: Option<i64> = conn
        .query_row(
            "SELECT length(embedding) FROM ros_messages WHERE id = ?1 AND embedding IS NOT NULL",
            [message_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(SqliteError::query("embed width"))?;
    width_of(length)
}

fn sample_width(conn: &rusqlite::Connection, sql: &str) -> Result<Option<usize>, SqliteError> {
    let length: Option<i64> = conn
        .query_row(sql, [], |row| row.get(0))
        .optional()
        .map_err(SqliteError::query("embed width"))?;
    width_of(length)
}

fn recorded_width(conn: &rusqlite::Connection) -> Result<Option<usize>, SqliteError> {
    let Some(expected) = meta::read(conn, MetaKey::EmbedDims)? else {
        return Ok(None);
    };
    expected
        .parse::<usize>()
        .map(Some)
        .map_err(|source| SqliteError::Query {
            operation: "embed width",
            source: rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(source),
            ),
        })
}

fn width_of(length: Option<i64>) -> Result<Option<usize>, SqliteError> {
    let Some(length) = length else {
        return Ok(None);
    };
    let Ok(length) = usize::try_from(length) else {
        return Err(SqliteError::StoredEmbedding);
    };
    if length == 0 {
        return Ok(None);
    }
    if length % 4 != 0 {
        return Err(SqliteError::StoredEmbedding);
    }
    Ok(Some(length / 4))
}

fn same_width(got: usize, expected: usize) -> Result<(), SqliteError> {
    if got == expected {
        Ok(())
    } else {
        Err(SqliteError::Width { got, expected })
    }
}

fn exec_id(
    conn: &rusqlite::Connection,
    sql: &str,
    id: &str,
    bytes: Option<Vec<u8>>,
) -> Result<usize, SqliteError> {
    conn.execute(
        sql,
        params_from_iter([Value::Text(uuid_text(id)?), blob_value(bytes)]),
    )
    .map_err(SqliteError::query("write embedding"))
}

fn exec_slug(
    conn: &rusqlite::Connection,
    sql: &str,
    slug: &str,
    bytes: Option<Vec<u8>>,
) -> Result<usize, SqliteError> {
    conn.execute(
        sql,
        params_from_iter([Value::Text(slug.to_owned()), blob_value(bytes)]),
    )
    .map_err(SqliteError::query("write embedding"))
}

fn identity(target: &EmbedTarget) -> Result<String, SqliteError> {
    match target.table {
        EmbedTable::Messages | EmbedTable::Summaries | EmbedTable::MessageChunks => {
            uuid_text(&target.id)
        }
        EmbedTable::WikiTopics => Ok(target.id.clone()),
    }
}

fn table_name(table: EmbedTable) -> String {
    table.to_string()
}

fn one_row(updated: usize, id: &str) -> Result<(), SqliteError> {
    if updated == 1 {
        Ok(())
    } else {
        Err(SqliteError::MissingHit { id: id.to_owned() })
    }
}

fn uuid_text(id: &str) -> Result<String, SqliteError> {
    Uuid::parse_str(id)
        .map(|id| id.to_string())
        .map_err(|source| SqliteError::Query {
            operation: "embed target id",
            source: rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                Box::new(source),
            ),
        })
}

fn blob_value(bytes: Option<Vec<u8>>) -> Value {
    match bytes {
        Some(bytes) => Value::Blob(bytes),
        None => Value::Null,
    }
}

fn embed_err(error: SqliteError) -> EmbeddingStoreError {
    if let SqliteError::SummaryReference(error) = error {
        return error.into();
    }
    if let SqliteError::Width { got, expected } = error {
        return EmbeddingStoreError::Width { got, expected };
    }
    if let SqliteError::MissingHit { id } = error {
        return EmbeddingStoreError::Missing { id };
    }
    if let SqliteError::EmbedModel { stored, configured } = error {
        return EmbeddingStoreError::Model { stored, configured };
    }
    EmbeddingStoreError::Query {
        message: error.to_string(),
        source: Box::new(error),
    }
}

fn guarded<T>(
    conn: &mut rusqlite::Connection,
    target: &EmbedTarget,
    write: impl FnOnce(&rusqlite::Transaction<'_>) -> Result<T, SqliteError>,
) -> Result<T, SqliteError> {
    let tx = crate::summary_replace::immediate(conn).map_err(SqliteError::query("embed begin"))?;
    guard_summary(&tx, target, None)?;
    let result = write(&tx)?;
    tx.commit().map_err(SqliteError::query("embed commit"))?;
    Ok(result)
}

fn guard_summary(
    tx: &rusqlite::Transaction<'_>,
    target: &EmbedTarget,
    hash: Option<&str>,
) -> Result<(), SqliteError> {
    if target.table != EmbedTable::Summaries {
        return Ok(());
    }
    let id = Uuid::parse_str(&target.id).map_err(|_| SqliteError::StoredValue {
        column: "summary ID",
        value: target.id.clone(),
    })?;
    let input = crate::summary_replace::shared(tx, id).map_err(|error| match error {
        crate::summary_replace::FenceError::Reference(error) => {
            SqliteError::SummaryReference(error)
        }
        crate::summary_replace::FenceError::Query(error) => {
            SqliteError::query("embed summary")(error)
        }
    })?;
    if input.input_key.is_some()
        || hash.is_some_and(|hash| {
            kernel::memory::summary_reference::embedding_input_hash(&input.content) != hash
        })
    {
        return Err(SummaryReferenceError::Stale(id).into());
    }
    Ok(())
}
