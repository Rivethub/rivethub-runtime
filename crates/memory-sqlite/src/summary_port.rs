use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::memory::ConversationHead;
use kernel::memory::ConversationWindow;
use kernel::memory::HEARTBEAT_SESSION_PREFIX;
use kernel::memory::NewSummary;
use kernel::memory::PendingConversation;
use kernel::memory::SummaryKind;
use kernel::memory::SummaryNode;
use kernel::memory::SummaryStore;
use kernel::memory::SummaryStoreError;
use kernel::memory::SummaryWatch;
use kernel::memory::SummaryWrite;
use kernel::memory::ToolUse;
use kernel::memory::WindowMessage;
use kernel::memory::WrittenSummary;
use rusqlite::OptionalExtension as _;
use rusqlite::params_from_iter;
use rusqlite::types::Value;
use uuid::Uuid;
use wire::message::Role;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::time;

#[async_trait]
impl SummaryStore for SqliteStore {
    async fn conversation(
        &self,
        id: Uuid,
    ) -> Result<Option<ConversationWindow>, SummaryStoreError> {
        self.with_conn(move |conn| load_window(conn, id))
            .await
            .map_err(store_err)
    }

    async fn pending(&self) -> Result<Vec<PendingConversation>, SummaryStoreError> {
        self.with_conn(|conn| pending_conversations(conn))
            .await
            .map_err(store_err)
    }

    async fn write_summary(&self, summary: NewSummary) -> Result<SummaryWrite, SummaryStoreError> {
        let (id, kind, depth) = (summary.id, summary.kind, summary.depth);
        let (written, session_key) = self
            .with_conn(move |conn| write_one(conn, &summary))
            .await
            .map_err(store_err)?;
        let told = self.summaries_written.send(WrittenSummary {
            id,
            session_key,
            kind,
            depth,
        });
        if told.is_err() {
            tracing::trace!("written summary has no listener");
        }
        Ok(written)
    }

    async fn retitle(&self, id: Uuid, title: &str) -> Result<bool, SummaryStoreError> {
        let title = title.to_owned();
        self.with_conn(move |conn| crate::title::name_conversation(conn, &id.to_string(), &title))
            .await
            .map_err(store_err)
    }
}

impl SummaryWatch for SqliteStore {
    fn written(&self) -> tokio::sync::broadcast::Receiver<WrittenSummary> {
        self.summaries_written.subscribe()
    }
}

