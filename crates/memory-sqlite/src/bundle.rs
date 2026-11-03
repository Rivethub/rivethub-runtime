use rusqlite::Connection;
use serde::Deserialize;
use wire::JsonObject;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::SESSION_BUNDLE_VERSION;
use wire::capture::SessionBundle;
use wire::message::Role;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::time;

const SESSIONS: &str = "SELECT c.session_key, c.agent, c.channel, c.title, c.created_at, c.updated_at, \
     m.id, m.role, m.content, m.tool_name, m.tool_args, m.tool_result, m.metadata, m.created_at \
     FROM ros_conversations c \
     JOIN ros_messages m ON m.conversation_id = c.id \
     ORDER BY c.session_key, c.agent, m.created_at, m.rowid";

struct StoredSession {
    session_key: String,
    agent: String,
    channel: String,
    title: Option<String>,
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
    pub async fn session_bundle(&self) -> Result<SessionBundle, SqliteError> {
        self.with_conn(read_bundle).await
    }
}

fn read_bundle(conn: &mut Connection) -> Result<SessionBundle, SqliteError> {
    let mut statement = conn
        .prepare(SESSIONS)
        .map_err(SqliteError::query("session bundle"))?;
    let rows = statement
        .query_map([], stored_session)
        .map_err(SqliteError::query("session bundle"))?;
    let mut sessions = Vec::new();
    let mut current: Option<CaptureBatchRequest> = None;
    for row in rows {
        let row = row.map_err(SqliteError::query("session bundle"))?;
        push_message(&mut sessions, &mut current, &row)?;
    }
    if let Some(batch) = current {
        sessions.push(batch);
    }
    Ok(SessionBundle {
        version: SESSION_BUNDLE_VERSION,
        sessions,
    })
}

fn stored_session(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredSession> {
    Ok(StoredSession {
        session_key: row.get(0)?,
        agent: row.get(1)?,
        channel: row.get(2)?,
        title: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        id: row.get(6)?,
        role: crate::message::stored_role(row, 7)?,
        content: row.get(8)?,
        tool_name: row.get(9)?,
        tool_args: row.get(10)?,
        tool_result: row.get(11)?,
        metadata: row.get(12)?,
        message_at: row.get(13)?,
    })
}

fn push_message(
    sessions: &mut Vec<CaptureBatchRequest>,
    current: &mut Option<CaptureBatchRequest>,
    row: &StoredSession,
) -> Result<(), SqliteError> {
    let same = current
        .as_ref()
        .is_some_and(|batch| batch.session_key == row.session_key && batch.agent == row.agent);
    if !same {
        if let Some(batch) = current.take() {
            sessions.push(batch);
        }
        *current = Some(batch_for(row)?);
    }
    let Some(batch) = current.as_mut() else {
        return Err(SqliteError::Invalid {
            message: "session bundle lost its open session".to_owned(),
        });
    };
    batch.messages.push(message_for(row)?);
    Ok(())
}

fn batch_for(row: &StoredSession) -> Result<CaptureBatchRequest, SqliteError> {
    Ok(CaptureBatchRequest {
        session_key: row.session_key.clone(),
        agent: row.agent.clone(),
        channel: present(&row.channel),
        title: row.title.as_deref().and_then(present),
        settings: None,
        task_id: None,
        finalize: None,
        created_at: Some(time::stored(&row.created_at, "created_at")?),
        updated_at: Some(time::stored(&row.updated_at, "updated_at")?),
        messages: Vec::new(),
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
