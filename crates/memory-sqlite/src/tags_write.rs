use kernel::memory::MemoryStoreError;
use kernel::memory::TagAdd;
use kernel::memory::TagDecision;
use kernel::memory::TagEntity;
use kernel::memory::TagEntityType;
use kernel::memory::TagId;
use kernel::memory::TagState;
use kernel::memory::TagWire;
use rusqlite::Connection;
use rusqlite::params_from_iter;
use rusqlite::types::Value;
use uuid::Uuid;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::tags::DISPLAY_CHARS;
use crate::tags::KEY_CHARS;
use crate::tags::TAG_COLUMNS;
use crate::tags::VALUE_CHARS;
use crate::tags::clip;
use crate::tags::colon_to_hyphen;
use crate::tags::invalid;
use crate::tags::read_raw;
use crate::tags::wire_tag;
use crate::time;

impl SqliteStore {
    pub(crate) async fn tags_decide(
        &self,
        ids: Vec<TagId>,
        decision: TagDecision,
        decided_by: Option<String>,
    ) -> Result<u64, MemoryStoreError> {
        let at = time::stamp(self.now());
        self.with_conn(move |conn| {
            let tx = crate::summary_replace::immediate(conn)
                .map_err(SqliteError::query("tags decide"))?;
            let changed = decide_tags(&tx, &ids, decision, decided_by.as_deref(), &at)?;
            tx.commit().map_err(SqliteError::query("tags decide"))?;
            Ok(changed)
        })
        .await
        .map_err(MemoryStoreError::from)
    }

    pub(crate) async fn tags_add(&self, add: TagAdd) -> Result<TagWire, MemoryStoreError> {
        let at = time::stamp(self.now());
        self.with_conn(move |conn| {
            let tx =
                crate::summary_replace::immediate(conn).map_err(SqliteError::query("tag add"))?;
            if let (TagEntityType::Summary, TagEntity::Id(id)) = (add.entity_type, &add.entity) {
                guard(&tx, *id)?;
            }
            let tag = add_tag(&tx, &add, &at)?;
            tx.commit().map_err(SqliteError::query("tag add"))?;
            Ok(tag)
        })
        .await
        .map_err(MemoryStoreError::from)
    }
}

fn decide_tags(
    conn: &Connection,
    ids: &[TagId],
    decision: TagDecision,
    decided_by: Option<&str>,
    at: &str,
) -> Result<u64, SqliteError> {
    let state = TagState::from(decision).to_string();
    let actor = actor(decided_by);
    let mut changed = 0u64;
    for id in ids {
        let updated = conn
            .execute(
                "UPDATE ros_tags \
                    SET state = ?2, \
                        source = CASE WHEN source = 'rule' AND ?2 = 'accepted' THEN 'user' ELSE source END, \
                        decided_by = ?3, decided_at = ?4, updated_at = ?4 \
                  WHERE id = ?1 AND state <> ?2
                  AND (entity_type <> 'summary' OR EXISTS (
                      SELECT 1 FROM ros_summaries s WHERE s.id = ros_tags.entity_id))",
                params_from_iter([
                    Value::Text(id.uuid().to_string()),
                    Value::Text(state.clone()),
                    Value::Text(actor.clone()),
                    Value::Text(at.to_owned()),
                ]),
            )
            .map_err(SqliteError::query("tags decide"))?;
        changed += u64::try_from(updated).map_err(SqliteError::Count)?;
    }
    Ok(changed)
}

