use chrono::{DateTime, Utc};
use kernel::filing::stored_keys;
use kernel::memory::summary_order::{MessagePosition, order_messages};
use kernel::memory::{
    MemoryBrowseOrder, MemoryEntry, MessageCursor, MessagesQuery, SessionMessages,
};
use sqlx::PgConnection;
use sqlx::types::Json;
use uuid::Uuid;
use wire::JsonObject;
use wire::message::Role;
use wire::session_id::SessionKey;

use crate::PostgresMemory;
use crate::error::Error;
use crate::read::MessageEntryRow;

const SESSION: &str = " from rh_messages m join rh_conversations c on c.id = m.conversation_id
     where c.task_id is null and c.session_key = any($1)";

const ENTRY_COLUMNS: &str = "m.id::text as id, c.session_key, m.agent, m.channel, m.role as role_text,
     m.content, m.tool_name, m.tool_args::text as tool_args, m.tool_result, m.metadata::text as metadata,
     case when c.session_key like 'import:%' then coalesce(c.settings::text, '{}') end as import_settings";

const SAFE_STAMP: &str = "case when m.created_at > '-infinity' and m.created_at <= $2 then m.created_at end as created_at";
const OPERATION: &str = "session messages";

#[derive(sqlx::FromRow)]
struct PositionRow {
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
        let mut tx = self.pool.begin().await.map_err(failure)?;
        sqlx::query("set transaction isolation level repeatable read, read only")
            .execute(&mut *tx)
            .await
            .map_err(failure)?;
        let page = if has_position(&mut tx, &keys).await? {
            positioned_page(&mut tx, &keys, query).await?
        } else {
            keyset_page(&mut tx, &keys, query).await?
        };
        tx.commit().await.map_err(failure)?;
        Ok(page)
    }
}

fn failure(source: sqlx::Error) -> Error {
    Error::operation(OPERATION, source)
}

async fn has_position(conn: &mut PgConnection, keys: &[String]) -> Result<bool, Error> {
    let sql = format!(
        "select exists (select 1{SESSION}
         and exists (
           select 1 from (values
             (m.metadata -> 'session_jsonl_line'),
             (case when jsonb_typeof(m.metadata -> 'session_sqlite_path') = 'string'
                    and m.metadata ->> 'session_sqlite_path' <> ''
                    and jsonb_typeof(m.metadata -> 'store_format') = 'string'
                    and m.metadata ->> 'store_format' <> ''
                   then m.metadata -> 'session_store_position' end),
             (case when jsonb_typeof(m.metadata -> 'hook_producer') = 'string'
                    and m.metadata ->> 'hook_producer' <> ''
                    and m.metadata -> 'hook_sequence' <> '0'::jsonb
                   then m.metadata -> 'hook_sequence' end)
           ) evidence(coordinate)
           where coordinate::text ~ '^[0-9]+$' and coordinate <= '18446744073709551615'::jsonb
         ))"
    );
    sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(keys)
        .fetch_one(conn)
        .await
        .map_err(failure)
}

async fn positioned_page(
    conn: &mut PgConnection,
    keys: &[String],
    query: &MessagesQuery,
) -> Result<SessionMessages, Error> {
    let sql = format!(
        "select m.id, m.conversation_id, to_json(m.role) as role,
         case when jsonb_typeof(m.metadata) = 'object' then m.metadata end as metadata,
         {SAFE_STAMP}{SESSION} order by m.created_at, m.id"
    );
    let rows: Vec<_> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(keys)
        .bind(DateTime::<Utc>::MAX_UTC)
        .fetch_all(&mut *conn)
        .await
        .map_err(failure)?;
    let total = u64::try_from(rows.len())?;
    let cursors = ordered_cursors(rows);
    let (_, page) = query.positioned_page(cursors.into_iter())?;
    let ids: Vec<_> = page.iter().map(|cursor| cursor.id).collect();
    let sql = format!(
        "select {ENTRY_COLUMNS}, m.created_at from unnest($1::uuid[]) with ordinality picked(id, position)
         join rh_messages m on m.id = picked.id join rh_conversations c on c.id = m.conversation_id
         order by picked.position"
    );
    let rows = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(ids)
        .fetch_all(conn)
        .await
        .map_err(failure)?;
    let entries = into_entries(rows)?;
    Ok(SessionMessages::page(
        entries.into_iter().zip(page).collect(),
        query.limit,
        total,
    ))
}

async fn keyset_page(
    conn: &mut PgConnection,
    keys: &[String],
    query: &MessagesQuery,
) -> Result<SessionMessages, Error> {
    let sql = format!(
        "select {ENTRY_COLUMNS}, {SAFE_STAMP}{SESSION}
         and ($3::timestamptz is null or (m.created_at, m.id) > ($3, $4::uuid))
         order by m.created_at, m.id limit $5"
    );
    let rows: Vec<_> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(keys)
        .bind(DateTime::<Utc>::MAX_UTC)
        .bind(query.after.map(|cursor| cursor.created_at))
        .bind(query.after.map(|cursor| cursor.id))
        .bind(i64::from(query.limit) + 1)
        .fetch_all(&mut *conn)
        .await
        .map_err(failure)?;
    let entries = into_entries(rows)?
        .into_iter()
        .map(|entry| {
            let id = entry
                .id
                .as_deref()
                .ok_or_else(|| Error::protocol("message row is missing its id"))?;
            let created_at = entry.created_at.ok_or_else(|| {
                Error::stored(
                    "created_at",
                    std::io::Error::other("message timestamp is outside the supported range"),
                )
            })?;
            let cursor = MessageCursor::new(created_at, Uuid::parse_str(id)?);
            Ok((entry, cursor))
        })
        .collect::<Result<Vec<_>, Error>>()?;
    let total: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!("select count(*){SESSION}")))
        .bind(keys)
        .fetch_one(conn)
        .await
        .map_err(failure)?;
    Ok(SessionMessages::page(
        entries,
        query.limit,
        u64::try_from(total)?,
    ))
}

fn into_entries(rows: Vec<MessageEntryRow>) -> Result<Vec<MemoryEntry>, Error> {
    let mut seen = std::collections::HashMap::new();
    rows.into_iter()
        .map(|row| row.into_entry(&mut seen))
        .collect()
}

fn ordered_cursors(rows: Vec<PositionRow>) -> Vec<MessageCursor> {
    order_messages(rows, MemoryBrowseOrder::Asc, |row| {
        MessagePosition::new(
            row.created_at,
            row.role.0,
            row.metadata.as_ref().map(|json| &json.0),
        )
        .in_conversation(row.conversation_id)
    })
    .into_iter()
    .map(|row| MessageCursor::new(row.created_at, row.id))
    .collect()
}
