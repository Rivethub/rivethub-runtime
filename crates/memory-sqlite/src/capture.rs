use chrono::DateTime;
use chrono::Utc;
use kernel::memory::CaptureBatchRequest;
use kernel::memory::CaptureMessage;
use kernel::memory::CaptureResult;
use kernel::memory::spoken_title;
use kernel::owner::Owner;
use rusqlite::OptionalExtension as _;
use rusqlite::Transaction;
use rusqlite::params;
use serde::Serialize;
use wire::JsonObject;
use wire::message::Role;

use crate::SqliteStore;
use crate::conversation::ConversationKey;
use crate::conversation::NewConversation;
use crate::error::SqliteError;
use crate::message::NewMessage;
use crate::queue::enqueue_embed;
use crate::time;
use crate::time::Stamp;

pub const MAX_CONTENT_CHARS: usize = 16_000;

#[derive(Debug)]
struct PreparedMessage {
    event_id: String,
    role: Role,
    content: String,
    tool_name: Option<String>,
    tool_args: Option<String>,
    tool_result: Option<String>,
    metadata: String,
    created_at: Stamp,
}

#[derive(Debug)]
struct PreparedBatch {
    key: ConversationKey,
    channel: Option<String>,
    title: Option<String>,
    settings: Option<JsonObject>,
    imported: bool,
    task_id: Option<String>,
    node: Option<String>,
    finalize: bool,
    created_at: Stamp,
    updated_at: Stamp,
    messages: Vec<PreparedMessage>,
}

#[derive(Debug)]
struct Target<'a> {
    conversation_id: &'a str,
    agent: &'a str,
    channel: &'a str,
    owner: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Outcome {
    Inserted(String),
    Filled(String),
    Skipped,
}

impl SqliteStore {
    pub async fn capture(
        &self,
        batch: CaptureBatchRequest,
        owner: &Owner,
        node: Option<&str>,
        now: DateTime<Utc>,
    ) -> Result<CaptureResult, SqliteError> {
        let prepared = PreparedBatch::new(batch, owner, node)?;
        let owner = owner.clone();
        let embed = self.embed_armed();
        let result = self
            .with_conn(move |conn| {
                let tx = conn
                    .transaction()
                    .map_err(SqliteError::query("capture begin"))?;
                if !crate::deletion::admits(&tx, &prepared.key.session_key, prepared.stamps())? {
                    tracing::debug!(
                        session_key = %prepared.key.session_key,
                        "capture dropped for a deleted session"
                    );
                    return prepared.dropped();
                }
                let result = prepared.write(&tx, owner.as_str(), now, embed)?;
                tx.commit().map_err(SqliteError::query("capture commit"))?;
                Ok(result)
            })
            .await?;
        if embed {
            self.notifications.send_replace(());
        }
        Ok(result)
    }
}

