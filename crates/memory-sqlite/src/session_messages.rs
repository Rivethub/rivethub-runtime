use kernel::filing::stored_keys;
use kernel::memory::summary_order::{MessagePosition, order_messages};
use kernel::memory::{
    MemoryBrowseOrder, MemoryEntry, MessageCursor, MessagesQuery, SessionMessages,
};
use rusqlite::{Params, params};
use uuid::Uuid;
use wire::JsonObject;
use wire::message::Role;
use wire::session_id::SessionKey;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::message::{ENTRY_COLUMNS, EntryRow, stored_role, transcript_metadata};
use crate::time;

pub(crate) const POSITION_COLUMNS: &str =
    "m.id, m.conversation_id, m.role, m.metadata, m.created_at";

const SESSION: &str = " FROM ros_messages m JOIN ros_conversations c ON c.id = m.conversation_id
     WHERE c.task_id IS NULL AND c.session_key IN (SELECT value FROM json_each(?1))
     ORDER BY m.created_at, m.id";

impl SqliteStore {
    pub(crate) async fn session_messages(
        &self,
        key: &SessionKey,
        query: &MessagesQuery,
    ) -> Result<SessionMessages, SqliteError> {
        let keys = encode(&stored_keys(key.as_str()), "session keys")?;
        let query = query.clone();
        self.with_conn(move |conn| {
            let operation = "session messages";
            let tx = conn.transaction().map_err(SqliteError::query(operation))?;
            let sql = format!("SELECT {POSITION_COLUMNS}{SESSION}");
            let cursors =
                ordered_cursors(&tx, &sql, params![keys], MemoryBrowseOrder::Asc, operation)?;
            let total = u64::try_from(cursors.len()).map_err(SqliteError::Count)?;
            let page = query.page_after(cursors, |(_, cursor)| *cursor);
            let ids: Vec<_> = page.iter().map(|(id, _)| id.as_str()).collect();
            let entries = read_ordered_entries(&tx, &ids, operation)?;
            tx.commit().map_err(SqliteError::query(operation))?;
            let entries = entries
                .into_iter()
                .zip(page.into_iter().map(|(_, cursor)| cursor));
            Ok(SessionMessages::page(entries.collect(), query.limit, total))
        })
        .await
    }
}

pub(crate) fn read_ordered_entries(
    conn: &rusqlite::Connection,
    ids: &[&str],
    operation: &'static str,
) -> Result<Vec<MemoryEntry>, SqliteError> {
    let ids = encode(&ids, "message ids")?;
    let rows = conn
        .prepare(&format!(
            "SELECT {ENTRY_COLUMNS} FROM json_each(?1) picked
         JOIN ros_messages m ON m.id = picked.value
         JOIN ros_conversations c ON c.id = m.conversation_id ORDER BY picked.key"
        ))
        .and_then(|mut statement| {
            statement
                .query_map([ids], EntryRow::read)?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(SqliteError::query(operation))?;
    let mut seen = std::collections::HashMap::new();
    rows.into_iter()
        .map(|row| row.into_entry(&mut seen))
        .collect()
}

fn encode<T: serde::Serialize>(value: &T, column: &'static str) -> Result<String, SqliteError> {
    serde_json::to_string(value).map_err(|source| SqliteError::StoredJson { column, source })
}

#[derive(Debug)]
struct PositionRow {
    id: String,
    cursor: MessageCursor,
    conversation_id: Uuid,
    role: Role,
    metadata: Option<JsonObject>,
}

pub(crate) fn ordered_cursors(
    conn: &rusqlite::Connection,
    sql: &str,
    bindings: impl Params,
    direction: MemoryBrowseOrder,
    operation: &'static str,
) -> Result<Vec<(String, MessageCursor)>, SqliteError> {
    let rows = conn
        .prepare(sql)
        .and_then(|mut statement| {
            statement
                .query_map(bindings, |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        stored_role(row, 2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ))
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(SqliteError::query(operation))?;
    let rows = rows
        .into_iter()
        .map(|(id, conversation_id, role, metadata, stamp)| {
            Ok(PositionRow {
                cursor: MessageCursor {
                    created_at: time::stored(&stamp, "created_at")?,
                    id: parse_id(&id, "ros_messages.id")?,
                },
                id,
                conversation_id: parse_id(&conversation_id, "ros_messages.conversation_id")?,
                role,
                metadata: transcript_metadata(&metadata),
            })
        })
        .collect::<Result<Vec<_>, SqliteError>>()?;
    Ok(order_messages(rows, direction, |row| {
        MessagePosition::new(row.cursor.created_at, row.role, row.metadata.as_ref())
            .in_conversation(row.conversation_id)
    })
    .into_iter()
    .map(|row| (row.id, row.cursor))
    .collect())
}

fn parse_id(value: &str, column: &'static str) -> Result<Uuid, SqliteError> {
    value.parse().map_err(|_| SqliteError::StoredValue {
        column,
        value: value.to_owned(),
    })
}