fn pending_conversations(
    conn: &rusqlite::Connection,
) -> Result<Vec<PendingConversation>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT c.id, pending.unsummarized, \
                    (SELECT max(m.created_at) FROM ros_messages m WHERE m.conversation_id = c.id) \
                      AS latest_at \
               FROM (SELECT m.conversation_id, count(*) AS unsummarized FROM ros_messages m \
                      WHERE NOT EXISTS ( \
                        SELECT 1 FROM ros_summary_sources s WHERE s.message_id = m.id) \
                      GROUP BY m.conversation_id) pending \
               JOIN ros_conversations c ON c.id = pending.conversation_id \
              WHERE c.session_key NOT LIKE ?1 \
              ORDER BY latest_at DESC, c.id",
        )
        .map_err(SqliteError::query("pending summaries"))?;
    let rows = statement
        .query_map(
            params_from_iter([Value::Text(format!("{HEARTBEAT_SESSION_PREFIX}%"))]),
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .map_err(SqliteError::query("pending summaries"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("pending summaries"))?;
    let mut pending = Vec::with_capacity(rows.len());
    for (id, count, latest) in rows {
        match pending_conversation(&id, count, latest.as_deref()) {
            Ok(conversation) => pending.push(conversation),
            Err(error) => tracing::warn!("skipped pending conversation {id}: {error}"),
        }
    }
    Ok(pending)
}

fn pending_conversation(
    id: &str,
    count: i64,
    latest: Option<&str>,
) -> Result<PendingConversation, SqliteError> {
    Ok(PendingConversation {
        id: parse_id(id)?,
        unsummarized: u32::try_from(count).map_err(SqliteError::Count)?,
        latest_message_at: latest
            .map(|text| time::stored(text, "created_at"))
            .transpose()?,
    })
}

fn write_one(
    conn: &mut rusqlite::Connection,
    summary: &NewSummary,
) -> Result<(SummaryWrite, String), SqliteError> {
    let count = i64::from(summary.message_count);
    let tx = conn
        .transaction()
        .map_err(SqliteError::query("summary begin"))?;
    tx.execute(
        "INSERT INTO ros_summaries \
            (id, conversation_id, depth, content, kind, message_count, earliest_at, latest_at, \
             model, created_at, pipeline_version) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params_from_iter([
            Value::Text(summary.id.to_string()),
            Value::Text(summary.conversation_id.to_string()),
            Value::Integer(i64::from(summary.depth)),
            Value::Text(summary.content.clone()),
            Value::Text(summary.kind.as_str().to_owned()),
            Value::Integer(count),
            Value::Text(time::stamp(summary.earliest_at)),
            Value::Text(time::stamp(summary.latest_at)),
            Value::Text(summary.model.clone()),
            Value::Text(time::stamp(summary.created_at)),
            Value::Integer(i64::from(summary.pipeline_version)),
        ]),
    )
    .map_err(SqliteError::query("insert summary"))?;
    for source in &summary.sources {
        tx.execute(
            "INSERT INTO ros_summary_sources (summary_id, message_id, ordinal) VALUES (?1, ?2, ?3)",
            params_from_iter([
                Value::Text(summary.id.to_string()),
                Value::Text(source.message_id.to_string()),
                Value::Integer(i64::from(source.ordinal)),
            ]),
        )
        .map_err(SqliteError::query("insert summary source"))?;
    }
    for child in &summary.child_ids {
        let updated = tx
            .execute(
                "UPDATE ros_summaries SET parent_id = ?1 WHERE id = ?2 AND conversation_id = ?3",
                params_from_iter([
                    Value::Text(summary.id.to_string()),
                    Value::Text(child.to_string()),
                    Value::Text(summary.conversation_id.to_string()),
                ]),
            )
            .map_err(SqliteError::query("link summary parent"))?;
        if updated != 1 {
            return Err(SqliteError::MissingHit {
                id: child.to_string(),
            });
        }
    }
    let written = if summary.kind == SummaryKind::Leaf {
        let conversation = summary.conversation_id.to_string();
        crate::title::title_from_first_leaf(&tx, &conversation)?;
        leaf_write(&tx, &conversation)?
    } else {
        SummaryWrite::Added
    };
    let session_key: String = tx
        .query_row(
            "SELECT session_key FROM ros_conversations WHERE id = ?1",
            params_from_iter([Value::Text(summary.conversation_id.to_string())]),
            |row| row.get(0),
        )
        .map_err(SqliteError::query("summary session"))?;
    tx.commit().map_err(SqliteError::query("summary commit"))?;
    Ok((written, session_key))
}

fn leaf_write(
    conn: &rusqlite::Connection,
    conversation: &str,
) -> Result<SummaryWrite, SqliteError> {
    let leaves: i64 = conn
        .query_row(
            "SELECT count(*) FROM ros_summaries WHERE conversation_id = ?1 AND kind = ?2",
            params_from_iter([
                Value::Text(conversation.to_owned()),
                Value::Text(SummaryKind::Leaf.as_str().to_owned()),
            ]),
            |row| row.get(0),
        )
        .map_err(SqliteError::query("count leaves"))?;
    Ok(if leaves == 1 {
        SummaryWrite::FirstLeaf
    } else {
        SummaryWrite::Added
    })
}

