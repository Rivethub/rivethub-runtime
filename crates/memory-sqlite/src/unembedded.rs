use async_trait::async_trait;
use kernel::memory::jobs::{MemoryJobError, UnembeddedStore};
use kernel::memory::{EmbedTable, EmbedTarget};

use crate::SqliteStore;
use crate::error::SqliteError;

const NULL_EMBEDDING: &str = "Embedding returned null";

struct Selection {
    id: &'static str,
    predicate: &'static str,
}

#[async_trait]
impl UnembeddedStore for SqliteStore {
    async fn unembedded(
        &self,
        table: EmbedTable,
        limit: u32,
    ) -> Result<Vec<EmbedTarget>, MemoryJobError> {
        let Some(selection) = selection(table) else {
            return self
                .with_conn(move |conn| pending_chunks(conn, limit))
                .await
                .map_err(|error| MemoryJobError::Query(Box::new(error)));
        };
        self.with_conn(move |conn| unembedded_rows(conn, table, &selection, limit))
            .await
            .map_err(|error| MemoryJobError::Query(Box::new(error)))
    }
}

fn selection(table: EmbedTable) -> Option<Selection> {
    match table {
        EmbedTable::Messages => Some(Selection {
            id: "id",
            predicate: "(length(content) > 20 OR length(tool_result) > 20)",
        }),
        EmbedTable::Summaries => Some(Selection {
            id: "id",
            predicate: "pipeline_version < 7 AND length(content) > 20",
        }),
        EmbedTable::WikiTopics => Some(Selection {
            id: "slug",
            predicate: "length(search_text) > 20",
        }),
        EmbedTable::MessageChunks => None,
    }
}

fn unembedded_rows(
    conn: &rusqlite::Connection,
    table: EmbedTable,
    selection: &Selection,
    limit: u32,
) -> Result<Vec<EmbedTarget>, SqliteError> {
    let Selection { id, predicate } = selection;
    conn.execute(
        &format!(
            "UPDATE {table} SET embed_status = NULL, embed_error = NULL, embed_failures = 0 \
              WHERE embedding IS NULL AND embed_status = 'failed' AND embed_error = ?1 \
                AND {predicate}"
        ),
        [NULL_EMBEDDING],
    )
    .map_err(SqliteError::query("heal null embeddings"))?;
    let mut statement = conn
        .prepare(&format!(
            "SELECT {id} FROM {table} \
              WHERE embedding IS NULL AND embed_status IS NULL AND {predicate} \
              ORDER BY created_at DESC LIMIT ?1"
        ))
        .map_err(SqliteError::query("select unembedded"))?;
    let ids = statement
        .query_map([i64::from(limit)], |row| row.get::<_, String>(0))
        .map_err(SqliteError::query("select unembedded"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("select unembedded"))?;
    Ok(ids
        .into_iter()
        .map(|id| EmbedTarget { table, id })
        .collect())
}

fn pending_chunks(
    conn: &rusqlite::Connection,
    limit: u32,
) -> Result<Vec<EmbedTarget>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT id FROM rh_message_chunks \
              WHERE embedding IS NULL AND embed_status IS NULL AND trim(content) <> '' \
              ORDER BY created_at DESC LIMIT ?1",
        )
        .map_err(SqliteError::query("select unembedded chunks"))?;
    let ids = statement
        .query_map([i64::from(limit)], |row| row.get::<_, String>(0))
        .map_err(SqliteError::query("select unembedded chunks"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("select unembedded chunks"))?;
    Ok(ids
        .into_iter()
        .map(|id| EmbedTarget {
            table: EmbedTable::MessageChunks,
            id,
        })
        .collect())
}
