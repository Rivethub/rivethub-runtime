use kernel::filing::stored_keys;
use kernel::memory::summary_order::{MessagePosition, order_messages};
use kernel::memory::{
    MemoryBrowseOrder, MemoryEntry, MessageCursor, MessagesQuery, SessionMessages,
};
use rusqlite::params;
use uuid::Uuid;
use wire::JsonObject;
use wire::message::Role;
use wire::session_id::SessionKey;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::message::{ENTRY_COLUMNS, EntryRow, stored_role, transcript_metadata};
use crate::time;

const POSITION_COLUMNS: &str = "m.id, m.conversation_id, m.role, m.metadata, m.created_at";

const SESSION: &str = " FROM ros_messages m JOIN ros_conversations c ON c.id = m.conversation_id
     WHERE c.task_id IS NULL AND c.session_key IN (SELECT value FROM json_each(?1))";

const OPERATION: &str = "session messages";

impl SqliteStore {
    pub(crate) async fn session_messages(
        &self,
        key: &SessionKey,
        query: &MessagesQuery,
    ) -> Result<SessionMessages, SqliteError> {
        let keys = encode(&stored_keys(key.as_str()), "session keys")?;
        let query = query.clone();
        self.with_conn(move |conn| {
            let tx = conn.transaction().map_err(SqliteError::query(OPERATION))?;
            let page = if has_position(&tx, &keys)? {
                positioned_page(&tx, &keys, &query)?
            } else {
                keyset_page(&tx, &keys, &query)?
            };
            tx.commit().map_err(SqliteError::query(OPERATION))?;
            Ok(page)
        })
        .await
    }
}

fn has_position(conn: &rusqlite::Connection, keys: &str) -> Result<bool, SqliteError> {
    let sql = format!(
        "SELECT EXISTS (SELECT 1{SESSION} AND CASE
          WHEN instr(m.metadata, 'session_jsonl_line') = 0
           AND instr(m.metadata, 'session_store_position') = 0
           AND instr(m.metadata, 'hook_sequence') = 0
           AND instr(m.metadata, char(92)) = 0 THEN 0
          WHEN json_valid(m.metadata) THEN EXISTS (
            SELECT 1 FROM (
              SELECT m.metadata -> '$.session_jsonl_line' AS coordinate
              UNION ALL
              SELECT m.metadata -> '$.session_store_position'
              WHERE json_type(m.metadata, '$.session_sqlite_path') = 'text'
                AND m.metadata ->> '$.session_sqlite_path' <> ''
                AND json_type(m.metadata, '$.store_format') = 'text'
                AND m.metadata ->> '$.store_format' <> ''
              UNION ALL
              SELECT m.metadata -> '$.hook_sequence'
              WHERE json_type(m.metadata, '$.hook_producer') = 'text'
                AND m.metadata ->> '$.hook_producer' <> ''
                AND m.metadata -> '$.hook_sequence' <> '0'
            )
            WHERE coordinate GLOB '[0-9]*' AND coordinate NOT GLOB '*[^0-9]*'
              AND (length(coordinate) < 20
                OR (length(coordinate) = 20 AND coordinate <= '18446744073709551615'))
          )
         END)"
    );
    conn.query_row(&sql, [keys], |row| row.get(0))
        .map_err(SqliteError::query(OPERATION))
}

fn positioned_page(
    conn: &rusqlite::Connection,
    keys: &str,
    query: &MessagesQuery,
) -> Result<SessionMessages, SqliteError> {
    let ordered = ordered_cursors(conn, keys)?;
    let total = u64::try_from(ordered.len()).map_err(SqliteError::Count)?;
    let (start, page) = query.positioned_page(ordered.iter().map(|(_, cursor)| *cursor))?;
    let ids: Vec<_> = ordered
        .iter()
        .skip(start)
        .take(page.len())
        .map(|(id, _)| id)
        .collect();
    let ids = encode(&ids, "message ids")?;
    let sql = format!(
        "SELECT {ENTRY_COLUMNS} FROM json_each(?1) picked
         JOIN ros_messages m ON m.id = picked.value
         JOIN ros_conversations c ON c.id = m.conversation_id ORDER BY picked.key"
    );
    let entries = read_entries(conn, &sql, [ids])?;
    Ok(SessionMessages::page(
        entries.into_iter().zip(page).collect(),
        query.limit,
        total,
    ))
}

fn keyset_page(
    conn: &rusqlite::Connection,
    keys: &str,
    query: &MessagesQuery,
) -> Result<SessionMessages, SqliteError> {
    let sql = format!(
        "SELECT {ENTRY_COLUMNS}{SESSION}
         AND (?2 IS NULL OR m.created_at > ?2 OR (m.created_at = ?2 AND m.id > ?3))
         ORDER BY m.created_at, m.id LIMIT ?4"
    );
    let entries = read_entries(
        conn,
        &sql,
        params![
            keys,
            query.after.map(|cursor| time::stamp(cursor.created_at)),
            query.after.map(|cursor| cursor.id.to_string()),
            i64::from(query.limit) + 1,
        ],
    )?;
    let rows = entries
        .into_iter()
        .map(|entry| {
            let id = entry.id.as_deref().unwrap_or_default();
            let created_at = entry.created_at.ok_or_else(|| SqliteError::StoredValue {
                column: "ros_messages.created_at",
                value: id.to_owned(),
            })?;
            let cursor = MessageCursor::new(created_at, parse_id(id, "ros_messages.id")?);
            Ok((entry, cursor))
        })
        .collect::<Result<Vec<_>, SqliteError>>()?;
    let total: i64 = conn
        .query_row(&format!("SELECT count(*){SESSION}"), [keys], |row| {
            row.get(0)
        })
        .map_err(SqliteError::query(OPERATION))?;
    Ok(SessionMessages::page(
        rows,
        query.limit,
        u64::try_from(total).map_err(SqliteError::Count)?,
    ))
}

fn read_entries(
    conn: &rusqlite::Connection,
    sql: &str,
    bindings: impl rusqlite::Params,
) -> Result<Vec<MemoryEntry>, SqliteError> {
    let rows = conn
        .prepare(sql)
        .and_then(|mut statement| {
            statement
                .query_map(bindings, EntryRow::read)?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(SqliteError::query(OPERATION))?;
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

fn ordered_cursors(
    conn: &rusqlite::Connection,
    keys: &str,
) -> Result<Vec<(String, MessageCursor)>, SqliteError> {
    let sql = format!("SELECT {POSITION_COLUMNS}{SESSION}");
    let rows = conn
        .prepare(&sql)
        .and_then(|mut statement| {
            statement
                .query_map([keys], |row| {
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
        .map_err(SqliteError::query(OPERATION))?;
    let mut rows = rows
        .into_iter()
        .map(|(id, conversation_id, role, metadata, stamp)| {
            Ok(PositionRow {
                cursor: MessageCursor::new(
                    time::stored(&stamp, "created_at")?,
                    parse_id(&id, "ros_messages.id")?,
                ),
                id,
                conversation_id: parse_id(&conversation_id, "ros_messages.conversation_id")?,
                role,
                metadata: transcript_metadata(&metadata),
            })
        })
        .collect::<Result<Vec<_>, SqliteError>>()?;
    rows.sort_unstable_by_key(|row| (row.cursor.created_at, row.cursor.id));
    Ok(order_messages(rows, MemoryBrowseOrder::Asc, |row| {
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
