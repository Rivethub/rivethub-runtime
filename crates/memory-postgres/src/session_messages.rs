use chrono::{DateTime, Utc};
use kernel::filing::stored_keys;
use kernel::memory::summary_order::{MessagePosition, order_messages};
use kernel::memory::{
    MemoryBrowseOrder, MemoryEntry, MessageCursor, MessagesQuery, SessionMessages,
};
use sqlx::types::Json;
use sqlx::{PgConnection, Postgres, Transaction};
use uuid::Uuid;
use wire::JsonObject;
use wire::message::Role;
use wire::session_id::SessionKey;

use crate::PostgresMemory;
use crate::error::Error;
use crate::read::MessageEntryRow;

pub(crate) const POSITION_COLUMNS: &str = "m.id, m.conversation_id, to_json(m.role) as role,
     case when jsonb_typeof(m.metadata) = 'object' then m.metadata end as metadata, m.created_at";

const SESSION: &str = " from rh_messages m join rh_conversations c on c.id = m.conversation_id
     where c.task_id is null and c.session_key = any(";

const PAGE: &str = "select m.id::text as id, c.session_key, m.agent, m.channel, m.role as role_text,      m.content, m.tool_name, m.tool_args::text as tool_args, m.tool_result,      m.metadata::text as metadata, m.created_at,      case when c.session_key like 'import:%' then coalesce(c.settings::text, '{}') end as import_settings      from unnest($1::uuid[]) with ordinality picked(id, position)
     join rh_messages m on m.id = picked.id join rh_conversations c on c.id = m.conversation_id
     order by picked.position";

#[derive(sqlx::FromRow)]
pub(crate) struct PositionRow {
    id: Uuid,
    conversation_id: Uuid,
    role: Json<Role>,
    metadata: Option<Json<JsonObject>>,
    created_at: DateTime<Utc>,
}

impl PostgresMemory {
    pub(crate) async fn session_messages(
        &self,
        key: &SessionKey,
        query: &MessagesQuery,
    ) -> Result<SessionMessages, Error> {
        let keys = stored_keys(key.as_str());
        let operation = "session messages";
        let mut tx = self.begin_transcript_read(operation).await?;
        let mut statement = sqlx::QueryBuilder::new(format!("select {POSITION_COLUMNS}{SESSION}"));
        statement
            .push_bind(&keys)
            .push(") order by m.created_at, m.id");
        let rows = statement
            .build_query_as()
            .fetch_all(&mut *tx)
            .await
            .map_err(|source| Error::operation(operation, source))?;
        let total = u64::try_from(rows.len())?;
        let cursors = ordered_cursors(rows, MemoryBrowseOrder::Asc);
        let page = query.page_after(cursors, |cursor| *cursor);
        let ids: Vec<_> = page.iter().map(|cursor| cursor.id).collect();
        let entries = read_ordered_entries(&mut tx, &ids, operation).await?;
        tx.commit()
            .await
            .map_err(|source| Error::operation(operation, source))?;
        Ok(SessionMessages::page(
            entries.into_iter().zip(page).collect(),
            query.limit,
            total,
        ))
    }

    pub(crate) async fn begin_transcript_read(
        &self,
        operation: &'static str,
    ) -> Result<Transaction<'static, Postgres>, Error> {
        let failure = |source| Error::operation(operation, source);
        let mut tx = self.pool.begin().await.map_err(failure)?;
        sqlx::query("set transaction isolation level repeatable read, read only")
            .execute(&mut *tx)
            .await
            .map_err(failure)?;
        Ok(tx)
    }
}

pub(crate) async fn read_ordered_entries(
    conn: &mut PgConnection,
    ids: &[Uuid],
    operation: &'static str,
) -> Result<Vec<MemoryEntry>, Error> {
    let rows: Vec<MessageEntryRow> = sqlx::query_as(PAGE)
        .bind(ids)
        .fetch_all(conn)
        .await
        .map_err(|source| Error::operation(operation, source))?;
    let mut seen = std::collections::HashMap::new();
    rows.into_iter()
        .map(|row| row.into_entry(&mut seen))
        .collect()
}

pub(crate) fn ordered_cursors(
    rows: Vec<PositionRow>,
    direction: MemoryBrowseOrder,
) -> Vec<MessageCursor> {
    order_messages(rows, direction, |row| {
        MessagePosition::new(
            row.created_at,
            row.role.0,
            row.metadata.as_ref().map(|json| &json.0),
        )
        .in_conversation(row.conversation_id)
    })
    .into_iter()
    .map(|row| MessageCursor {
        created_at: row.created_at,
        id: row.id,
    })
    .collect()
}
