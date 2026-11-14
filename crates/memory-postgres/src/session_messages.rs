use kernel::filing::stored_keys;
use kernel::memory::MessageCursor;
use kernel::memory::MessagesQuery;
use kernel::memory::SessionMessages;
use uuid::Uuid;
use wire::session_id::SessionKey;

use crate::PostgresMemory;
use crate::error::Error;
use crate::read::MessageEntryRow;

const PAGE: &str = "select m.id::text as id, c.session_key, m.agent, m.channel, m.role as role_text, \
     m.content, m.tool_name, m.tool_args::text as tool_args, m.tool_result, \
     m.metadata::text as metadata, m.created_at, \
     case when c.session_key like 'import:%' then coalesce(c.settings::text, '{}') end as import_settings \
     from rh_messages m join rh_conversations c on c.id = m.conversation_id \
     where c.task_id is null and c.session_key = any($1) \
       and ($2::timestamptz is null or (m.created_at, m.id) > ($2, $3::uuid)) \
     order by m.created_at, m.id limit $4";

const TOTAL: &str = "select count(*) from rh_messages m join rh_conversations c on c.id = m.conversation_id \
     where c.task_id is null and c.session_key = any($1)";

impl PostgresMemory {
    pub(crate) async fn session_messages(
        &self,
        key: &SessionKey,
        query: &MessagesQuery,
    ) -> Result<SessionMessages, Error> {
        let keys = stored_keys(key.as_str());
        let rows: Vec<MessageEntryRow> = sqlx::query_as(PAGE)
            .bind(&keys)
            .bind(query.after.map(|cursor| cursor.created_at))
            .bind(query.after.map(|cursor| cursor.id))
            .bind(i64::from(query.limit) + 1)
            .fetch_all(&self.pool)
            .await
            .map_err(|source| Error::operation("session messages", source))?;
        let total: i64 = sqlx::query_scalar(TOTAL)
            .bind(&keys)
            .fetch_one(&self.pool)
            .await
            .map_err(|source| Error::operation("session messages", source))?;
        let mut seen = std::collections::HashMap::new();
        let mut page = Vec::with_capacity(rows.len());
        for row in rows {
            let entry = row.into_entry(&mut seen)?;
            let cursor = cursor(&entry)?;
            page.push((entry, cursor));
        }
        Ok(SessionMessages::page(
            page,
            query.limit,
            u64::try_from(total)?,
        ))
    }
}

fn cursor(entry: &kernel::memory::MemoryEntry) -> Result<MessageCursor, Error> {
    let id = entry
        .id
        .as_deref()
        .ok_or_else(|| Error::protocol("message row is missing its id"))?;
    Ok(MessageCursor {
        created_at: entry
            .created_at
            .ok_or_else(|| Error::protocol("message row is missing its time"))?,
        id: Uuid::parse_str(id)?,
    })
}