fn add_tag(conn: &Connection, add: &TagAdd, at: &str) -> Result<TagWire, SqliteError> {
    let key = clip(&colon_to_hyphen(&add.tag.key), KEY_CHARS);
    let value = clip(&add.tag.value, VALUE_CHARS);
    let display = match add.display.as_deref().map(str::trim) {
        Some(text) if !text.is_empty() => clip(text, DISPLAY_CHARS),
        _ => clip(&add.tag.value, DISPLAY_CHARS),
    };
    if key.is_empty() || value.is_empty() {
        return Err(invalid("key and value are required"));
    }
    let entity = resolve_entity(conn, add.entity_type, &add.entity, add.agent.as_deref())?;
    let decided_by = actor(add.decided_by.as_deref());
    let reason = match &add.reason {
        Some(text) => text.clone(),
        None => String::new(),
    };
    conn.execute(
        "INSERT INTO ros_tags \
            (id, entity_type, entity_id, key, value, display, source, state, \
             proposed_by, reason, decided_by, decided_at, created_at, updated_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'user', 'accepted', 'owner', ?7, ?8, ?9, ?9, ?9) \
         ON CONFLICT (entity_type, entity_id, key, value) DO UPDATE SET \
            state = 'accepted', decided_by = excluded.decided_by, \
            decided_at = excluded.decided_at, updated_at = excluded.updated_at",
        params_from_iter([
            Value::Text(Uuid::new_v4().to_string()),
            Value::Text(add.entity_type.to_string()),
            Value::Text(entity.clone()),
            Value::Text(key.clone()),
            Value::Text(value.clone()),
            Value::Text(display),
            Value::Text(reason),
            Value::Text(decided_by),
            Value::Text(at.to_owned()),
        ]),
    )
    .map_err(SqliteError::query("tag add"))?;
    let raw = conn
        .query_row(
            &format!(
                "SELECT {TAG_COLUMNS} FROM ros_tags \
                  WHERE entity_type = ?1 AND entity_id = ?2 AND key = ?3 AND value = ?4"
            ),
            params_from_iter([
                Value::Text(add.entity_type.to_string()),
                Value::Text(entity),
                Value::Text(key),
                Value::Text(value),
            ]),
            read_raw,
        )
        .map_err(SqliteError::query("tag add"))?;
    wire_tag(raw)
}

fn resolve_entity(
    conn: &Connection,
    entity_type: TagEntityType,
    entity: &TagEntity,
    agent: Option<&str>,
) -> Result<String, SqliteError> {
    match entity {
        TagEntity::Id(id) => Ok(id.to_string()),
        TagEntity::SessionKey(key) => {
            if entity_type != TagEntityType::Conversation {
                return Err(invalid(
                    "a session key addresses a conversation, not a summary",
                ));
            }
            resolve_session(conn, key, agent)
        }
    }
}

fn resolve_session(
    conn: &Connection,
    key: &str,
    agent: Option<&str>,
) -> Result<String, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT id FROM ros_conversations \
              WHERE session_key = ?1 AND (?2 IS NULL OR agent = ?2) \
              ORDER BY id",
        )
        .map_err(SqliteError::query("session lookup"))?;
    let rows = statement
        .query_map(
            params_from_iter([
                Value::Text(key.to_owned()),
                match agent {
                    Some(name) => Value::Text(name.to_owned()),
                    None => Value::Null,
                },
            ]),
            |row| row.get::<_, String>(0),
        )
        .map_err(SqliteError::query("session lookup"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("session lookup"))?;
    match rows.len() {
        0 => Err(invalid(format!(
            "no conversation captured for session \"{key}\""
        ))),
        1 => match rows.first() {
            Some(id) => Ok(id.clone()),
            None => Err(invalid(format!(
                "no conversation captured for session \"{key}\""
            ))),
        },
        _ => Err(invalid(format!(
            "invalid request: session \"{key}\" exists under several agents; pass agent"
        ))),
    }
}

fn actor(decided_by: Option<&str>) -> String {
    match decided_by {
        Some(name) => name.to_owned(),
        None => "owner".to_owned(),
    }
}

pub(crate) fn guard(tx: &rusqlite::Transaction<'_>, id: Uuid) -> Result<(), SqliteError> {
    crate::summary_replace::shared(tx, id).map_err(|error| match error {
        crate::summary_replace::FenceError::Reference(error) => error.into(),
        crate::summary_replace::FenceError::Query(error) => SqliteError::query("tag source")(error),
    })?;
    Ok(())
}
