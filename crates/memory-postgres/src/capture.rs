use crate::error::Error;
use chrono::DateTime;
use chrono::Utc;
use kernel::memory::CaptureMessage;
use kernel::owner::Owner;
use sqlx::PgPool;
use sqlx::Transaction;
use sqlx::postgres::Postgres;
use uuid::Uuid;
use wire::JsonObject;
use wire::message::Role;

pub const MAX_CONTENT_CHARS: usize = 16_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureOptions {
    pub owner: Owner,
    pub node: Option<String>,
}

impl CaptureOptions {
    #[must_use]
    pub fn owned_by(owner: &Owner) -> Self {
        Self {
            owner: owner.clone(),
            node: None,
        }
    }
}

#[derive(Debug)]
pub struct CaptureWriter<'a> {
    pub(crate) tx: Transaction<'a, Postgres>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationUpsert<'a> {
    pub session_key: &'a str,
    pub agent: &'a str,
    pub channel: Option<&'a str>,
    pub title: Option<&'a str>,
    pub settings: Option<&'a JsonObject>,
    pub task_id: Option<&'a str>,
    pub owner_user_id: &'a str,
    pub node: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpsertedConversation {
    pub id: Uuid,
    pub owner_user_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationEnsure<'a> {
    pub session_key: &'a str,
    pub agent: &'a str,
    pub channel: Option<&'a str>,
    pub title: Option<&'a str>,
    pub settings: &'a JsonObject,
    pub task_id: Option<&'a str>,
    pub owner_user_id: &'a str,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationRecord {
    pub id: Uuid,
    pub created: bool,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationUpdate<'a> {
    pub id: Uuid,
    pub updated_at: Option<DateTime<Utc>>,
    pub active: bool,
    pub title: Option<&'a str>,
    pub settings: &'a JsonObject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageRow<'a> {
    pub conversation_id: Uuid,
    pub agent: &'a str,
    pub channel: &'a str,
    pub role: Role,
    pub content: &'a str,
    pub tool_name: Option<&'a str>,
    pub tool_args: Option<&'a JsonObject>,
    pub tool_result: Option<&'a str>,
    pub metadata: &'a JsonObject,
    pub owner_user_id: &'a str,
    pub created_at: Option<DateTime<Utc>>,
}

impl<'a> CaptureWriter<'a> {
    pub async fn begin(pool: &'a PgPool, session_key: &str) -> Result<Self, Error> {
        let mut tx = pool
            .begin()
            .await
            .map_err(|source| Error::operation("capture begin", source))?;
        let _locked = sqlx::query!(
            "select pg_advisory_xact_lock(hashtext($1)) is not null as locked",
            session_key
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(|source| Error::operation("capture lock", source))?;
        sqlx::query_scalar!(
            "select set_config('rivet.defer_embed_enqueue',
                     case when to_regclass('graphile_worker._private_jobs') is null
                          then 'on' else 'off' end, true) as mode"
        )
        .fetch_one(&mut *tx)
        .await
        .map_err(|source| Error::operation("embed defer", source))?;
        Ok(Self { tx })
    }

    pub async fn commit(self) -> Result<(), Error> {
        self.tx
            .commit()
            .await
            .map_err(|source| Error::operation("capture commit", source))
    }

    pub async fn backfill_task_id(&mut self, id: Uuid, task_id: &str) -> Result<(), Error> {
        sqlx::query!(
            "update rh_conversations set task_id = $2::text::uuid
              where id = $1 and task_id is null",
            id,
            task_id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("task backfill", source))?;
        Ok(())
    }

    pub async fn update_conversation(
        &mut self,
        update: &ConversationUpdate<'_>,
    ) -> Result<(), Error> {
        let settings = sqlx::types::Json(update.settings);
        sqlx::query!(
            "update rh_conversations
                set updated_at = coalesce($2, now()),
                    active = $3,
                    title = coalesce($4, title),
                    settings = $5
              where id = $1",
            update.id,
            update.updated_at,
            update.active,
            update.title,
            settings as _
        )
        .execute(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("conversation update", source))?;
        Ok(())
    }

    pub async fn touch_settings(&mut self, id: Uuid, settings: &JsonObject) -> Result<(), Error> {
        let json = sqlx::types::Json(settings);
        sqlx::query!(
            "update rh_conversations set updated_at = now(), settings = $2 where id = $1",
            id,
            json as _
        )
        .execute(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("conversation settings", source))?;
        Ok(())
    }

    pub async fn finalize_conversation(&mut self, id: Uuid) -> Result<(), Error> {
        sqlx::query!(
            "update rh_conversations set active = false, updated_at = now()
              where id = $1 and active = true",
            id
        )
        .execute(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("conversation finalize", source))?;
        Ok(())
    }

    pub async fn insert_message(&mut self, row: &MessageRow<'_>) -> Result<Uuid, Error> {
        let tool_args = row.tool_args.map(sqlx::types::Json);
        let metadata = sqlx::types::Json(row.metadata);
        let record = sqlx::query!(
            "insert into rh_messages
               (conversation_id, agent, channel, role, content, tool_name, tool_args,
                tool_result, metadata, owner_user_id, created_at)
             values ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, coalesce($11, now()))
             returning id",
            row.conversation_id,
            row.agent,
            row.channel,
            row.role.to_string(),
            row.content,
            row.tool_name,
            tool_args as _,
            row.tool_result,
            metadata as _,
            row.owner_user_id,
            row.created_at,
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("message insert", source))?;
        Ok(record.id)
    }

    pub async fn seen_event_ids(
        &mut self,
        conversation_id: Uuid,
        event_ids: &[String],
    ) -> Result<Vec<String>, Error> {
        if event_ids.is_empty() {
            return Ok(Vec::new());
        }
        let seen = sqlx::query_scalar!(
            "select seen.event_id
               from unnest($2::text[]) as wanted(event_id)
              cross join lateral (
                  select m.metadata->>'event_id' as event_id from rh_messages m
                   where m.conversation_id = $1 and m.metadata->>'event_id' = wanted.event_id
                   limit 1
              ) seen",
            conversation_id,
            event_ids
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("event id lookup", source))?;
        Ok(seen.into_iter().flatten().collect())
    }

    pub async fn stored_assistant_uuids(
        &mut self,
        conversation_id: Uuid,
    ) -> Result<Vec<String>, Error> {
        let stored = sqlx::query_scalar!(
            "select metadata->>'uuid' from rh_messages
              where conversation_id = $1 and role = 'assistant' and jsonb_exists(metadata, 'uuid')",
            conversation_id
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("uuid lookup", source))?;
        Ok(stored.into_iter().flatten().collect())
    }

    pub async fn stored_tool_calls(
        &mut self,
        conversation_id: Uuid,
    ) -> Result<Vec<(Option<String>, Option<String>)>, Error> {
        let rows = sqlx::query!(
            "select tool_name, tool_args::text as args from rh_messages
              where conversation_id = $1 and role = 'tool'",
            conversation_id
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("tool call lookup", source))?;
        Ok(rows
            .into_iter()
            .map(|row| (row.tool_name, row.args))
            .collect())
    }

    pub async fn stored_user_contents(
        &mut self,
        conversation_id: Uuid,
    ) -> Result<Vec<String>, Error> {
        sqlx::query_scalar!(
            "select content from rh_messages where conversation_id = $1 and role = 'user'",
            conversation_id
        )
        .fetch_all(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("user content lookup", source))
    }

    pub async fn ingest_key_seen(
        &mut self,
        conversation_id: Uuid,
        key: &str,
    ) -> Result<bool, Error> {
        let record = sqlx::query!(
            "select exists(select 1 from rh_messages
                where conversation_id = $1 and metadata->>'ingest_key' = $2) as \"seen!\"",
            conversation_id,
            key
        )
        .fetch_one(&mut *self.tx)
        .await
        .map_err(|source| Error::operation("ingest key lookup", source))?;
        Ok(record.seen)
    }
}

