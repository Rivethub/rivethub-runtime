use std::collections::BTreeMap;
use std::str::FromStr as _;

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::Limit;
use kernel::memory::MemoryStoreError;
use kernel::memory::PendingTagWire;
use kernel::memory::TagCount;
use kernel::memory::TagCountsQuery;
use kernel::memory::TagEntityType;
use kernel::memory::TagListQuery;
use kernel::memory::TagLookup;
use kernel::memory::TagState;
use kernel::memory::TagWire;
use rusqlite::Connection;
use rusqlite::Row;
use rusqlite::params_from_iter;
use rusqlite::types::Value;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::time;

pub(crate) const VALUE_CHARS: usize = 128;
pub(crate) const DISPLAY_CHARS: usize = 200;
pub(crate) const KEY_CHARS: usize = 64;

pub(crate) const TAG_COLUMNS: &str = "id, entity_type, entity_id, key, value, display, source, state, \
     confidence, proposed_by, reason, decided_by, decided_at, created_at, updated_at";

pub(crate) struct RawTag {
    id: String,
    entity_type: String,
    entity_id: String,
    key: String,
    value: String,
    display: String,
    source: String,
    state: String,
    confidence: Option<f64>,
    proposed_by: String,
    reason: String,
    decided_by: Option<String>,
    decided_at: Option<String>,
    created_at: String,
    updated_at: String,
}

impl SqliteStore {
    pub(crate) async fn tags_list(
        &self,
        query: TagListQuery,
    ) -> Result<Vec<TagWire>, MemoryStoreError> {
        self.with_conn(move |conn| list_tags(conn, &query))
            .await
            .map_err(MemoryStoreError::from)
    }

    pub(crate) async fn tags_pending(
        &self,
        limit: Limit,
    ) -> Result<Vec<PendingTagWire>, MemoryStoreError> {
        self.with_conn(move |conn| pending_tags(conn, limit))
            .await
            .map_err(MemoryStoreError::from)
    }

    pub(crate) async fn tags_counts(
        &self,
        query: TagCountsQuery,
    ) -> Result<Vec<TagCount>, MemoryStoreError> {
        self.with_conn(move |conn| tag_counts(conn, &query))
            .await
            .map_err(MemoryStoreError::from)
    }

    pub(crate) async fn tags_lookup(
        &self,
        lookup: TagLookup,
    ) -> Result<BTreeMap<String, Vec<TagWire>>, MemoryStoreError> {
        self.with_conn(move |conn| lookup_tags(conn, &lookup))
            .await
            .map_err(MemoryStoreError::from)
    }
}