impl PreparedBatch {
    fn new(
        batch: CaptureBatchRequest,
        owner: &Owner,
        node: Option<&str>,
    ) -> Result<Self, SqliteError> {
        let created_at = crate::time::Stamp::from(batch.created_at);
        let updated_at = crate::time::Stamp::from(batch.updated_at);
        let owned_key = wire::capture::owned_import_key(owner.as_str(), &batch.session_key)
            .map_err(|error| SqliteError::Invalid {
                message: error.to_string(),
            })?;
        let imported = owned_key.is_some();
        let messages = batch
            .messages
            .iter()
            .map(PreparedMessage::new)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self {
            key: ConversationKey {
                session_key: owned_key.unwrap_or(batch.session_key),
                agent: batch.agent,
            },
            channel: batch.channel,
            title: batch
                .title
                .as_deref()
                .and_then(spoken_title)
                .map(str::to_owned),
            settings: batch.settings,
            imported,
            task_id: batch.task_id,
            node: node.map(str::to_owned),
            finalize: batch.finalize == Some(true),
            created_at,
            updated_at,
            messages,
        })
    }

    fn stamps(&self) -> Vec<Option<DateTime<Utc>>> {
        self.messages
            .iter()
            .map(|message| match message.created_at {
                Stamp::Source(at) => Some(at),
                Stamp::CaptureTime => None,
            })
            .collect()
    }

    fn dropped(&self) -> Result<CaptureResult, SqliteError> {
        Ok(CaptureResult {
            ok: true,
            conversation_id: String::new(),
            inserted: 0,
            skipped: u64::try_from(self.messages.len()).map_err(SqliteError::Count)?,
        })
    }

    fn write(
        &self,
        tx: &Transaction<'_>,
        owner: &str,
        now: DateTime<Utc>,
        embed: bool,
    ) -> Result<CaptureResult, SqliteError> {
        let conversation = match self.key.find(tx)? {
            Some(found) => {
                self.update_conversation(tx, &found.id, found.updated_at, owner, now)?;
                found
            }
            None => NewConversation {
                key: &self.key,
                channel: self.channel.as_deref(),
                title: self.title.as_deref(),
                settings: self
                    .settings
                    .as_ref()
                    .map(|settings| encode(settings, "settings"))
                    .transpose()?
                    .as_deref(),
                task_id: self.task_id.as_deref(),
                owner,
                created_at: self.created_at.at(now),
                updated_at: self.updated_at.at(now),
            }
            .insert(tx)?,
        };
        if let Some(node) = &self.node {
            tx.execute(
                "INSERT INTO rh_conversation_nodes (conversation_id, node) VALUES (?1, ?2)
                 ON CONFLICT (conversation_id) DO NOTHING",
                params![conversation.id, node],
            )
            .map_err(SqliteError::query("conversation node"))?;
        }
        let channel = match &self.channel {
            Some(channel) => channel,
            None => &conversation.channel,
        };
        let target = Target {
            conversation_id: &conversation.id,
            agent: &self.key.agent,
            channel,
            owner: conversation.owner.as_deref().unwrap_or(owner),
        };
        let mut inserted: u64 = 0;
        let mut skipped: u64 = 0;
        for message in &self.messages {
            match message.write(tx, &target, now)? {
                Outcome::Inserted(id) => {
                    inserted += 1;
                    queue_inserted(tx, &id, embed, now)?;
                }
                Outcome::Filled(id) => {
                    skipped += 1;
                    queue_if_absent(tx, message, &id, embed, now)?;
                }
                Outcome::Skipped => skipped += 1,
            }
        }
        if self.finalize {
            tx.execute(
                "UPDATE ros_conversations SET active = 0 WHERE id = ?1 AND active = 1",
                [&conversation.id],
            )
            .map_err(SqliteError::query("conversation finalize"))?;
        }
        Ok(CaptureResult {
            ok: true,
            conversation_id: conversation.id,
            inserted,
            skipped,
        })
    }

    fn update_conversation(
        &self,
        tx: &Transaction<'_>,
        id: &str,
        stored_updated_at: DateTime<Utc>,
        owner: &str,
        now: DateTime<Utc>,
    ) -> Result<(), SqliteError> {
        let updated_at = self.updated_at.at(now).max(stored_updated_at);
        let (settings, title) = if self.imported {
            let stored = stored_settings(tx, id)?;
            let merged =
                wire::capture::merge_imported_settings(stored.as_ref(), self.settings.as_ref())
                    .map_err(|source| SqliteError::Encode {
                        column: "settings",
                        source,
                    })?;
            let title = self.title.as_ref().filter(|_| merged.incoming_is_newer);
            (Some(merged.settings), title)
        } else {
            (self.settings.clone(), self.title.as_ref())
        };
        let settings = settings
            .map(|settings| encode(&settings, "settings"))
            .transpose()?;
        tx.execute(
            "UPDATE ros_conversations
                SET updated_at = ?2,
                    title = coalesce(?3, title),
                    settings = coalesce(?4, settings),
                    task_id = coalesce(?5, task_id),
                    owner_user_id = coalesce(owner_user_id, ?6)
              WHERE id = ?1",
            params![
                id,
                time::stamp(updated_at),
                title,
                settings,
                self.task_id,
                owner,
            ],
        )
        .map_err(SqliteError::query("conversation update"))?;
        Ok(())
    }
}

impl PreparedMessage {
    fn new(message: &CaptureMessage) -> Result<Self, SqliteError> {
        let created_at = crate::time::Stamp::from(message.created_at);
        let (content, full_content_length) = capped(&message.content)?;
        let (tool_result, full_tool_result_length) = match message.tool_result.as_deref() {
            Some(text) => {
                let (kept, full) = capped(text)?;
                (Some(kept.to_owned()), full)
            }
            None => (None, None),
        };
        let metadata = stored_metadata(&StoredMetadata {
            supplied: message.metadata.as_ref(),
            event_id: &message.event_id,
            truncated: full_content_length.is_some() || full_tool_result_length.is_some(),
            full_content_length,
            full_tool_result_length,
        })?;
        Ok(Self {
            event_id: message.event_id.clone(),
            role: message.role,
            content: content.to_owned(),
            tool_name: message.tool_name.clone(),
            tool_args: message
                .tool_args
                .as_ref()
                .map(|args| encode(args, "tool_args"))
                .transpose()?,
            tool_result,
            metadata,
            created_at,
        })
    }

