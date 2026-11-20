use kernel::memory::MemoryStoreError;
use kernel::memory::TagJobContext;
use kernel::memory::TagOnEntity;
use kernel::memory::TagSuggestKind;
use kernel::memory::TagSuggestionRow;
use kernel::memory::TagSuggestionStored;
use kernel::memory::TagSuggestionWrite;
use kernel::memory::TagVocabulary;
use kernel::memory::TagVocabularyProposal;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
use rusqlite::params_from_iter;
use rusqlite::types::Value;
use uuid::Uuid;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::tags::entity_type_of;
use crate::tags::state_of;
use crate::time;

const SUMMARY_SQL: &str = "SELECT id, conversation_id, content FROM ros_summaries WHERE id = ?1";

const CONVERSATION_SQL: &str =
    "SELECT session_key, title, agent FROM ros_conversations WHERE id = ?1";

const VOCABULARY_SQL: &str = "SELECT key, value, display, aliases, state FROM ( \
        SELECT key, value, display, aliases, state, created_at FROM ros_tag_taxonomy \
        UNION ALL \
        SELECT t.key, t.value, max(t.display), '[]', 'accepted', min(t.created_at) \
          FROM ros_tags t \
         WHERE t.state = 'accepted' \
           AND NOT EXISTS (SELECT 1 FROM ros_tag_taxonomy x WHERE x.key = t.key AND x.value = t.value) \
         GROUP BY t.key, t.value \
     ) ORDER BY created_at, key, value";

const TAGS_SQL: &str = "SELECT entity_type, key, value, display, source, state FROM ros_tags \
     WHERE (entity_type = 'summary' AND entity_id = ?1) \
        OR (entity_type = 'conversation' AND entity_id = ?2) \
     ORDER BY created_at, key, value";

const INSERT_TAG_SQL: &str = "INSERT INTO ros_tags ( \
        id, entity_type, entity_id, key, value, display, source, state, \
        confidence, proposed_by, reason, created_at, updated_at \
     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'model', 'suggested', ?7, ?8, coalesce(?9, ''), ?10, ?10) \
     ON CONFLICT (entity_type, entity_id, key, value) DO NOTHING";

const INSERT_VOCABULARY_SQL: &str = "INSERT INTO ros_tag_taxonomy ( \
        key, value, display, aliases, state, source, reason, created_at, updated_at \
     ) VALUES (?1, ?2, ?3, '[]', 'suggested', 'model', ?4, ?5, ?5) \
     ON CONFLICT (key, value) DO NOTHING";

const REMOVAL_SQL: &str = "UPDATE ros_tags SET removal_state = 'suggested', \
     removal_reason = coalesce(?1, ''), \
     updated_at = ?2 \
     WHERE entity_type = ?3 AND entity_id = ?4 AND key = ?5 AND value = ?6 \
       AND state = 'accepted' AND removal_state IS NULL";

struct SummaryRaw {
    summary_id: String,
    conversation_id: Option<String>,
    summary: String,
}

struct SummaryHead {
    summary_id: Uuid,
    conversation_id: Option<Uuid>,
    summary: String,
}

struct ConversationRaw {
    session_key: String,
    title: Option<String>,
    agent: String,
}

struct VocabularyRaw {
    key: String,
    value: String,
    display: String,
    aliases: String,
    state: String,
}

struct TagRaw {
    entity_type: String,
    key: String,
    value: String,
    display: String,
    source: String,
    state: String,
}

impl SqliteStore {
    pub(crate) async fn read_tag_job_context(
        &self,
        summary_id: Uuid,
    ) -> Result<Option<TagJobContext>, MemoryStoreError> {
        self.with_conn(move |conn| load_context(conn, summary_id))
            .await
            .map_err(MemoryStoreError::from)
    }

    pub(crate) async fn write_tag_suggestions(
        &self,
        write: TagSuggestionWrite,
    ) -> Result<TagSuggestionStored, MemoryStoreError> {
        self.with_conn(move |conn| store_suggestions(conn, &write))
            .await
            .map_err(MemoryStoreError::from)
    }
}

fn load_context(conn: &Connection, summary_id: Uuid) -> Result<Option<TagJobContext>, SqliteError> {
    let Some(head) = load_summary(conn, summary_id)? else {
        return Ok(None);
    };
    let Some(conversation_id) = head.conversation_id else {
        return Err(invalid(format!(
            "summary {} has no conversation",
            head.summary_id
        )));
    };
    let conversation = load_conversation(conn, conversation_id)?;
    let vocabulary = load_vocabulary(conn)?;
    let tags = load_tags(conn, head.summary_id, conversation_id)?;
    Ok(Some(TagJobContext {
        summary_id: head.summary_id,
        conversation_id,
        session_key: conversation.session_key,
        title: conversation.title,
        agent: conversation.agent,
        summary: head.summary,
        vocabulary,
        tags,
    }))
}

