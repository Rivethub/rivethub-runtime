use jiff::Timestamp;
use kernel::memory::EmbedTable;
use kernel::memory::StoredChunk;
use rusqlite::OptionalExtension as _;
use rusqlite::params;
use uuid::Uuid;

use crate::error::SqliteError;
use crate::queue::enqueue_embed;
use crate::time;
use crate::vector;

pub(crate) const TABLE: &str = "rh_message_chunks";

pub(crate) fn replace(
    conn: &rusqlite::Connection,
    message_id: &str,
    chunks: &[StoredChunk],
    now: Timestamp,
) -> Result<(), SqliteError> {
    conn.execute(
        &format!("DELETE FROM {TABLE} WHERE message_id = ?1"),
        [message_id],
    )
    .map_err(SqliteError::query("clear message chunks"))?;
    for chunk in chunks {
        insert(conn, message_id, chunk, now)?;
    }
    Ok(())
}

pub(crate) fn insert(
    conn: &rusqlite::Connection,
    message_id: &str,
    chunk: &StoredChunk,
    now: Timestamp,
) -> Result<(), SqliteError> {
    if let Some(vector) = chunk.vector.as_deref() {
        crate::embed_store::require_chunk_width(conn, Some(message_id), vector.len())?;
    }
    let id = Uuid::new_v4().to_string();
    let bytes = chunk.vector.as_deref().map(vector::pack);
    conn.execute(
        &format!(
            "INSERT INTO {TABLE} \
                (id, message_id, idx, char_start, char_end, content, embedding, embed_failures, \
                 created_at, owner_user_id) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0, ?8, \
                     (SELECT owner_user_id FROM ros_messages WHERE id = ?2))"
        ),
        params![
            &id,
            message_id,
            i64::from(chunk.index),
            i64::from(chunk.char_start),
            i64::from(chunk.char_end),
            chunk.content,
            bytes,
            time::stamp(now),
        ],
    )
    .map_err(SqliteError::query("insert message chunk"))?;
    if chunk.vector.is_none() && !chunk.content.trim().is_empty() {
        enqueue_embed(conn, &EmbedTable::MessageChunks.to_string(), &id, now)?;
    }
    Ok(())
}

fn message_of(conn: &rusqlite::Connection, id: &str) -> Result<Option<String>, SqliteError> {
    conn.query_row(
        &format!("SELECT message_id FROM {TABLE} WHERE id = ?1"),
        [id],
        |row| row.get(0),
    )
    .optional()
    .map_err(SqliteError::query("load message chunk"))
}

pub(crate) fn load(conn: &rusqlite::Connection, id: &str) -> Result<Option<String>, SqliteError> {
    conn.query_row(
        &format!("SELECT content FROM {TABLE} WHERE id = ?1"),
        [id],
        |row| row.get(0),
    )
    .optional()
    .map_err(SqliteError::query("load message chunk"))
}

pub(crate) fn write_embedding(
    conn: &rusqlite::Connection,
    id: &str,
    bytes: &[u8],
) -> Result<usize, SqliteError> {
    let got = bytes.len() / 4;
    if bytes.len() != got * 4 {
        return Err(SqliteError::StoredEmbedding);
    }
    let message = message_of(conn, id)?;
    crate::embed_store::require_chunk_width(conn, message.as_deref(), got)?;
    conn.execute(
        &format!(
            "UPDATE {TABLE} SET embedding = ?2, embed_error = NULL, embed_status = 'done', \
             embed_failures = 0 WHERE id = ?1"
        ),
        params![id, bytes],
    )
    .map_err(SqliteError::query("write chunk embedding"))
}

pub(crate) fn mark(
    conn: &rusqlite::Connection,
    id: &str,
    status: &str,
    error: &str,
) -> Result<usize, SqliteError> {
    conn.execute(
        &format!(
            "UPDATE {TABLE} SET embed_status = ?2, embed_error = ?3, embedding = NULL WHERE id = ?1"
        ),
        params![id, status, error],
    )
    .map_err(SqliteError::query("mark chunk embedding"))
}

pub(crate) fn bump(
    conn: &rusqlite::Connection,
    id: &str,
    error: &str,
) -> Result<Option<i64>, SqliteError> {
    conn.query_row(
        &format!(
            "UPDATE {TABLE} SET embed_failures = embed_failures + 1, embed_error = ?2 \
             WHERE id = ?1 RETURNING embed_failures"
        ),
        params![id, error],
        |row| row.get(0),
    )
    .optional()
    .map_err(SqliteError::query("record null chunk embedding"))
}