fn load_window(
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
    Ok(Some(ConversationWindow {
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

fn load_messages(conn: &rusqlite::Connection, id: &str) -> Result<Vec<WindowMessage>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT m.id, m.agent, m.role, m.content, m.created_at, \
                    CASE WHEN m.role = 'tool' \
                         THEN substr(CAST(coalesce(m.tool_args, '') AS BLOB), 1, ?2) END, \
                    CASE WHEN m.role = 'tool' \
                         THEN substr(CAST(coalesce(m.tool_result, '') AS BLOB), 1, ?3) END \
               FROM ros_messages m \
              WHERE m.conversation_id = ?1 \
                AND NOT EXISTS (SELECT 1 FROM ros_summary_sources s WHERE s.message_id = m.id) \
              ORDER BY m.created_at, m.id",
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
                    (
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                    ),
                    (
                        row.get::<_, Option<Vec<u8>>>(5)?,
                        row.get::<_, Option<Vec<u8>>>(6)?,
                    ),
                ))
            },
        )
        .map_err(SqliteError::query("load messages"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("load messages"))?;
    rows.into_iter()
        .map(
            |((id, agent, role, content, created_at), (input, result))| {
                Ok(WindowMessage {
                    id: parse_id(&id)?,
                    agent,
                    role: role.parse::<Role>().map_err(|source| SqliteError::Query {
                        operation: "message role",
                        source: rusqlite::Error::FromSqlConversionFailure(
                            2,
                            rusqlite::types::Type::Text,
                            Box::new(source),
                        ),
                    })?,
                    content,
                    tool: match (input, result) {
                        (Some(input), Some(result)) => ToolUse::from_prefixes(&input, &result),
                        _ => None,
                    },
                    created_at: time::stored(&created_at, "created_at")?,
                })
            },
        )
        .collect()
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

fn latest_message(
    conn: &rusqlite::Connection,
    id: &str,
) -> Result<Option<DateTime<Utc>>, SqliteError> {
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

fn parse_id(id: &str) -> Result<Uuid, SqliteError> {
    Uuid::parse_str(id).map_err(|source| SqliteError::Query {
        operation: "summary id",
        source: rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(source),
        ),
    })
}

fn store_err(error: SqliteError) -> SummaryStoreError {
    if let SqliteError::MissingHit { id } = error {
        return SummaryStoreError::Missing { id };
    }
    if let SqliteError::Count(_) = error {
        return SummaryStoreError::Count;
    }
    if matches!(
        error,
        SqliteError::Open { .. }
            | SqliteError::NotInitialised { .. }
            | SqliteError::SchemaBehind { .. }
            | SqliteError::Pragma(_)
            | SqliteError::UnsupportedSchema { .. }
            | SqliteError::Join(_)
    ) {
        return SummaryStoreError::Closed;
    }
    if let SqliteError::Invalid { message } = &error
        && let Some(kind) = stored_kind(message)
    {
        return SummaryStoreError::Kind { kind };
    }
    SummaryStoreError::Query {
        message: error.to_string(),
        source: Box::new(error),
    }
}

fn stored_kind(message: &str) -> Option<String> {
    let rest = message.strip_prefix("stored summary kind ")?;
    let kind = rest.strip_suffix(" is not leaf, branch, or root")?;
    Some(kind.to_owned())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use parking_lot::Mutex;
    use rusqlite::Connection;

    use super::pending_conversations;

    const HEALTHY: &str = "0199e2a0-7b3c-7a00-8000-0000000000a1";
    const UNREADABLE: &str = "not-a-conversation-id";

    struct WarningRecorder(Arc<Mutex<Vec<String>>>);

    struct MessageVisitor(Arc<Mutex<Vec<String>>>, tracing::Level);

    impl tracing::field::Visit for MessageVisitor {
        fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
            if field.name() == "message" {
                self.0.lock().push(format!("{} {value:?}", self.1));
            }
        }
    }

    impl<S> tracing_subscriber::Layer<S> for WarningRecorder
    where
        S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
    {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _context: tracing_subscriber::layer::Context<'_, S>,
        ) {
            event.record(&mut MessageVisitor(
                self.0.clone(),
                *event.metadata().level(),
            ));
        }
    }

    fn seed(conn: &Connection, conversation: &str, message: &str) {
        conn.execute(
            "INSERT INTO ros_conversations (id, session_key, agent, created_at, updated_at)
             VALUES (?1, ?1, 'rivet', '2026-10-10T12:00:00.000Z', '2026-10-10T12:00:00.000Z')",
            [conversation],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, created_at)
             VALUES (?1, ?2, 'rivet', 'test', 'user', 'hello', '2026-10-10T12:00:00.000Z')",
            [message, conversation],
        )
        .unwrap();
    }

    #[test]
    fn a_conversation_that_cannot_be_read_is_skipped_with_a_warning() {
        use tracing_subscriber::layer::SubscriberExt as _;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("memory.sqlite");
        migrate::sqlite(&path).unwrap();
        let conn = Connection::open(&path).unwrap();
        seed(&conn, HEALTHY, "0199e2a0-7b3c-7a00-8001-0000000000a1");
        seed(&conn, UNREADABLE, "0199e2a0-7b3c-7a00-8001-0000000000b2");
        let lines: Arc<Mutex<Vec<String>>> = Arc::default();
        let subscriber = tracing_subscriber::registry().with(WarningRecorder(lines.clone()));
        let pending =
            tracing::subscriber::with_default(subscriber, || pending_conversations(&conn)).unwrap();
        let ids: Vec<String> = pending
            .iter()
            .map(|conversation| conversation.id.to_string())
            .collect();
        assert_eq!(ids, [HEALTHY]);
        let warnings = lines.lock().clone();
        let [warning] = warnings.as_slice() else {
            panic!("one warning: {warnings:?}");
        };
        assert!(
            warning.starts_with(&format!("WARN skipped pending conversation {UNREADABLE}: ")),
            "{warning}"
        );
    }
}
