use chrono::DateTime;
use chrono::Local;
use chrono::Utc;
use kernel::memory::BrowseFilter;
use kernel::memory::MemoryBrowseOrder;
use kernel::memory::MemoryEntry;
use kernel::memory_core::TimeRange;
use kernel::memory_core::resolve_window;
use rusqlite::OptionalExtension as _;
use rusqlite::params;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::message::ENTRY_COLUMNS;
use crate::message::EntryRow;
use crate::time;

const FULL_ENTRY_COLUMNS: &str = "m.id, c.session_key, m.agent, m.channel, m.role, m.content, \
     m.tool_name, m.tool_args, m.tool_result, \
     CASE WHEN json_extract(c.settings, '$.origin') IS NULL THEN m.metadata \
          ELSE json_set(coalesce(m.metadata, '{}'), '$.origin', \
                        json(json_extract(c.settings, '$.origin'))) END, \
     m.created_at";

const BROWSE_CONDITIONS: &str = "WHERE (?1 OR m.role <> 'tool')
   AND (?2 IS NULL OR m.conversation_id = ?2)
   AND (?3 IS NULL OR m.agent = ?3)
   AND (?4 IS NULL OR m.role = ?4)
   AND (?5 IS NULL OR m.tool_name = ?5)
   AND (?6 IS NULL OR m.conversation_id IN (
         SELECT entity_id FROM ros_tags
          WHERE entity_type = 'conversation' AND state = 'accepted'
            AND key = ?6 AND value = ?7))
   AND (?8 IS NULL OR m.created_at >= ?8)
   AND (?9 IS NULL OR m.created_at < ?9)
   AND NOT EXISTS (SELECT 1 FROM rh_session_trash tr WHERE tr.session_key = c.session_key)";

impl SqliteStore {
    pub async fn browse_at(
        &self,
        filter: BrowseFilter,
        now: DateTime<Utc>,
    ) -> Result<Vec<MemoryEntry>, SqliteError> {
        let bounds = match (filter.since, filter.before, filter.window) {
            (None, None, Some(window)) => resolve_window(window, now, &Local),
            (since, before, _) => TimeRange { since, before },
        };
        let direction = match filter.order {
            MemoryBrowseOrder::Asc => "ASC",
            MemoryBrowseOrder::Desc => "DESC",
        };
        let sql = format!(
            "SELECT {ENTRY_COLUMNS} FROM ros_messages m
               JOIN ros_conversations c ON c.id = m.conversation_id
             {BROWSE_CONDITIONS}
             ORDER BY m.created_at {direction}, m.rowid ASC
             LIMIT ?10"
        );
        self.with_conn(move |conn| {
            let mut statement = conn.prepare(&sql).map_err(SqliteError::query("browse"))?;
            let rows = statement
                .query_map(
                    params![
                        filter.include_tools,
                        filter.conversation_id.map(|id| id.to_string()),
                        filter.agent,
                        filter.role.map(|role| role.to_string()),
                        filter.tool_name,
                        filter.tag.as_ref().map(|tag| tag.key.as_str()),
                        filter.tag.as_ref().map(|tag| tag.value.as_str()),
                        bounds.since.map(time::stamp),
                        bounds.before.map(time::stamp),
                        filter.limit.get(),
                    ],
                    EntryRow::read,
                )
                .map_err(SqliteError::query("browse"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(SqliteError::query("browse"))?;
            rows.into_iter().map(MemoryEntry::try_from).collect()
        })
        .await
    }

    pub async fn get_full(&self, id: &str) -> Result<Option<MemoryEntry>, SqliteError> {
        self.message_entry(id).await
    }

    pub(crate) async fn message_entry(&self, id: &str) -> Result<Option<MemoryEntry>, SqliteError> {
        let id = id.to_owned();
        self.with_conn(move |conn| {
            conn.query_row(
                &format!(
                    "SELECT {FULL_ENTRY_COLUMNS} FROM ros_messages m
                       JOIN ros_conversations c ON c.id = m.conversation_id
                      WHERE m.id = ?1"
                ),
                [&id],
                EntryRow::read,
            )
            .optional()
            .map_err(SqliteError::query("get full"))?
            .map(MemoryEntry::try_from)
            .transpose()
        })
        .await
    }
}
