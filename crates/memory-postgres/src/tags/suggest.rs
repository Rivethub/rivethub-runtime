use jiff::Timestamp;
use kernel::memory::MemoryStoreError;
use kernel::memory::TagJobContext;
use kernel::memory::TagOnEntity;
use kernel::memory::TagSuggestKind;
use kernel::memory::TagSuggestionRow;
use kernel::memory::TagSuggestionStored;
use kernel::memory::TagSuggestionWrite;
use kernel::memory::TagVocabulary;
use kernel::memory::TagVocabularyProposal;
use sqlx::PgPool;
use sqlx::Postgres;
use sqlx::Transaction;
use uuid::Uuid;

use super::entity_type_of;
use super::state_of;
use super::store_error;

const SUMMARY_SQL: &str = "select id as summary_id, conversation_id, content as summary \
     from rh_summaries where id = $1";

const CONVERSATION_SQL: &str =
    "select session_key, title, agent from rh_conversations where id = $1";

const VOCABULARY_SQL: &str = "select key, value, display, aliases, state from ( \
        select key, value, display, aliases, state, created_at from rh_tag_taxonomy \
        union all \
        select t.key, t.value, max(t.display), '{}'::text[], 'accepted', min(t.created_at) \
          from rh_tags t \
         where t.state = 'accepted' \
           and not exists (select 1 from rh_tag_taxonomy x where x.key = t.key and x.value = t.value) \
         group by t.key, t.value \
     ) vocabulary order by created_at, key, value";

const TAGS_SQL: &str = "select entity_type, key, value, display, source, state from rh_tags \
     where (entity_type = 'summary' and entity_id = $1) \
        or (entity_type = 'conversation' and entity_id = $2) \
     order by created_at, key, value";

const INSERT_TAG_SQL: &str = "insert into rh_tags ( \
        entity_type, entity_id, key, value, display, source, state, \
        confidence, proposed_by, reason, created_at, updated_at \
     ) values ($1, $2, $3, $4, $5, 'model', 'suggested', $6, $7, coalesce($8, ''), $9, $9) \
     on conflict (entity_type, entity_id, key, value) do nothing";

const INSERT_VOCABULARY_SQL: &str = "insert into rh_tag_taxonomy ( \
        key, value, display, aliases, state, source, reason, created_at, updated_at \
     ) values ($1, $2, $3, $4, 'suggested', 'model', $5, $6, $6) \
     on conflict (key, value) do nothing";

const REMOVAL_SQL: &str = "update rh_tags set removal_state = 'suggested', \
     removal_reason = coalesce($1, ''), \
     updated_at = $2 \
     where entity_type = $3 and entity_id = $4 and key = $5 and value = $6 \
       and state = 'accepted' and removal_state is null";

#[derive(Debug, sqlx::FromRow)]
struct SummaryHead {
    summary_id: Uuid,
    conversation_id: Option<Uuid>,
    summary: String,
}

#[derive(Debug, sqlx::FromRow)]
struct ConversationHead {
    session_key: String,
    title: Option<String>,
    agent: String,
}

#[derive(Debug, sqlx::FromRow)]
struct VocabularyRow {
    key: String,
    value: String,
    display: String,
    aliases: Vec<String>,
    state: String,
}

#[derive(Debug, sqlx::FromRow)]
struct TagRow {
    entity_type: String,
    key: String,
    value: String,
    display: String,
    source: String,
    state: String,
}

