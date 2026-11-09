use chrono::DateTime;
use chrono::Utc;
use kernel::memory::MemoryEntry;
use uuid::Uuid;
use wire::JsonObject;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::SessionOrigin;

use crate::error::Error;
use crate::read::MessageEntryRow;

#[derive(serde::Deserialize)]
struct RecordedEvent {
    event_id: Option<String>,
}

struct BundleRow {
    conversation_id: Uuid,
    session_key: String,
    agent: String,
    channel: String,
    title: Option<String>,
    settings: Option<sqlx::types::Json<JsonObject>>,
    node: Option<String>,
    owner_user_id: String,
    conversation_created_at: DateTime<Utc>,
    conversation_updated_at: DateTime<Utc>,
    message_id: Option<String>,
    role_text: Option<String>,
    content: String,
    tool_name: Option<String>,
    tool_args: Option<String>,
    tool_result: Option<String>,
    metadata: Option<String>,
    message_at: DateTime<Utc>,
}

impl crate::PostgresMemory {
    pub async fn bundled_sessions(
        &self,
        author: &BundleAuthor,
        conversation_ids: Option<&[Uuid]>,
    ) -> Result<Vec<BundledSession>, Error> {
        let rows = sqlx::query_as!(
            BundleRow,
            "select c.id as conversation_id, c.session_key, c.agent, c.channel, c.title, \
             c.settings as \"settings: sqlx::types::Json<JsonObject>\", c.node, c.owner_user_id, \
             c.created_at as conversation_created_at, c.updated_at as conversation_updated_at, \
             m.id::text as message_id, m.role as role_text, m.content, m.tool_name, \
             m.tool_args::text as tool_args, m.tool_result, m.metadata::text as metadata, \
             m.created_at as message_at \
             from rh_conversations c join rh_messages m on m.conversation_id = c.id \
             where c.owner_user_id = $1 and ($2::uuid[] is null or c.id = any($2)) \
             order by c.created_at, c.id, m.created_at, m.id",
            author.user,
            conversation_ids as Option<&[Uuid]>,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("session bundle", source))?;
        let mut sessions: Vec<BundledSession> = Vec::new();
        for row in rows {
            let open = sessions.last().is_some_and(|session| {
                session.origin.conversation_id == row.conversation_id.to_string()
            });
            if !open {
                sessions.push(session_for(author, &row));
            }
            let message = message_for(row)?;
            if let Some(session) = sessions.last_mut() {
                session.batch.messages.push(message);
            }
        }
        Ok(sessions)
    }
}

fn session_for(author: &BundleAuthor, row: &BundleRow) -> BundledSession {
    BundledSession {
        origin: SessionOrigin {
            author: row.owner_user_id.clone(),
            node: row.node.clone().unwrap_or_else(|| author.node.clone()),
            conversation_id: row.conversation_id.to_string(),
            session_key: row.session_key.clone(),
            agent: row.agent.clone(),
            created_at: row.conversation_created_at,
            updated_at: row.conversation_updated_at,
        },
        batch: CaptureBatchRequest {
            session_key: row.session_key.clone(),
            agent: row.agent.clone(),
            channel: Some(row.channel.clone()).filter(|channel| !channel.is_empty()),
            title: row.title.clone().filter(|title| !title.is_empty()),
            settings: row
                .settings
                .as_ref()
                .map(|settings| settings.0.clone())
                .filter(|settings| !settings.as_map().is_empty()),
            task_id: None,
            finalize: None,
            created_at: Some(row.conversation_created_at),
            updated_at: Some(row.conversation_updated_at),
            messages: Vec::new(),
        },
    }
}

fn message_for(row: BundleRow) -> Result<CaptureMessage, Error> {
    let entry = MemoryEntry::try_from(MessageEntryRow {
        id: row.message_id,
        session_key: row.session_key,
        agent: row.agent,
        channel: row.channel,
        role_text: row.role_text,
        content: row.content,
        tool_name: row.tool_name,
        tool_args: row.tool_args,
        tool_result: row.tool_result,
        metadata: row.metadata,
        created_at: Some(row.message_at),
    })?;
    let mut metadata = entry.metadata.unwrap_or_default();
    let event_id = metadata
        .deserialize::<RecordedEvent>()
        .map_err(|source| Error::stored("metadata", source))?
        .event_id
        .filter(|event_id| !event_id.is_empty())
        .or(entry.id)
        .ok_or_else(|| Error::protocol("bundled message row is missing its id"))?;
    metadata.retain(|key| key != "event_id");
    Ok(CaptureMessage {
        event_id,
        role: entry.role,
        content: entry.content,
        tool_name: entry.tool_name,
        tool_args: entry.tool_args,
        tool_result: entry.tool_result,
        metadata: Some(metadata).filter(|metadata| !metadata.as_map().is_empty()),
        created_at: entry.created_at,
    })
}
