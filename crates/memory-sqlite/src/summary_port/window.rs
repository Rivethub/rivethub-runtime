use super::*;

pub(super) fn load_window(
    conn: &rusqlite::Connection,
    id: Uuid,
) -> Result<Option<ConversationWindow>, SqliteError> {
    let head = conn
        .query_row(
            "SELECT id, session_key, agent, channel, title FROM ros_conversations WHERE id = ?1",
            params_from_iter([Value::Text(id.to_string())]),
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            },
        )
        .optional()
        .map_err(SqliteError::query("load conversation"))?;
    let Some((head_id, session_key, agent, channel, title)) = head else {
        return Ok(None);
    };
    let messages = load_messages(conn, &id.to_string())?;
    let latest = latest_message(conn, &id.to_string())?;
    let provenance = provenance_of(conn, &id.to_string(), &session_key)?;
    Ok(Some(ConversationWindow {
        provenance,
        conversation: ConversationHead {
            id: parse_id(&head_id)?,
            session_key,
            agent,
            channel,
            title,
        },
        messages,
        leaves: load_nodes(conn, &id.to_string(), SummaryKind::Leaf)?,
        branches: load_nodes(conn, &id.to_string(), SummaryKind::Branch)?,
        latest_message_at: latest,
    }))
}

fn provenance_of(
    conn: &rusqlite::Connection,
    id: &str,
    session_key: &str,
) -> Result<Option<Provenance>, SqliteError> {
    if !kernel::memory::is_imported(session_key) {
        return Ok(None);
    }
    let settings: Option<String> = conn
        .query_row(
            "SELECT settings FROM ros_conversations WHERE id = ?1",
            [id],
            |row| row.get(0),
        )
        .map_err(SqliteError::query("load import origin"))?;
    let settings = match settings {
        Some(text) => serde_json::from_str(&text).map_err(|source| SqliteError::StoredJson {
            column: "settings",
            source,
        })?,
        None => wire::JsonObject::default(),
    };
    Ok(Some(Provenance::from_settings(&settings)))
}

fn load_messages(conn: &rusqlite::Connection, id: &str) -> Result<Vec<WindowMessage>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT m.id, m.agent, m.role, m.content, m.created_at, m.metadata, \
                    CASE WHEN m.role = 'tool' \
                         THEN substr(CAST(coalesce(m.tool_args, '') AS BLOB), 1, ?2) END, \
                    CASE WHEN m.role = 'tool' \
                         THEN substr(CAST(coalesce(m.tool_result, '') AS BLOB), 1, ?3) END \
               FROM ros_messages m \
              WHERE m.conversation_id = ?1 \
                AND NOT EXISTS (SELECT 1 FROM ros_summary_sources s WHERE s.message_id = m.id) \
              ORDER BY m.created_at, m.rowid",
        )
        .map_err(SqliteError::query("load messages"))?;
    let rows = statement
        .query_map(
            params_from_iter([
                Value::Text(id.to_owned()),
                prefix_bytes(ToolUse::INPUT_BYTES)?,
                prefix_bytes(ToolUse::RESULT_BYTES)?,
            ]),
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    stored_role(row, 2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<Vec<u8>>>(6)?,
                    row.get::<_, Option<Vec<u8>>>(7)?,
                ))
            },
        )
        .map_err(SqliteError::query("load messages"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("load messages"))?;
    let mut messages = rows
        .into_iter()
        .map(
            |(id, agent, role, content, created_at, metadata, input, result)| {
                let message = WindowMessage {
                    id: parse_id(&id)?,
                    agent,
                    role,
                    content,
                    tool: match (input, result) {
                        (Some(input), Some(result)) => ToolUse::from_prefixes(&input, &result),
                        _ => None,
                    },
                    created_at: time::stored(&created_at, "created_at")?,
                };
                Ok((message, transcript_metadata(&metadata)))
            },
        )
        .collect::<Result<Vec<_>, SqliteError>>()?;
    if messages.iter().any(|(message, metadata)| {
        MessagePosition::new(message.created_at, message.role, metadata.as_ref())
            .has_order_evidence()
    }) {
        messages.sort_unstable_by_key(|(message, _)| (message.created_at, message.id));
    }
    Ok(
        order_messages(messages, MemoryBrowseOrder::Asc, |(message, metadata)| {
            MessagePosition::new(message.created_at, message.role, metadata.as_ref())
        })
        .into_iter()
        .map(|(message, _)| message)
        .collect(),
    )
}

fn prefix_bytes(budget: usize) -> Result<Value, SqliteError> {
    let bytes = i64::try_from(budget).map_err(SqliteError::Count)?;
    Ok(Value::Integer(bytes.saturating_add(1)))
}

fn load_nodes(
    conn: &rusqlite::Connection,
    id: &str,
    kind: SummaryKind,
) -> Result<Vec<SummaryNode>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT id, kind, content, message_count, earliest_at, latest_at, depth \
               FROM ros_summaries \
              WHERE conversation_id = ?1 AND parent_id IS NULL AND kind = ?2 \
              ORDER BY earliest_at, id",
        )
        .map_err(SqliteError::query("load summary nodes"))?;
    let rows = statement
        .query_map(
            params_from_iter([
                Value::Text(id.to_owned()),
                Value::Text(kind.as_str().to_owned()),
            ]),
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                ))
            },
        )
        .map_err(SqliteError::query("load summary nodes"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("load summary nodes"))?;
    rows.into_iter()
        .map(|(id, kind, content, count, earliest, latest, depth)| {
            node(NodeParts {
                id,
                kind,
                content,
                count,
                earliest,
                latest,
                depth,
            })
        })
        .collect()
}

struct NodeParts {
    id: String,
    kind: String,
    content: String,
    count: i64,
    earliest: Option<String>,
    latest: Option<String>,
    depth: i64,
}

fn node(parts: NodeParts) -> Result<SummaryNode, SqliteError> {
    let kind = parts
        .kind
        .parse::<SummaryKind>()
        .map_err(|error| SqliteError::Invalid {
            message: format!(
                "stored summary kind {} is not leaf, branch, or root: {error}",
                parts.kind
            ),
        })?;
    let Some(earliest) = parts.earliest else {
        return Err(SqliteError::MissingHit { id: parts.id });
    };
    let Some(latest) = parts.latest else {
        return Err(SqliteError::MissingHit { id: parts.id });
    };
    Ok(SummaryNode {
        id: parse_id(&parts.id)?,
        kind,
        content: parts.content,
        message_count: u32::try_from(parts.count).map_err(SqliteError::Count)?,
        earliest_at: time::stored(&earliest, "earliest_at")?,
        latest_at: time::stored(&latest, "latest_at")?,
        depth: i32::try_from(parts.depth).map_err(SqliteError::Count)?,
    })
}

fn latest_message(conn: &rusqlite::Connection, id: &str) -> Result<Option<Timestamp>, SqliteError> {
    let text: Option<String> = conn
        .query_row(
            "SELECT max(created_at) FROM ros_messages WHERE conversation_id = ?1",
            params_from_iter([Value::Text(id.to_owned())]),
            |row| row.get(0),
        )
        .optional()
        .map_err(SqliteError::query("latest message"))?;
    match text {
        Some(text) => Ok(Some(time::stored(&text, "created_at")?)),
        None => Ok(None),
    }
}
