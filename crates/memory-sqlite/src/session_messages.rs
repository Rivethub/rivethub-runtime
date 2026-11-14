use kernel::filing::stored_keys;
use kernel::memory::MemoryEntry;
use kernel::memory::MessageCursor;
use kernel::memory::MessagesQuery;
use kernel::memory::SessionMessages;
use rusqlite::params;
use wire::session_id::SessionKey;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::message::ENTRY_COLUMNS;
use crate::message::EntryRow;
use crate::time;

const SESSION: &str = "c.task_id IS NULL AND c.session_key IN (SELECT value FROM json_each(?1))";

impl SqliteStore {
    pub(crate) async fn session_messages(
        &self,
        key: &SessionKey,
        query: &MessagesQuery,
    ) -> Result<SessionMessages, SqliteError> {
        let keys = serde_json::to_string(&stored_keys(key.as_str())).map_err(|source| {
            SqliteError::StoredJson {
                column: "session keys",
                source,
            }
        })?;
        let after = query.after;
        let limit = query.limit;
        self.with_conn(move |conn| {
            let page = format!(
                "SELECT {ENTRY_COLUMNS} FROM ros_messages m
                   JOIN ros_conversations c ON c.id = m.conversation_id
                  WHERE {SESSION}
                    AND (?2 IS NULL OR m.created_at > ?2 OR (m.created_at = ?2 AND m.id > ?3))
                  ORDER BY m.created_at, m.id
                  LIMIT ?4"
            );
            let rows = conn
                .prepare(&page)
                .and_then(|mut statement| {
                    statement
                        .query_map(
                            params![
                                keys,
                                after.map(|cursor| time::stamp(cursor.created_at)),
                                after.map(|cursor| cursor.id.to_string()),
                                i64::from(limit) + 1,
                            ],
                            EntryRow::read,
                        )?
                        .collect::<Result<Vec<_>, _>>()
                })
                .map_err(SqliteError::query("session messages"))?;
            let total: i64 = conn
                .query_row(
                    &format!(
                        "SELECT count(*) FROM ros_messages m
                           JOIN ros_conversations c ON c.id = m.conversation_id WHERE {SESSION}"
                    ),
                    params![keys],
                    |row| row.get(0),
                )
                .map_err(SqliteError::query("session messages"))?;
            let mut seen = std::collections::HashMap::new();
            let mut entries = Vec::with_capacity(rows.len());
            for row in rows {
                let entry = row.into_entry(&mut seen)?;
                let cursor = cursor(&entry)?;
                entries.push((entry, cursor));
            }
            Ok(SessionMessages::page(
                entries,
                limit,
                u64::try_from(total).map_err(SqliteError::Count)?,
            ))
        })
        .await
    }
}

fn cursor(entry: &MemoryEntry) -> Result<MessageCursor, SqliteError> {
    let id = entry.id.clone().ok_or(SqliteError::StoredValue {
        column: "ros_messages.id",
        value: String::new(),
    })?;
    let parsed = uuid::Uuid::parse_str(&id).map_err(|_| SqliteError::StoredValue {
        column: "ros_messages.id",
        value: id.clone(),
    })?;
    let created_at = entry.created_at.ok_or(SqliteError::StoredValue {
        column: "ros_messages.created_at",
        value: id,
    })?;
    Ok(MessageCursor {
        created_at,
        id: parsed,
    })
}
