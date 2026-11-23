mod pages;

use std::str::FromStr;

use async_trait::async_trait;
use jiff::Timestamp;
use kernel::cleanup::CleanupError;
use kernel::cleanup::CleanupSource;
use kernel::cleanup::Snapshot;
use kernel::cleanup::SnapshotConversation;
use kernel::cleanup::SnapshotCorrection;
use kernel::cleanup::SnapshotHead;
use kernel::cleanup::SnapshotPlacement;
use kernel::cleanup::SnapshotRedirect;
use kernel::cleanup::TaskOrigin;
use kernel::filing::Filable;
use kernel::topics::PlacementSource;
use kernel::topics::TopicId;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;
use uuid::Uuid;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::time;

#[async_trait]
impl CleanupSource for SqliteStore {
    async fn snapshot(&self) -> Result<Snapshot, CleanupError> {
        self.with_conn(read_snapshot)
            .await
            .map_err(|error| CleanupError::Snapshot(Box::new(error)))
    }
}

fn read_snapshot(conn: &mut Connection) -> Result<Snapshot, SqliteError> {
    let writable = !conn.is_readonly("main").map_err(SqliteError::Pragma)?;
    if writable {
        conn.pragma_update(None, "query_only", "ON")
            .map_err(SqliteError::Pragma)?;
    }
    let read = assemble(conn);
    if writable {
        conn.pragma_update(None, "query_only", "OFF")
            .map_err(SqliteError::Pragma)?;
    }
    read
}

fn assemble(conn: &Connection) -> Result<Snapshot, SqliteError> {
    let (projects, refused) = pages::projects(conn)?;
    Ok(Snapshot {
        taken_at: Timestamp::now(),
        conversations: conversations(conn)?,
        topics: pages::topics(conn)?,
        redirects: redirects(conn)?,
        placements: placements(conn)?,
        corrections: corrections(conn)?,
        projects,
        refused,
        pages: pages::knowledge(conn)?,
        sections: pages::sections(conn)?,
    })
}

fn conversations(conn: &Connection) -> Result<Vec<SnapshotConversation>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT c.id, c.session_key, c.agent, c.channel, c.title,
                    json_extract(c.settings, '$.cwd') AS cwd, opening.content,
                    c.updated_at,
                    (SELECT count(*) FROM ros_messages m
                      WHERE m.conversation_id = c.id AND m.role = 'user'),
                    t.origin
               FROM rh_summary_openings opening
               JOIN ros_conversations c ON c.id = opening.conversation_id
               LEFT JOIN rh_tasks_archive t ON c.session_key = 'task:' || t.id
              ORDER BY c.updated_at, c.id",
        )
        .map_err(failed("cleanup conversations"))?;
    let rows = statement
        .query_map([], conversation_row)
        .map_err(failed("cleanup conversations"))?;
    let mut conversations = Vec::new();
    for row in rows {
        let mut conversation = conversation_from(row.map_err(failed("cleanup conversations"))?)?;
        conversation.task_parent = task_parent(conn, &conversation)?;
        conversations.push(conversation);
    }
    Ok(conversations)
}

struct ConversationText {
    id: String,
    session_key: String,
    agent: String,
    channel: String,
    title: Option<String>,
    cwd: Option<String>,
    opening: Option<String>,
    updated_at: String,
    user_turns: i64,
    task_origin: Option<String>,
}

fn conversation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConversationText> {
    let opening: Option<String> = row.get(6)?;
    Ok(ConversationText {
        id: row.get(0)?,
        session_key: row.get(1)?,
        agent: row.get(2)?,
        channel: row.get(3)?,
        title: row.get(4)?,
        cwd: row.get(5)?,
        opening,
        updated_at: row.get(7)?,
        user_turns: row.get(8)?,
        task_origin: row.get(9)?,
    })
}

fn conversation_from(row: ConversationText) -> Result<SnapshotConversation, SqliteError> {
    Ok(SnapshotConversation {
        head: SnapshotHead {
            id: parse_uuid(&row.id)?,
            session_key: row.session_key,
            agent: row.agent,
            channel: row.channel,
            title: row.title,
        },
        cwd: row.cwd.filter(|cwd| !cwd.trim().is_empty()),
        summary: row.opening.clone(),
        last_active: time::stored(&row.updated_at, "updated_at")?,
        task_parent: None,
        task_origin: task_origin(row.task_origin)?,
        opening: row.opening,
        user_turns: u32::try_from(row.user_turns).map_err(SqliteError::Count)?,
        parent_session: None,
    })
}

fn task_origin(raw: Option<String>) -> Result<Option<TaskOrigin>, SqliteError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    TaskOrigin::from_str(trimmed)
        .map(Some)
        .map_err(|source| SqliteError::StoredValue {
            column: "task origin",
            value: source.to_string(),
        })
}