fn load_summary(conn: &Connection, summary_id: Uuid) -> Result<Option<SummaryHead>, SqliteError> {
    let found = conn
        .query_row(
            SUMMARY_SQL,
            params_from_iter([Value::Text(summary_id.to_string())]),
            read_summary,
        )
        .optional()
        .map_err(|source| adapt("tag job context", source))?;
    found.map(summary_head).transpose()
}

fn load_conversation(
    conn: &Connection,
    conversation_id: Uuid,
) -> Result<ConversationRaw, SqliteError> {
    let found = conn
        .query_row(
            CONVERSATION_SQL,
            params_from_iter([Value::Text(conversation_id.to_string())]),
            read_conversation,
        )
        .optional()
        .map_err(|source| adapt("tag job context", source))?;
    found.ok_or_else(|| invalid(format!("conversation {conversation_id} is missing")))
}

fn load_vocabulary(conn: &Connection) -> Result<Vec<TagVocabulary>, SqliteError> {
    let mut statement = conn
        .prepare(VOCABULARY_SQL)
        .map_err(|source| adapt("tag job context", source))?;
    let rows = statement
        .query_map([], read_vocabulary)
        .map_err(|source| adapt("tag job context", source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| adapt("tag job context", source))?;
    rows.into_iter().map(vocabulary_of).collect()
}

fn load_tags(
    conn: &Connection,
    summary_id: Uuid,
    conversation_id: Uuid,
) -> Result<Vec<TagOnEntity>, SqliteError> {
    let mut statement = conn
        .prepare(TAGS_SQL)
        .map_err(|source| adapt("tag job context", source))?;
    let rows = statement
        .query_map(
            params_from_iter([
                Value::Text(summary_id.to_string()),
                Value::Text(conversation_id.to_string()),
            ]),
            read_tag,
        )
        .map_err(|source| adapt("tag job context", source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| adapt("tag job context", source))?;
    rows.into_iter().map(tag_of).collect()
}

fn store_suggestions(
    conn: &mut Connection,
    write: &TagSuggestionWrite,
) -> Result<TagSuggestionStored, SqliteError> {
    if write.rows.is_empty() && write.vocabulary.is_empty() {
        return Ok(TagSuggestionStored {
            tags: 0,
            removals: 0,
            vocabulary: 0,
        });
    }
    let at = time::stamp(write.at);
    let tx = conn
        .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
        .map_err(|source| adapt("tag suggestions", source))?;
    for row in &write.rows {
        if row.entity_type == kernel::memory::TagEntityType::Summary {
            crate::tags_write::guard(&tx, row.entity_id)?;
        }
    }
    let stored = apply_all(&tx, write, &at)?;
    tx.commit()
        .map_err(|source| adapt("tag suggestions", source))?;
    Ok(stored)
}

fn apply_all(
    conn: &Connection,
    write: &TagSuggestionWrite,
    at: &str,
) -> Result<TagSuggestionStored, SqliteError> {
    let mut tags = 0u64;
    let mut removals = 0u64;
    for row in &write.rows {
        let affected = apply_row(conn, row, &write.proposed_by, at)?;
        match row.kind {
            TagSuggestKind::Add => tags = add_count(tags, affected)?,
            TagSuggestKind::Remove => removals = add_count(removals, affected)?,
        }
    }
    let mut vocabulary = 0u64;
    for proposal in &write.vocabulary {
        let affected = insert_vocabulary(conn, proposal, at)?;
        vocabulary = add_count(vocabulary, affected)?;
    }
    Ok(TagSuggestionStored {
        tags,
        removals,
        vocabulary,
    })
}

fn apply_row(
    conn: &Connection,
    row: &TagSuggestionRow,
    proposed_by: &str,
    at: &str,
) -> Result<u64, SqliteError> {
    match row.kind {
        TagSuggestKind::Add => insert_tag(conn, row, proposed_by, at),
        TagSuggestKind::Remove => suggest_removal(conn, row, at),
    }
}

fn insert_tag(
    conn: &Connection,
    row: &TagSuggestionRow,
    proposed_by: &str,
    at: &str,
) -> Result<u64, SqliteError> {
    let inserted = conn
        .execute(
            INSERT_TAG_SQL,
            params_from_iter([
                Value::Text(Uuid::new_v4().to_string()),
                Value::Text(row.entity_type.to_string()),
                Value::Text(row.entity_id.to_string()),
                Value::Text(row.key.clone()),
                Value::Text(row.value.clone()),
                Value::Text(row.display.clone()),
                real_or_null(row.confidence),
                Value::Text(proposed_by.to_owned()),
                text_or_null(row.reason.as_deref()),
                Value::Text(at.to_owned()),
            ]),
        )
        .map_err(|source| adapt("tag suggestions", source))?;
    u64::try_from(inserted).map_err(SqliteError::Count)
}

fn suggest_removal(
    conn: &Connection,
    row: &TagSuggestionRow,
    at: &str,
) -> Result<u64, SqliteError> {
    let updated = conn
        .execute(
            REMOVAL_SQL,
            params_from_iter([
                text_or_null(row.reason.as_deref()),
                Value::Text(at.to_owned()),
                Value::Text(row.entity_type.to_string()),
                Value::Text(row.entity_id.to_string()),
                Value::Text(row.key.clone()),
                Value::Text(row.value.clone()),
            ]),
        )
        .map_err(|source| adapt("tag suggestions", source))?;
    u64::try_from(updated).map_err(SqliteError::Count)
}

fn insert_vocabulary(
    conn: &Connection,
    proposal: &TagVocabularyProposal,
    at: &str,
) -> Result<u64, SqliteError> {
    let inserted = conn
        .execute(
            INSERT_VOCABULARY_SQL,
            params_from_iter([
                Value::Text(proposal.key.clone()),
                Value::Text(proposal.value.clone()),
                Value::Text(proposal.display.clone()),
                Value::Text(proposal.reason.clone()),
                Value::Text(at.to_owned()),
            ]),
        )
        .map_err(|source| adapt("tag suggestions", source))?;
    u64::try_from(inserted).map_err(SqliteError::Count)
}

fn read_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<SummaryRaw> {
    Ok(SummaryRaw {
        summary_id: row.get(0)?,
        conversation_id: row.get(1)?,
        summary: row.get(2)?,
    })
}

fn read_conversation(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConversationRaw> {
    Ok(ConversationRaw {
        session_key: row.get(0)?,
        title: row.get(1)?,
        agent: row.get(2)?,
    })
}

fn read_vocabulary(row: &rusqlite::Row<'_>) -> rusqlite::Result<VocabularyRaw> {
    Ok(VocabularyRaw {
        key: row.get(0)?,
        value: row.get(1)?,
        display: row.get(2)?,
        aliases: row.get(3)?,
        state: row.get(4)?,
    })
}

fn read_tag(row: &rusqlite::Row<'_>) -> rusqlite::Result<TagRaw> {
    Ok(TagRaw {
        entity_type: row.get(0)?,
        key: row.get(1)?,
        value: row.get(2)?,
        display: row.get(3)?,
        source: row.get(4)?,
        state: row.get(5)?,
    })
}

fn summary_head(raw: SummaryRaw) -> Result<SummaryHead, SqliteError> {
    let conversation_id = match raw.conversation_id {
        Some(text) => Some(parse_id(&text)?),
        None => None,
    };
    Ok(SummaryHead {
        summary_id: parse_id(&raw.summary_id)?,
        conversation_id,
        summary: raw.summary,
    })
}

fn vocabulary_of(raw: VocabularyRaw) -> Result<TagVocabulary, SqliteError> {
    Ok(TagVocabulary {
        key: raw.key,
        value: raw.value,
        display: raw.display,
        aliases: aliases_of(&raw.aliases)?,
        state: state_of(&raw.state)?,
    })
}

fn tag_of(raw: TagRaw) -> Result<TagOnEntity, SqliteError> {
    Ok(TagOnEntity {
        entity_type: entity_type_of(&raw.entity_type)?,
        key: raw.key,
        value: raw.value,
        display: raw.display,
        source: raw.source,
        state: state_of(&raw.state)?,
    })
}

fn aliases_of(text: &str) -> Result<Vec<String>, SqliteError> {
    serde_json::from_str(text).map_err(|source| SqliteError::StoredJson {
        column: "aliases",
        source,
    })
}

fn parse_id(text: &str) -> Result<Uuid, SqliteError> {
    Uuid::parse_str(text).map_err(|source| SqliteError::Query {
        operation: "tag job context",
        source: rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(source),
        ),
    })
}

fn add_count(total: u64, more: u64) -> Result<u64, SqliteError> {
    total
        .checked_add(more)
        .ok_or_else(|| invalid("tag suggestion count overflowed"))
}

fn invalid(message: impl Into<String>) -> SqliteError {
    SqliteError::Invalid {
        message: message.into(),
    }
}

fn text_or_null(value: Option<&str>) -> Value {
    match value {
        Some(text) => Value::Text(text.to_owned()),
        None => Value::Null,
    }
}

fn real_or_null(value: Option<f64>) -> Value {
    match value {
        Some(number) => Value::Real(number),
        None => Value::Null,
    }
}

fn adapt(operation: &'static str, source: rusqlite::Error) -> SqliteError {
    if missing_relation(&source) {
        return SqliteError::TagTablesMissing;
    }
    if check_violation(&source) {
        return SqliteError::Invalid {
            message: source.to_string(),
        };
    }
    SqliteError::Query { operation, source }
}

fn missing_relation(error: &rusqlite::Error) -> bool {
    if let rusqlite::Error::SqliteFailure(_, Some(message)) = error {
        message.starts_with("no such table")
    } else {
        false
    }
}

fn check_violation(error: &rusqlite::Error) -> bool {
    if let rusqlite::Error::SqliteFailure(code, Some(message)) = error {
        code.code == rusqlite::ErrorCode::ConstraintViolation
            && message.contains("CHECK constraint")
    } else {
        false
    }
}