fn list_tags(conn: &Connection, query: &TagListQuery) -> Result<Vec<TagWire>, SqliteError> {
    let (sql, params) = list_query(query);
    let mut statement = conn
        .prepare(&sql)
        .map_err(SqliteError::query("tags list"))?;
    let rows = statement
        .query_map(params_from_iter(params.iter()), read_raw)
        .map_err(SqliteError::query("tags list"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("tags list"))?;
    rows.into_iter().map(wire_tag).collect()
}

fn list_query(query: &TagListQuery) -> (String, Vec<Value>) {
    let mut sql = format!("SELECT {TAG_COLUMNS} FROM ros_tags WHERE 1 = 1");
    let mut params = Vec::new();
    if let Some(entity_type) = query.entity_type {
        sql.push_str(" AND entity_type = ?");
        params.push(Value::Text(entity_type.to_string()));
    }
    if let Some(entity_id) = query.entity_id {
        sql.push_str(" AND entity_id = ?");
        params.push(Value::Text(entity_id.to_string()));
    }
    if let Some(key) = &query.key {
        sql.push_str(" AND key = ?");
        params.push(Value::Text(key.clone()));
    }
    if let Some(value) = &query.value {
        sql.push_str(" AND value = ?");
        params.push(Value::Text(value.clone()));
    }
    if !query.states.is_empty() {
        sql.push_str(" AND state IN (");
        for (index, state) in query.states.iter().enumerate() {
            if index > 0 {
                sql.push_str(", ");
            }
            sql.push('?');
            params.push(Value::Text(state.to_string()));
        }
        sql.push(')');
    }
    sql.push_str(" ORDER BY key, value, id LIMIT ?");
    params.push(Value::Integer(i64::from(query.limit.get())));
    (sql, params)
}

fn pending_tags(conn: &Connection, limit: Limit) -> Result<Vec<PendingTagWire>, SqliteError> {
    let columns = prefixed("t");
    let sql = format!(
        "SELECT {columns}, t.removal_state, c.session_key, c.title, c.agent, c.id, \
            (SELECT substr(m.content, 1, 120) FROM ros_messages m \
              WHERE m.conversation_id = c.id ORDER BY m.created_at, m.rowid LIMIT 1) \
         FROM ros_tags t \
         LEFT JOIN ros_conversations c \
           ON t.entity_type = 'conversation' AND c.id = t.entity_id \
         WHERE t.state = 'suggested' OR t.removal_state = 'suggested' \
         ORDER BY t.created_at DESC, t.id \
         LIMIT ?1"
    );
    let mut statement = conn
        .prepare(&sql)
        .map_err(SqliteError::query("tags pending"))?;
    let rows = statement
        .query_map(params_from_iter([i64::from(limit.get())]), read_pending)
        .map_err(SqliteError::query("tags pending"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("tags pending"))?;
    rows.into_iter()
        .map(|parts| {
            let tag = wire_tag(parts.raw)?;
            Ok(PendingTagWire {
                tag,
                action: parts
                    .removal
                    .then_some(kernel::memory::PendingTagAction::Remove),
                session_key: parts.session_key,
                title: parts.title,
                agent: parts.agent,
                conversation_id: parts.conversation_id,
                excerpt: parts.excerpt,
            })
        })
        .collect()
}

fn tag_counts(conn: &Connection, query: &TagCountsQuery) -> Result<Vec<TagCount>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT key, value, max(display), count(*) \
             FROM ros_tags \
             WHERE state = 'accepted' AND entity_type = 'conversation' \
               AND (?1 IS NULL OR key = ?1) \
             GROUP BY key, value \
             ORDER BY count(*) DESC, key, value \
             LIMIT ?2",
        )
        .map_err(SqliteError::query("tag counts"))?;
    let rows = statement
        .query_map(
            params_from_iter([
                query
                    .key
                    .as_ref()
                    .map_or(Value::Null, |key| Value::Text(key.clone())),
                Value::Integer(i64::from(query.limit.get())),
            ]),
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .map_err(SqliteError::query("tag counts"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("tag counts"))?;
    rows.into_iter()
        .map(|(key, value, display, conversations)| {
            Ok(TagCount {
                key,
                value,
                display,
                conversations: u64::try_from(conversations).map_err(SqliteError::Count)?,
            })
        })
        .collect()
}

fn lookup_tags(
    conn: &Connection,
    lookup: &TagLookup,
) -> Result<BTreeMap<String, Vec<TagWire>>, SqliteError> {
    if lookup.session_keys.is_empty() {
        return Ok(BTreeMap::new());
    }
    let columns = prefixed("t");
    let mut sql = format!(
        "SELECT c.session_key, {columns} \
         FROM ros_tags t \
         JOIN ros_conversations c ON c.id = t.entity_id \
         WHERE t.entity_type = 'conversation' AND c.session_key IN ("
    );
    let mut params = Vec::new();
    for (index, key) in lookup.session_keys.iter().enumerate() {
        if index > 0 {
            sql.push_str(", ");
        }
        sql.push('?');
        params.push(Value::Text(key.clone()));
    }
    sql.push(')');
    if !lookup.states.is_empty() {
        sql.push_str(" AND t.state IN (");
        for (index, state) in lookup.states.iter().enumerate() {
            if index > 0 {
                sql.push_str(", ");
            }
            sql.push('?');
            params.push(Value::Text(state.to_string()));
        }
        sql.push(')');
    }
    sql.push_str(
        " ORDER BY CASE t.state WHEN 'accepted' THEN 0 WHEN 'rejected' THEN 1 \
         WHEN 'suggested' THEN 2 ELSE 3 END, t.key, t.value, t.id",
    );
    let mut statement = conn
        .prepare(&sql)
        .map_err(SqliteError::query("tags lookup"))?;
    let rows = statement
        .query_map(params_from_iter(params.iter()), |row| {
            Ok((row.get::<_, String>(0)?, read_raw_at(row, 1)?))
        })
        .map_err(SqliteError::query("tags lookup"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("tags lookup"))?;
    let mut grouped: BTreeMap<String, Vec<TagWire>> = BTreeMap::new();
    for (session_key, raw) in rows {
        grouped.entry(session_key).or_default().push(wire_tag(raw)?);
    }
    Ok(grouped)
}

struct PendingParts {
    raw: RawTag,
    removal: bool,
    session_key: Option<String>,
    title: Option<String>,
    agent: Option<String>,
    conversation_id: Option<String>,
    excerpt: Option<String>,
}

fn read_pending(row: &Row<'_>) -> rusqlite::Result<PendingParts> {
    let raw = read_raw(row)?;
    let removal: Option<String> = row.get(15)?;
    Ok(PendingParts {
        raw,
        removal: removal.as_deref() == Some("suggested"),
        session_key: row.get(16)?,
        title: row.get(17)?,
        agent: row.get(18)?,
        conversation_id: row.get(19)?,
        excerpt: row.get(20)?,
    })
}

pub(crate) fn read_raw(row: &Row<'_>) -> rusqlite::Result<RawTag> {
    read_raw_at(row, 0)
}

fn read_raw_at(row: &Row<'_>, offset: usize) -> rusqlite::Result<RawTag> {
    Ok(RawTag {
        id: row.get(offset)?,
        entity_type: row.get(offset + 1)?,
        entity_id: row.get(offset + 2)?,
        key: row.get(offset + 3)?,
        value: row.get(offset + 4)?,
        display: row.get(offset + 5)?,
        source: row.get(offset + 6)?,
        state: row.get(offset + 7)?,
        confidence: row.get(offset + 8)?,
        proposed_by: row.get(offset + 9)?,
        reason: row.get(offset + 10)?,
        decided_by: row.get(offset + 11)?,
        decided_at: row.get(offset + 12)?,
        created_at: row.get(offset + 13)?,
        updated_at: row.get(offset + 14)?,
    })
}

pub(crate) fn wire_tag(raw: RawTag) -> Result<TagWire, SqliteError> {
    Ok(TagWire {
        id: raw.id,
        entity_type: entity_type_of(&raw.entity_type)?,
        entity_id: raw.entity_id,
        key: raw.key,
        value: raw.value,
        display: raw.display,
        source: raw.source,
        state: state_of(&raw.state)?,
        confidence: raw.confidence,
        proposed_by: raw.proposed_by,
        reason: raw.reason,
        decided_by: raw.decided_by,
        decided_at: optional_time(raw.decided_at)?,
        created_at: time::stored(&raw.created_at, "created_at")?,
        updated_at: time::stored(&raw.updated_at, "updated_at")?,
    })
}

fn optional_time(value: Option<String>) -> Result<Option<DateTime<Utc>>, SqliteError> {
    value
        .map(|text| time::stored(&text, "decided_at"))
        .transpose()
}

pub(crate) fn entity_type_of(raw: &str) -> Result<TagEntityType, SqliteError> {
    TagEntityType::from_str(raw).map_err(|_source| SqliteError::StoredTagType {
        value: raw.to_owned(),
    })
}

pub(crate) fn state_of(raw: &str) -> Result<TagState, SqliteError> {
    TagState::from_str(raw).map_err(|source| SqliteError::Query {
        operation: "tag state",
        source: rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(source),
        ),
    })
}

fn prefixed(alias: &str) -> String {
    TAG_COLUMNS
        .split(", ")
        .map(|column| format!("{alias}.{column}"))
        .collect::<Vec<_>>()
        .join(", ")
}

pub(crate) fn colon_to_hyphen(text: &str) -> String {
    text.chars()
        .map(|ch| if ch == ':' { '-' } else { ch })
        .collect()
}

pub(crate) fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

pub(crate) fn invalid(message: impl Into<String>) -> SqliteError {
    SqliteError::Invalid {
        message: message.into(),
    }
}
