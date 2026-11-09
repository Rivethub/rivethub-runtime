use rusqlite::Connection;
use serde::Deserialize;
use wire::JsonObject;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::SessionOrigin;
use wire::message::Role;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::time;

const SESSIONS: &str = "SELECT c.id, c.session_key, c.agent, c.channel, c.title, c.settings, \
     c.created_at, c.updated_at, \
     m.id, m.role, m.content, m.tool_name, m.tool_args, m.tool_result, m.metadata, m.created_at \
     FROM ros_conversations c \
     JOIN ros_messages m ON m.conversation_id = c.id \
     WHERE ?1 IS NULL OR c.id IN (SELECT value FROM json_each(?1)) \
     ORDER BY c.created_at, c.id, m.created_at, m.rowid";

struct StoredSession {
    conversation_id: String,
    session_key: String,
    agent: String,
    channel: String,
    title: Option<String>,
    settings: Option<String>,
    created_at: String,
    updated_at: String,
    id: String,
    role: Role,
    content: String,
    tool_name: Option<String>,
    tool_args: Option<String>,
    tool_result: Option<String>,
    metadata: String,
    message_at: String,
}

impl SqliteStore {
    pub async fn bundled_sessions(
        &self,
        author: BundleAuthor,
        conversation_ids: Option<Vec<String>>,
    ) -> Result<Vec<BundledSession>, SqliteError> {
        self.with_conn(move |conn| read_bundle(conn, &author, conversation_ids.as_deref()))
            .await
    }
}

fn read_bundle(
    conn: &Connection,
    author: &BundleAuthor,
    conversation_ids: Option<&[String]>,
) -> Result<Vec<BundledSession>, SqliteError> {
    let wanted = conversation_ids
        .map(serde_json::to_string)
        .transpose()
        .map_err(|source| SqliteError::StoredJson {
            column: "conversation ids",
            source,
        })?;
    let mut statement = conn
        .prepare(SESSIONS)
        .map_err(SqliteError::query("session bundle"))?;
    let rows = statement
        .query_map([wanted], stored_session)
        .map_err(SqliteError::query("session bundle"))?;
    let mut sessions = Vec::new();
    let mut current: Option<BundledSession> = None;
    for row in rows {
        let row = row.map_err(SqliteError::query("session bundle"))?;
        push_message(&mut sessions, &mut current, author, &row)?;
    }
    if let Some(session) = current {
        sessions.push(session);
    }
    Ok(sessions)
}

fn stored_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredSession> {
    Ok(StoredSession {
        conversation_id: row.get(0)?,
        session_key: row.get(1)?,
        agent: row.get(2)?,
        channel: row.get(3)?,
        title: row.get(4)?,
        settings: row.get(5)?,
        created_at: row.get(6)?,
        updated_at: row.get(7)?,
        id: row.get(8)?,
        role: crate::message::stored_role(row, 9)?,
        content: row.get(10)?,
        tool_name: row.get(11)?,
        tool_args: row.get(12)?,
        tool_result: row.get(13)?,
        metadata: row.get(14)?,
        message_at: row.get(15)?,
    })
}

fn push_message(
    sessions: &mut Vec<BundledSession>,
    current: &mut Option<BundledSession>,
    author: &BundleAuthor,
    row: &StoredSession,
) -> Result<(), SqliteError> {
    let same = current
        .as_ref()
        .is_some_and(|session| session.origin.conversation_id == row.conversation_id);
    if !same {
        if let Some(session) = current.take() {
            sessions.push(session);
        }
        *current = Some(session_for(author, row)?);
    }
    let Some(session) = current.as_mut() else {
        return Err(SqliteError::Invalid {
            message: "session bundle lost its open session".to_owned(),
        });
    };
    session.batch.messages.push(message_for(row)?);
    Ok(())
}

fn session_for(author: &BundleAuthor, row: &StoredSession) -> Result<BundledSession, SqliteError> {
    let created_at = time::stored(&row.created_at, "created_at")?;
    let updated_at = time::stored(&row.updated_at, "updated_at")?;
    Ok(BundledSession {
        origin: SessionOrigin {
            author: author.user.clone(),
            node: author.node.clone(),
            conversation_id: row.conversation_id.clone(),
            session_key: row.session_key.clone(),
            agent: row.agent.clone(),
            created_at,
            updated_at,
        },
        batch: CaptureBatchRequest {
            session_key: row.session_key.clone(),
            agent: row.agent.clone(),
            channel: present(&row.channel),
            title: row.title.as_deref().and_then(present),
            settings: row
                .settings
                .as_deref()
                .map(|text| stored_object(text, "settings"))
                .transpose()?
                .filter(|settings| !settings.as_map().is_empty()),
            task_id: None,
            finalize: None,
            created_at: Some(created_at),
            updated_at: Some(updated_at),
            messages: Vec::new(),
        },
    })
}

fn message_for(row: &StoredSession) -> Result<CaptureMessage, SqliteError> {
    let metadata = stored_object(&row.metadata, "metadata")?;
    let event_id = recorded_event(&metadata, &row.id)?;
    let mut shareable = metadata;
    shareable.retain(|key| key != "event_id");
    let metadata = if shareable.as_map().is_empty() {
        None
    } else {
        Some(shareable)
    };
    Ok(CaptureMessage {
        event_id,
        role: row.role,
        content: row.content.clone(),
        tool_name: row.tool_name.clone(),
        tool_args: row
            .tool_args
            .as_deref()
            .map(|text| stored_object(text, "tool_args"))
            .transpose()?,
        tool_result: row.tool_result.clone(),
        metadata,
        created_at: Some(time::stored(&row.message_at, "created_at")?),
    })
}

fn present(text: &str) -> Option<String> {
    if text.is_empty() {
        None
    } else {
        Some(text.to_owned())
    }
}

fn stored_object(text: &str, column: &'static str) -> Result<JsonObject, SqliteError> {
    serde_json::from_str(text).map_err(|source| SqliteError::StoredJson { column, source })
}

#[derive(Deserialize)]
struct RecordedEvent {
    event_id: Option<String>,
}

fn recorded_event(metadata: &JsonObject, fallback: &str) -> Result<String, SqliteError> {
    let recorded =
        metadata
            .deserialize::<RecordedEvent>()
            .map_err(|source| SqliteError::StoredJson {
                column: "metadata",
                source,
            })?;
    match recorded.event_id {
        Some(id) if !id.is_empty() => Ok(id),
        Some(_) | None => Ok(fallback.to_owned()),
    }
}