pub(crate) async fn tag_job_context(
    pool: &PgPool,
    summary_id: Uuid,
) -> Result<Option<TagJobContext>, MemoryStoreError> {
    let Some(head) = load_summary(pool, summary_id).await? else {
        return Ok(None);
    };
    let conversation_id = head
        .conversation_id
        .ok_or_else(|| MemoryStoreError::InvalidRequest {
            message: format!("summary {summary_id} has no conversation"),
        })?;
    let conversation = load_conversation(pool, conversation_id).await?;
    let vocabulary = load_vocabulary(pool).await?;
    let tags = load_tags(pool, head.summary_id, conversation_id).await?;
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

pub(crate) async fn store_tag_suggestions(
    pool: &PgPool,
    write: &TagSuggestionWrite,
) -> Result<TagSuggestionStored, MemoryStoreError> {
    if write.rows.is_empty() && write.vocabulary.is_empty() {
        return Ok(TagSuggestionStored {
            tags: 0,
            removals: 0,
            vocabulary: 0,
        });
    }
    let mut tx = pool
        .begin()
        .await
        .map_err(|source| store_error("tag suggestions", source))?;
    let stored = write_all(&mut tx, write).await?;
    tx.commit()
        .await
        .map_err(|source| store_error("tag suggestions", source))?;
    Ok(stored)
}

async fn load_summary(
    pool: &PgPool,
    summary_id: Uuid,
) -> Result<Option<SummaryHead>, MemoryStoreError> {
    sqlx::query_as(SUMMARY_SQL)
        .bind(summary_id)
        .fetch_optional(pool)
        .await
        .map_err(|source| store_error("tag job context", source))
}

async fn load_conversation(
    pool: &PgPool,
    conversation_id: Uuid,
) -> Result<ConversationHead, MemoryStoreError> {
    sqlx::query_as(CONVERSATION_SQL)
        .bind(conversation_id)
        .fetch_optional(pool)
        .await
        .map_err(|source| store_error("tag job context", source))?
        .ok_or_else(|| MemoryStoreError::InvalidRequest {
            message: format!("conversation {conversation_id} is missing"),
        })
}

async fn load_vocabulary(pool: &PgPool) -> Result<Vec<TagVocabulary>, MemoryStoreError> {
    let rows: Vec<VocabularyRow> = sqlx::query_as(VOCABULARY_SQL)
        .fetch_all(pool)
        .await
        .map_err(|source| store_error("tag job context", source))?;
    rows.into_iter().map(vocabulary_of).collect()
}

async fn load_tags(
    pool: &PgPool,
    summary_id: Uuid,
    conversation_id: Uuid,
) -> Result<Vec<TagOnEntity>, MemoryStoreError> {
    let rows: Vec<TagRow> = sqlx::query_as(TAGS_SQL)
        .bind(summary_id)
        .bind(conversation_id)
        .fetch_all(pool)
        .await
        .map_err(|source| store_error("tag job context", source))?;
    rows.into_iter().map(tag_of).collect()
}

fn vocabulary_of(row: VocabularyRow) -> Result<TagVocabulary, MemoryStoreError> {
    Ok(TagVocabulary {
        key: row.key,
        value: row.value,
        display: row.display,
        aliases: row.aliases,
        state: state_of(&row.state)?,
    })
}

fn tag_of(row: TagRow) -> Result<TagOnEntity, MemoryStoreError> {
    Ok(TagOnEntity {
        entity_type: entity_type_of(&row.entity_type)?,
        key: row.key,
        value: row.value,
        display: row.display,
        source: row.source,
        state: state_of(&row.state)?,
    })
}

async fn write_all(
    tx: &mut Transaction<'_, Postgres>,
    write: &TagSuggestionWrite,
) -> Result<TagSuggestionStored, MemoryStoreError> {
    let mut tags = 0u64;
    let mut removals = 0u64;
    for row in &write.rows {
        let affected = apply_row(tx, row, &write.proposed_by, write.at).await?;
        match row.kind {
            TagSuggestKind::Add => tags = add_count(tags, affected)?,
            TagSuggestKind::Remove => removals = add_count(removals, affected)?,
        }
    }
    let mut vocabulary = 0u64;
    for proposal in &write.vocabulary {
        let affected = insert_vocabulary(tx, proposal, write.at).await?;
        vocabulary = add_count(vocabulary, affected)?;
    }
    Ok(TagSuggestionStored {
        tags,
        removals,
        vocabulary,
    })
}

async fn apply_row(
    tx: &mut Transaction<'_, Postgres>,
    row: &TagSuggestionRow,
    proposed_by: &str,
    at: Timestamp,
) -> Result<u64, MemoryStoreError> {
    match row.kind {
        TagSuggestKind::Add => insert_tag(tx, row, proposed_by, at).await,
        TagSuggestKind::Remove => suggest_removal(tx, row, at).await,
    }
}

async fn insert_tag(
    tx: &mut Transaction<'_, Postgres>,
    row: &TagSuggestionRow,
    proposed_by: &str,
    at: Timestamp,
) -> Result<u64, MemoryStoreError> {
    let result = sqlx::query(INSERT_TAG_SQL)
        .bind(row.entity_type.to_string())
        .bind(row.entity_id)
        .bind(&row.key)
        .bind(&row.value)
        .bind(&row.display)
        .bind(row.confidence)
        .bind(proposed_by)
        .bind(&row.reason)
        .bind(crate::time::BindTime::bind_time(at))
        .execute(&mut **tx)
        .await
        .map_err(|source| store_error("tag suggestions", source))?;
    Ok(result.rows_affected())
}

async fn suggest_removal(
    tx: &mut Transaction<'_, Postgres>,
    row: &TagSuggestionRow,
    at: Timestamp,
) -> Result<u64, MemoryStoreError> {
    let result = sqlx::query(REMOVAL_SQL)
        .bind(&row.reason)
        .bind(crate::time::BindTime::bind_time(at))
        .bind(row.entity_type.to_string())
        .bind(row.entity_id)
        .bind(&row.key)
        .bind(&row.value)
        .execute(&mut **tx)
        .await
        .map_err(|source| store_error("tag suggestions", source))?;
    Ok(result.rows_affected())
}

async fn insert_vocabulary(
    tx: &mut Transaction<'_, Postgres>,
    proposal: &TagVocabularyProposal,
    at: Timestamp,
) -> Result<u64, MemoryStoreError> {
    let aliases: Vec<String> = Vec::new();
    let result = sqlx::query(INSERT_VOCABULARY_SQL)
        .bind(&proposal.key)
        .bind(&proposal.value)
        .bind(&proposal.display)
        .bind(&aliases)
        .bind(&proposal.reason)
        .bind(crate::time::BindTime::bind_time(at))
        .execute(&mut **tx)
        .await
        .map_err(|source| store_error("tag suggestions", source))?;
    Ok(result.rows_affected())
}

fn add_count(total: u64, more: u64) -> Result<u64, MemoryStoreError> {
    total
        .checked_add(more)
        .ok_or_else(|| MemoryStoreError::InvalidRequest {
            message: "tag suggestion count overflowed".to_owned(),
        })
}
