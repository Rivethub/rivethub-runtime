use jiff::Timestamp;
use kernel::filing::AllFiled;
use kernel::filing::FiledConversation;
use kernel::filing::FiledSession;
use kernel::filing::bare_session_prefix;
use kernel::topics::Suggestion;
use kernel::topics::TopicError;
use rusqlite::Connection;
use rusqlite::params;

use super::filing::conversation_placement;
use super::history::mark;
use super::rows;
use super::rows::PlacementRow;
use super::rows::failed;
use super::rows::stored;
use super::rows::timestamp;

const ALL_FILED: &str = "WITH placed AS (
        SELECT p.session_id, p.topic_id, p.placed_by, p.placed_at, p.owner_user_id,
               CASE WHEN substr(p.session_id, 1, length(?1)) = ?1
                    THEN substr(p.session_id, length(?1) + 1) END AS bare
          FROM rh_session_placements p
    ),
    titled AS (
        SELECT c.session_key, c.title, c.updated_at FROM ros_conversations c
         WHERE c.title IS NOT NULL
    ),
    noded AS (
        SELECT c.session_key, n.node, c.updated_at FROM ros_conversations c
          JOIN rh_conversation_nodes n ON n.conversation_id = c.id
    )
    SELECT p.session_id, NULL, p.topic_id, p.placed_by, p.placed_at,
           coalesce(
               (SELECT t.title FROM titled t WHERE t.session_key = p.session_id
                 ORDER BY t.updated_at DESC LIMIT 1),
               (SELECT t.title FROM titled t WHERE t.session_key = p.bare
                 ORDER BY t.updated_at DESC LIMIT 1)),
           NULL,
           (SELECT max(c.updated_at) FROM ros_conversations c
             WHERE c.session_key IN (p.session_id, p.bare)),
           coalesce(
               (SELECT k.node FROM noded k WHERE k.session_key = p.session_id
                 ORDER BY k.updated_at DESC LIMIT 1),
               (SELECT k.node FROM noded k WHERE k.session_key = p.bare
                 ORDER BY k.updated_at DESC LIMIT 1)),
           p.owner_user_id
      FROM placed p
    UNION ALL
    SELECT NULL, p.conversation_id, p.topic_id, p.placed_by, p.placed_at,
           c.title, c.channel, c.updated_at, n.node, p.owner_user_id
      FROM rh_conversation_placements p
      JOIN ros_conversations c ON c.id = p.conversation_id
      LEFT JOIN rh_conversation_nodes n ON n.conversation_id = c.id
    UNION ALL
    SELECT h.session_id, NULL, NULL, NULL, h.marked_at, NULL, NULL, NULL, NULL, NULL
      FROM rh_session_history h
    ORDER BY 8 DESC, 5 DESC, 1, 2";

struct FilingRow {
    session_id: Option<String>,
    conversation_id: Option<String>,
    topic_id: Option<String>,
    placed_by: Option<String>,
    at: String,
    title: Option<String>,
    channel: Option<String>,
    last_active: Option<String>,
    node: Option<String>,
    owner_user_id: Option<String>,
}

pub(super) fn all_filed(conn: &Connection) -> Result<AllFiled, TopicError> {
    let found = conn
        .prepare(ALL_FILED)
        .and_then(|mut statement| {
            statement
                .query_map(params![bare_session_prefix()], |row| {
                    Ok(FilingRow {
                        session_id: row.get(0)?,
                        conversation_id: row.get(1)?,
                        topic_id: row.get(2)?,
                        placed_by: row.get(3)?,
                        at: row.get(4)?,
                        title: row.get(5)?,
                        channel: row.get(6)?,
                        last_active: row.get(7)?,
                        node: row.get(8)?,
                        owner_user_id: row.get(9)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(failed("all filed"))?;
    let mut filed = AllFiled::default();
    for row in found {
        match (row.topic_id.clone(), row.conversation_id.clone()) {
            (Some(topic), Some(conversation)) => {
                filed
                    .conversations
                    .push(conversation_row(row, topic, conversation)?);
            }
            (Some(topic), None) => filed.sessions.push(session_row(row, topic)?),
            (None, None) => filed.history.push(mark(
                row.session_id.ok_or_else(|| missing("session_id"))?,
                &row.at,
            )?),
            (None, Some(_)) => return Err(missing("topic_id")),
        }
    }
    filed
        .sessions
        .extend(super::inherited::sessions(conn, None)?);
    Ok(filed)
}

pub(super) fn suggestions(conn: &Connection) -> Result<Vec<Suggestion>, TopicError> {
    conn.prepare(
        "SELECT s.session_id, s.name, s.topic_id, s.parent_id
           FROM rh_session_suggestions s
          WHERE NOT EXISTS (SELECT 1 FROM rh_session_placements p WHERE p.session_id = s.session_id)
          ORDER BY s.session_id",
    )
    .and_then(|mut statement| {
        statement
            .query_map([], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
            })?
            .collect::<Result<Vec<_>, _>>()
    })
    .map_err(failed("session suggestions"))?
    .into_iter()
    .map(rows::suggestion)
    .collect()
}

fn session_row(row: FilingRow, topic: String) -> Result<FiledSession, TopicError> {
    Ok(FiledSession {
        placement: rows::placement(PlacementRow {
            session_id: row.session_id.ok_or_else(|| missing("session_id"))?,
            topic_id: topic,
            placed_by: row.placed_by.ok_or_else(|| missing("placed_by"))?,
            placed_at: row.at,
            owner_user_id: row.owner_user_id,
        })?,
        title: row.title,
        last_active: last_active(row.last_active.as_deref())?,
        node: row.node,
        lineage: None,
        visibility: None,
    })
}

fn conversation_row(
    row: FilingRow,
    topic: String,
    conversation: String,
) -> Result<FiledConversation, TopicError> {
    Ok(FiledConversation {
        placement: conversation_placement((
            conversation,
            topic,
            row.placed_by.ok_or_else(|| missing("placed_by"))?,
            row.at,
        ))?,
        title: row.title,
        channel: row.channel.ok_or_else(|| missing("channel"))?,
        last_active: last_active(row.last_active.as_deref())?,
        node: row.node,
    })
}

fn last_active(text: Option<&str>) -> Result<Option<Timestamp>, TopicError> {
    text.map(|at| timestamp(at, "updated_at")).transpose()
}

fn missing(field: &'static str) -> TopicError {
    stored(field)("a filed row without it".into())
}
