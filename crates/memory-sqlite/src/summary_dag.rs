use kernel::memory::MemoryMessageTally;
use kernel::memory::MemoryStoreError;
use kernel::memory::MemorySummaryNode;
use kernel::memory::SummaryDag;
use kernel::memory::SummaryKind;
use kernel::memory::SummaryOwner;
use kernel::memory::VectorState;
use rusqlite::Connection;
use rusqlite::OptionalExtension as _;
use rusqlite::Row;
use rusqlite::Statement;
use rusqlite::params_from_iter;
use rusqlite::types::Type;
use rusqlite::types::Value;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::time;

const SESSION_SQL: &str = "SELECT id, session_key FROM ros_conversations \
     WHERE session_key IN (SELECT value FROM json_each(?1)) \
     ORDER BY (active IS NOT 0) DESC, updated_at DESC, id LIMIT 1";

const CONVERSATION_SQL: &str = "SELECT id, session_key FROM ros_conversations WHERE id = ?1";

const DAG_SQL: &str = "WITH RECURSIVE dag(id, level) AS ( \
        SELECT id, 0 FROM ros_summaries WHERE conversation_id = ?1 \
        UNION \
        SELECT s.parent_id, dag.level + 1 FROM ros_summaries s JOIN dag ON dag.id = s.id \
         WHERE s.parent_id IS NOT NULL) \
    SELECT s.id, s.parent_id, max(dag.level) AS level, s.kind, s.content, s.message_count, \
           s.earliest_at, s.latest_at, s.created_at, \
           CASE WHEN s.embedding IS NOT NULL THEN 'embedded' \
                WHEN s.embed_status = 'failed' THEN 'failed' \
                WHEN s.embed_status = 'unembeddable' THEN 'skipped' \
                ELSE 'pending' END AS vector \
      FROM dag JOIN ros_summaries s ON s.id = dag.id \
     GROUP BY s.id \
     ORDER BY level DESC, s.latest_at, s.id";

const SOURCES_SQL: &str =
    "SELECT message_id FROM ros_summary_sources WHERE summary_id = ?1 ORDER BY ordinal";

const TALLY_SQL: &str = "SELECT count(*), \
        count(*) FILTER (WHERE role = 'user'), \
        count(*) FILTER (WHERE role = 'assistant'), \
        count(*) FILTER (WHERE role = 'system'), \
        count(*) FILTER (WHERE role = 'tool' OR tool_name IS NOT NULL), \
        count(embedding), \
        count(*) FILTER (WHERE embedding IS NULL AND embed_status IS NOT 'failed' \
                           AND embed_status IS NOT 'unembeddable'), \
        count(*) FILTER (WHERE embedding IS NULL AND embed_status = 'failed'), \
        count(*) FILTER (WHERE embedding IS NULL AND embed_status = 'unembeddable') \
      FROM ros_messages WHERE conversation_id = ?1";

struct DagRow {
    id: String,
    parent_id: Option<String>,
    level: i64,
    kind: SummaryKind,
    content: String,
    message_count: i64,
    earliest_at: Option<String>,
    latest_at: Option<String>,
    created_at: String,
    vector: VectorState,
}

impl SqliteStore {
    pub(crate) async fn read_summary_dag(
        &self,
        owner: &SummaryOwner,
    ) -> Result<SummaryDag, MemoryStoreError> {
        let owner = owner.clone();
        self.with_conn(move |conn| summary_dag(conn, owner))
            .await
            .map_err(MemoryStoreError::from)
    }
}

fn summary_dag(conn: &Connection, owner: SummaryOwner) -> Result<SummaryDag, SqliteError> {
    let (sql, key) = match owner {
        SummaryOwner::Session(session) => (
            SESSION_SQL,
            serde_json::to_string(&kernel::filing::stored_keys(&session)).map_err(|source| {
                SqliteError::Encode {
                    column: "session_key",
                    source,
                }
            })?,
        ),
        SummaryOwner::Conversation(id) => (CONVERSATION_SQL, id.to_string()),
    };
    let found = conn
        .query_row(sql, params_from_iter([Value::Text(key)]), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .optional()
        .map_err(SqliteError::query("summary dag"))?;
    let Some((conversation, session_key)) = found else {
        return Ok(SummaryDag::default());
    };
    let mut statement = conn
        .prepare(DAG_SQL)
        .map_err(SqliteError::query("summary dag"))?;
    let rows = statement
        .query_map([&conversation], dag_row)
        .map_err(SqliteError::query("summary dag"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("summary dag"))?;
    let mut sources = conn
        .prepare(SOURCES_SQL)
        .map_err(SqliteError::query("summary dag sources"))?;
    let summaries = rows
        .into_iter()
        .map(|row| summary_node(&mut sources, row))
        .collect::<Result<_, _>>()?;
    Ok(SummaryDag {
        session_key: Some(session_key),
        summaries,
        messages: message_tally(conn, &conversation)?,
    })
}

fn dag_row(row: &Row<'_>) -> rusqlite::Result<DagRow> {
    let kind = row.get::<_, String>(3)?;
    let vector = row.get::<_, String>(9)?;
    Ok(DagRow {
        id: row.get(0)?,
        parent_id: row.get(1)?,
        level: row.get(2)?,
        kind: kind.parse::<SummaryKind>().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(3, Type::Text, Box::new(error))
        })?,
        content: row.get(4)?,
        message_count: row.get(5)?,
        earliest_at: row.get(6)?,
        latest_at: row.get(7)?,
        created_at: row.get(8)?,
        vector: vector.parse::<VectorState>().map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(9, Type::Text, Box::new(error))
        })?,
    })
}

fn summary_node(
    sources: &mut Statement<'_>,
    row: DagRow,
) -> Result<MemorySummaryNode, SqliteError> {
    let source_message_ids = sources
        .query_map(params_from_iter([Value::Text(row.id.clone())]), |source| {
            source.get::<_, String>(0)
        })
        .map_err(SqliteError::query("summary dag sources"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("summary dag sources"))?;
    Ok(MemorySummaryNode {
        id: row.id,
        parent_id: row.parent_id,
        level: u32::try_from(row.level).map_err(SqliteError::Count)?,
        kind: row.kind,
        content: row.content,
        message_count: u32::try_from(row.message_count).map_err(SqliteError::Count)?,
        earliest_at: row
            .earliest_at
            .as_deref()
            .map(|text| time::stored(text, "earliest_at"))
            .transpose()?,
        latest_at: row
            .latest_at
            .as_deref()
            .map(|text| time::stored(text, "latest_at"))
            .transpose()?,
        created_at: time::stored(&row.created_at, "created_at")?,
        source_message_ids,
        vector: row.vector,
        truncated: false,
        children: None,
    })
}

fn message_tally(conn: &Connection, conversation: &str) -> Result<MemoryMessageTally, SqliteError> {
    let counts = conn
        .query_row(TALLY_SQL, [conversation], |row| {
            Ok([
                row.get::<_, i64>(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
            ])
        })
        .map_err(SqliteError::query("summary dag messages"))?;
    let [
        total,
        user,
        assistant,
        system,
        tool,
        embedded,
        pending,
        failed,
        skipped,
    ] = counts.map(|value| u32::try_from(value).map_err(SqliteError::Count));
    Ok(MemoryMessageTally {
        total: total?,
        user: user?,
        assistant: assistant?,
        system: system?,
        tool: tool?,
        embedded: embedded?,
        pending: pending?,
        failed: failed?,
        skipped: skipped?,
    })
}