pub(crate) fn char_prefix(text: &str, max: usize) -> &str {
    text.char_indices()
        .nth(max)
        .map_or(text, |(end, _)| &text[..end])
}

fn capped(text: &str) -> Result<(String, Option<u64>), Error> {
    let text = text.replace('\0', "");
    let kept = char_prefix(&text, MAX_CONTENT_CHARS);
    let full = (kept.len() < text.len())
        .then(|| u64::try_from(text.chars().count()))
        .transpose()?;
    Ok((kept.to_owned(), full))
}

pub(crate) struct PreparedMessage {
    pub(crate) content: String,
    pub(crate) tool_result: Option<String>,
    pub(crate) metadata: JsonObject,
    pub(crate) created_at: Option<DateTime<Utc>>,
}

pub(crate) fn prepare_capture_message(message: &CaptureMessage) -> Result<PreparedMessage, Error> {
    let (content, full_content_length) = capped(&message.content)?;
    let (tool_result, full_tool_result_length) = match message.tool_result.as_deref() {
        Some(text) => {
            let (kept, full) = capped(text)?;
            (Some(kept), full)
        }
        None => (None, None),
    };
    let mut metadata = message.metadata.clone();
    let metadata = metadata.get_or_insert_default();
    metadata.insert("event_id", &message.event_id)?;
    if let Some(length) = full_content_length {
        metadata.insert("truncated", true)?;
        metadata.insert("full_content_length", length)?;
    }
    if let Some(length) = full_tool_result_length {
        metadata.insert("full_tool_result_length", length)?;
    }
    Ok(PreparedMessage {
        content,
        tool_result,
        metadata: metadata.clone(),
        created_at: message.created_at,
    })
}