    fn write(
        &self,
        tx: &Transaction<'_>,
        target: &Target<'_>,
        now: DateTime<Utc>,
    ) -> Result<Outcome, SqliteError> {
        let conversation_id = target.conversation_id;
        if let Some(stored) = self.stored_by_event(tx, conversation_id)? {
            if self.fill_pending(tx, &stored)? {
                return Ok(Outcome::Filled(stored));
            }
            return Ok(Outcome::Skipped);
        }
        if let Some(hook_row) = self.unclaimed_hook_row(tx, conversation_id)? {
            tx.execute(
                "UPDATE ros_messages SET metadata = ?2 WHERE id = ?1",
                params![hook_row, self.metadata],
            )
            .map_err(SqliteError::query("cowork claim"))?;
            return Ok(Outcome::Skipped);
        }
        let id = NewMessage {
            conversation_id,
            agent: target.agent,
            channel: target.channel,
            role: self.role,
            content: &self.content,
            tool_name: self.tool_name.as_deref(),
            tool_args: self.tool_args.as_deref(),
            tool_result: self.tool_result.as_deref(),
            metadata: Some(&self.metadata),
            created_at: self.created_at.at(now),
            owner: target.owner,
        }
        .insert(tx)?;
        Ok(Outcome::Inserted(id))
    }

    fn stored_by_event(
        &self,
        tx: &Transaction<'_>,
        conversation_id: &str,
    ) -> Result<Option<String>, SqliteError> {
        tx.query_row(
            "SELECT id FROM ros_messages
              WHERE conversation_id = ?1 AND json_extract(metadata, '$.event_id') = ?2
              ORDER BY rowid LIMIT 1",
            params![conversation_id, self.event_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(SqliteError::query("event id lookup"))
    }

    fn fill_pending(&self, tx: &Transaction<'_>, id: &str) -> Result<bool, SqliteError> {
        let changed = tx
            .execute(
                "UPDATE ros_messages SET tool_result = ?2, metadata = ?3
                  WHERE id = ?1
                    AND ?2 IS NOT NULL
                    AND json_extract(metadata, '$.pending_result') = 1
                    AND json_extract(?3, '$.pending_result') IS NOT 1",
                params![id, self.tool_result, self.metadata],
            )
            .map_err(SqliteError::query("pending tool result fill"))?;
        Ok(changed > 0)
    }

    fn unclaimed_hook_row(
        &self,
        tx: &Transaction<'_>,
        conversation_id: &str,
    ) -> Result<Option<String>, SqliteError> {
        tx.query_row(
            "SELECT id FROM ros_messages
              WHERE json_extract(?4, '$.source') = 'cowork-transcript'
                AND conversation_id = ?1
                AND json_extract(metadata, '$.source') = 'cowork-hook'
                AND role = ?2 AND content = ?3
              ORDER BY rowid LIMIT 1",
            params![
                conversation_id,
                self.role.to_string(),
                self.content,
                self.metadata
            ],
            |row| row.get(0),
        )
        .optional()
        .map_err(SqliteError::query("cowork hook lookup"))
    }
}

fn queue_if_absent(
    tx: &Transaction<'_>,
    message: &PreparedMessage,
    id: &str,
    embed: bool,
    now: DateTime<Utc>,
) -> Result<(), SqliteError> {
    if !embed || message.role != Role::Tool {
        return Ok(());
    }
    if enqueue_embed(tx, "ros_messages", id, now)? {
        return Ok(());
    }
    Ok(())
}

fn queue_inserted(
    tx: &Transaction<'_>,
    id: &str,
    embed: bool,
    now: DateTime<Utc>,
) -> Result<(), SqliteError> {
    if !embed {
        return Ok(());
    }
    if enqueue_embed(tx, "ros_messages", id, now)? {
        Ok(())
    } else {
        Err(SqliteError::EmbedAlreadyQueued { id: id.to_owned() })
    }
}

fn capped(text: &str) -> Result<(&str, Option<u64>), SqliteError> {
    match text.char_indices().nth(MAX_CONTENT_CHARS) {
        Some((end, _)) => {
            let full = u64::try_from(text.chars().count()).map_err(SqliteError::Count)?;
            Ok((text.split_at(end).0, Some(full)))
        }
        None => Ok((text, None)),
    }
}

#[derive(Debug, Serialize)]
struct StoredMetadata<'a> {
    #[serde(flatten)]
    supplied: Option<&'a JsonObject>,
    event_id: &'a str,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    truncated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    full_content_length: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    full_tool_result_length: Option<u64>,
}

fn stored_metadata(metadata: &StoredMetadata<'_>) -> Result<String, SqliteError> {
    let failed = |source| SqliteError::Encode {
        column: "metadata",
        source,
    };
    let merged = serde_json::to_value(metadata).map_err(failed)?;
    serde_json::to_string(&merged).map_err(failed)
}

fn stored_settings(tx: &Transaction<'_>, id: &str) -> Result<Option<JsonObject>, SqliteError> {
    let text: Option<String> = tx
        .query_row(
            "SELECT settings FROM ros_conversations WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .map_err(SqliteError::query("conversation settings"))?;
    text.map(|text| {
        serde_json::from_str(&text).map_err(|source| SqliteError::StoredJson {
            column: "settings",
            source,
        })
    })
    .transpose()
}

pub(crate) fn encode(object: &JsonObject, column: &'static str) -> Result<String, SqliteError> {
    serde_json::to_string(object).map_err(|source| SqliteError::Encode { column, source })
}
