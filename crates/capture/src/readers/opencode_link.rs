use std::path::Path;

use rusqlite::Connection;
use rusqlite::OptionalExtension;

use super::harness_db::HarnessDbError;
use super::harness_db::OpencodeLayout;
use super::harness_db::Opened;
use super::harness_db::open_read;
use super::harness_db::opencode_layout;
use super::harness_db::query_error;
use crate::records::SubagentLink;

const PARENT_OF_SESSION: &str = "SELECT parent_id FROM session WHERE id = ?1";
const PARENT_OF_SESSION_V2: &str = "SELECT parent_id FROM session_v2 WHERE id = ?1";

const SPAWN_PART: &str = "\
SELECT json_extract(data, '$.callID') AS tool_use_id, \
       json_extract(data, '$.state.input.subagent_type') AS agent_type, \
       json_extract(data, '$.state.input.description') AS description \
FROM part \
WHERE session_id = ?1 \
  AND json_extract(data, '$.type') = 'tool' \
  AND json_extract(data, '$.state.metadata.sessionId') = ?2 \
ORDER BY time_created DESC LIMIT 1";

const SPAWN_CONTENT: &str = "\
SELECT json_extract(item.value, '$.id') AS tool_use_id, \
       json_extract(item.value, '$.state.input.agent') AS agent_type, \
       json_extract(item.value, '$.state.input.description') AS description \
FROM session_message, json_each(session_message.data, '$.content') AS item \
WHERE session_message.session_id = ?1 \
  AND session_message.type = 'assistant' \
  AND json_extract(item.value, '$.type') = 'tool' \
  AND json_extract(item.value, '$.state.metadata.sessionID') = ?2 \
ORDER BY session_message.seq DESC LIMIT 1";

const SPAWN_PART_ROW: &str = "\
SELECT json_extract(data, '$.id') AS tool_use_id, \
       json_extract(data, '$.state.input.agent') AS agent_type, \
       json_extract(data, '$.state.input.description') AS description \
FROM session_part \
WHERE session_id = ?1 \
  AND type = 'tool' \
  AND json_extract(data, '$.state.metadata.sessionID') = ?2 \
ORDER BY time_created DESC LIMIT 1";

const PART_ROWS: &str =
    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'session_part'";

struct Spawn {
    tool_use_id: Option<String>,
    agent_type: Option<String>,
    description: Option<String>,
}

pub(crate) fn opencode_link(
    database: &Path,
    child: &str,
) -> Result<Option<SubagentLink>, HarnessDbError> {
    let Opened::Ready(connection) = open_read(database) else {
        return Ok(None);
    };
    link(&connection, child).map_err(|error| query_error(&error.to_string(), Box::new(error)))
}

fn link(connection: &Connection, child: &str) -> rusqlite::Result<Option<SubagentLink>> {
    let part_rows: i64 = connection.query_row(PART_ROWS, [], |row| row.get(0))?;
    let (parent_of, spawned_by): (&str, &[&str]) = match opencode_layout(connection)? {
        OpencodeLayout::Events if part_rows > 0 => {
            (PARENT_OF_SESSION_V2, &[SPAWN_CONTENT, SPAWN_PART_ROW])
        }
        OpencodeLayout::Events => (PARENT_OF_SESSION_V2, &[SPAWN_CONTENT]),
        OpencodeLayout::JsonParts => (PARENT_OF_SESSION, &[SPAWN_PART]),
        OpencodeLayout::Parts => (PARENT_OF_SESSION, &[]),
    };
    let parent: Option<String> = connection
        .query_row(parent_of, [child], |row| row.get(0))
        .optional()?
        .flatten();
    let Some(parent) = parent.filter(|parent| !parent.is_empty() && parent != child) else {
        return Ok(None);
    };
    let mut spawn = None;
    for query in spawned_by {
        spawn = connection
            .query_row(query, [parent.as_str(), child], |row| {
                Ok(Spawn {
                    tool_use_id: row.get("tool_use_id")?,
                    agent_type: row.get("agent_type")?,
                    description: row.get("description")?,
                })
            })
            .optional()?;
        if spawn.is_some() {
            break;
        }
    }
    let (tool_use_id, agent_type, description) = match spawn {
        Some(spawn) => (spawn.tool_use_id, spawn.agent_type, spawn.description),
        None => (None, None, None),
    };
    Ok(Some(SubagentLink {
        parent_session_id: parent,
        agent_id: child.to_owned(),
        agent_type: present(agent_type),
        description: present(description),
        tool_use_id: present(tool_use_id),
    }))
}

fn present(value: Option<String>) -> Option<String> {
    value
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
}