fn task_parent(
    conn: &Connection,
    conversation: &SnapshotConversation,
) -> Result<Option<SnapshotHead>, SqliteError> {
    let Filable::Task(task) = Filable::of(&conversation.head.head()) else {
        return Ok(None);
    };
    let named = format!("%{task}%");
    conn.query_row(
        "WITH named AS (
             SELECT m.conversation_id, m.created_at, 1 AS tool_output FROM ros_messages m
              WHERE m.tool_result IS NOT NULL AND length(m.tool_result) > 0
                AND m.tool_result LIKE ?1
             UNION ALL
             SELECT m.conversation_id, m.created_at, 0 FROM ros_messages m
              WHERE m.content LIKE ?1
         )
         SELECT c.id, c.session_key, c.agent, c.channel, c.title
           FROM named n
           JOIN ros_conversations c ON c.id = n.conversation_id
          WHERE n.conversation_id <> ?2
          ORDER BY n.tool_output DESC, rh_instant(n.created_at), c.id
          LIMIT 1",
        params![named, conversation.head.id.to_string()],
        head_text,
    )
    .optional()
    .map_err(failed("task parent"))?
    .map(head_from)
    .transpose()
}

fn head_text(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<(String, String, String, String, Option<String>)> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
    ))
}

fn head_from(
    (id, session_key, agent, channel, title): (String, String, String, String, Option<String>),
) -> Result<SnapshotHead, SqliteError> {
    Ok(SnapshotHead {
        id: parse_uuid(&id)?,
        session_key,
        agent,
        channel,
        title,
    })
}

fn placements(conn: &Connection) -> Result<Vec<SnapshotPlacement>, SqliteError> {
    let mut rows = placement_query(
        conn,
        "SELECT session_id, NULL, topic_id, placed_by FROM rh_session_placements",
    )?;
    rows.extend(placement_query(
        conn,
        "SELECT NULL, conversation_id, topic_id, placed_by FROM rh_conversation_placements",
    )?);
    Ok(rows)
}

fn placement_query(conn: &Connection, sql: &str) -> Result<Vec<SnapshotPlacement>, SqliteError> {
    let mut statement = conn.prepare(sql).map_err(failed("cleanup placements"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(failed("cleanup placements"))?;
    let mut placements = Vec::new();
    for row in rows {
        let (session, conversation, topic, by) = row.map_err(failed("cleanup placements"))?;
        placements.push(SnapshotPlacement {
            session,
            conversation: optional_uuid(conversation)?,
            topic: topic_id(&topic)?,
            by: PlacementSource::from_str(&by).map_err(|_| stored("placed_by", by))?,
        });
    }
    Ok(placements)
}

fn corrections(conn: &Connection) -> Result<Vec<SnapshotCorrection>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT k.session_id, k.conversation_id, f.name, t.name,
                    COALESCE(
                        (SELECT c.title FROM ros_conversations c WHERE c.id = k.conversation_id),
                        (SELECT c.title FROM ros_conversations c
                          WHERE c.session_key = k.session_id
                          ORDER BY c.updated_at DESC LIMIT 1)
                    )
               FROM rh_filing_corrections k
               JOIN rh_topics f ON f.id = k.from_topic
               LEFT JOIN rh_topics t ON t.id = k.to_topic
              ORDER BY k.corrected_at DESC",
        )
        .map_err(failed("cleanup corrections"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, Option<String>>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
            ))
        })
        .map_err(failed("cleanup corrections"))?;
    let mut corrections = Vec::new();
    for row in rows {
        let (session, conversation, from, to, title) =
            row.map_err(failed("cleanup corrections"))?;
        corrections.push(SnapshotCorrection {
            session,
            conversation: optional_uuid(conversation)?,
            from,
            to,
            title,
        });
    }
    Ok(corrections)
}

fn redirects(conn: &Connection) -> Result<Vec<SnapshotRedirect>, SqliteError> {
    let mut statement = conn
        .prepare("SELECT slug, topic_id FROM rh_topic_redirects ORDER BY slug")
        .map_err(failed("cleanup redirects"))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(failed("cleanup redirects"))?;
    let mut redirects = Vec::new();
    for row in rows {
        let (slug, topic) = row.map_err(failed("cleanup redirects"))?;
        redirects.push(SnapshotRedirect {
            slug,
            topic: topic_id(&topic)?,
        });
    }
    Ok(redirects)
}

fn topic_id(text: &str) -> Result<TopicId, SqliteError> {
    TopicId::from_str(text).map_err(|_| stored("topic_id", text.to_owned()))
}

fn parse_uuid(text: &str) -> Result<Uuid, SqliteError> {
    Uuid::parse_str(text).map_err(|_| stored("id", text.to_owned()))
}

fn optional_uuid(value: Option<String>) -> Result<Option<Uuid>, SqliteError> {
    match value {
        None => Ok(None),
        Some(text) => Uuid::parse_str(&text)
            .map(Some)
            .map_err(|_| stored("conversation_id", text)),
    }
}

fn stored(column: &'static str, value: String) -> SqliteError {
    SqliteError::StoredValue { column, value }
}

fn failed(operation: &'static str) -> impl FnOnce(rusqlite::Error) -> SqliteError {
    SqliteError::query(operation)
}
