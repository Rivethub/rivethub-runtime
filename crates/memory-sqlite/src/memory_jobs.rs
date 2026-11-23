use async_trait::async_trait;
use jiff::SignedDuration;
use jiff::Timestamp;
use kernel::memory::jobs::{
    ChunkWrite, MemoryJobError, MemoryJobStore, MissingWiki, ToolCallMessage, UnchunkedMessage,
};
use kernel::memory::{EmbedTable, EmbedTarget, StoredChunk};
use kernel::queue::FixedJob;
use kernel::wiki::builder::ExtractWiki;
use rusqlite::OptionalExtension as _;
use rusqlite::TransactionBehavior;
use rusqlite::params;
use serde::Deserialize;
use uuid::Uuid;
use wire::JsonObject;
use wire::message::Role;

use crate::SqliteStore;
use crate::chunk_rows;
use crate::error::SqliteError;
use crate::job_rank::rank;
use crate::queue::JobInsert;
use crate::queue::JobState;
use crate::queue::insert_job;
use crate::time;

const REVIVE_AFTER_HOURS: i64 = 24;
const UNLOCK_AFTER_HOURS: i64 = 4;
const REVIVE_PRIORITY: i16 = 10;
const TOOL_ATTEMPTS: u32 = 3;

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ToolArguments {
    Object(JsonObject),
    Encoded(String),
}

#[async_trait]
impl MemoryJobStore for SqliteStore {
    async fn tool_call(&self, id: Uuid) -> Result<Option<ToolCallMessage>, MemoryJobError> {
        let row = self
            .with_conn(move |conn| load_tool_call(conn, id))
            .await
            .map_err(job_err)?;
        let Some(row) = row else {
            return Ok(None);
        };
        Ok(Some(ToolCallMessage {
            id,
            tool_name: row.tool_name,
            tool_args: arguments(row.tool_args)?,
            content: row.content,
        }))
    }

    async fn write_tool_sentence(
        &self,
        id: Uuid,
        previous_content: &str,
        content: &str,
    ) -> Result<(), MemoryJobError> {
        let previous_content = previous_content.to_owned();
        let content = content.to_owned();
        self.with_conn(move |conn| write_sentence(conn, id, &previous_content, &content))
            .await
            .map_err(job_err)
    }

    async fn unchunked(
        &self,
        limit: u32,
        chars_per_chunk: u32,
        tool_result_cap: u32,
    ) -> Result<Vec<UnchunkedMessage>, MemoryJobError> {
        self.with_conn(move |conn| unchunked_rows(conn, limit, chars_per_chunk, tool_result_cap))
            .await
            .map_err(job_err)
    }

    async fn write_chunks(
        &self,
        message_id: Uuid,
        chunks: Vec<StoredChunk>,
    ) -> Result<ChunkWrite, MemoryJobError> {
        let now = self.now();
        self.with_conn(move |conn| write_chunk_rows(conn, message_id, &chunks, now))
            .await
            .map_err(job_err)
    }

    async fn unembedded_chunks(
        &self,
        message_id: Uuid,
    ) -> Result<Vec<EmbedTarget>, MemoryJobError> {
        self.with_conn(move |conn| chunk_targets(conn, message_id))
            .await
            .map_err(job_err)
    }

    async fn revive_dead_wiki(&self, limit: u32) -> Result<u64, MemoryJobError> {
        let now = self.now();
        self.with_conn(move |conn| revive_wiki(conn, limit, now))
            .await
            .map_err(job_err)
    }

    async fn missing_wiki(&self, limit: u32) -> Result<Vec<MissingWiki>, MemoryJobError> {
        self.with_conn(move |conn| missing_rows(conn, limit))
            .await
            .map_err(job_err)
    }
}

impl SqliteStore {
    pub async fn backfill_tool_synth(
        &self,
        limit: Option<u32>,
        dry_run: bool,
    ) -> Result<u64, SqliteError> {
        let now = self.now();
        let cap = match limit {
            Some(limit) => i64::from(limit),
            None => -1,
        };
        self.with_conn(move |conn| backfill_tools(conn, cap, dry_run, now))
            .await
    }
}

struct LoadedTool {
    tool_name: Option<String>,
    tool_args: Option<String>,
    content: String,
}

fn load_tool_call(
    conn: &rusqlite::Connection,
    id: Uuid,
) -> Result<Option<LoadedTool>, SqliteError> {
    conn.query_row(
        "SELECT tool_name, tool_args, content FROM ros_messages WHERE id = ?1",
        [id.to_string()],
        |row| {
            Ok(LoadedTool {
                tool_name: row.get(0)?,
                tool_args: row.get(1)?,
                content: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(SqliteError::query("tool call"))
}

fn arguments(text: Option<String>) -> Result<JsonObject, MemoryJobError> {
    let Some(text) = text.filter(|text| !text.trim().is_empty() && text != "null") else {
        return Ok(JsonObject::default());
    };
    let parsed: ToolArguments =
        serde_json::from_str(&text).map_err(|error| MemoryJobError::Arguments(Box::new(error)))?;
    match parsed {
        ToolArguments::Object(object) => Ok(object),
        ToolArguments::Encoded(encoded) if encoded.trim().is_empty() => Ok(JsonObject::default()),
        ToolArguments::Encoded(encoded) => serde_json::from_str(&encoded)
            .map_err(|error| MemoryJobError::Arguments(Box::new(error))),
    }
}

fn write_sentence(
    conn: &rusqlite::Connection,
    id: Uuid,
    previous_content: &str,
    content: &str,
) -> Result<(), SqliteError> {
    conn.execute(
        "UPDATE ros_messages SET content = ?2 WHERE id = ?1 AND content = ?3",
        params![id.to_string(), content, previous_content],
    )
    .map(|_| ())
    .map_err(SqliteError::query("tool sentence"))
}

fn unchunked_rows(
    conn: &rusqlite::Connection,
    limit: u32,
    chars_per_chunk: u32,
    tool_result_cap: u32,
) -> Result<Vec<UnchunkedMessage>, SqliteError> {
    let whitespace = whitespace_chars();
    let mut statement = conn
        .prepare(
            "SELECT id, content, tool_result FROM (
                 SELECT m.id, m.content, m.tool_result, m.created_at,
                        length(trim(coalesce(m.content, ''), ?1)) AS content_chars,
                        length(trim(coalesce(m.tool_result, ''), ?1)) AS tool_chars
                   FROM ros_messages m
                  WHERE NOT EXISTS (
                            SELECT 1 FROM rh_message_chunks c WHERE c.message_id = m.id)
                    AND coalesce(m.embed_status, '') <> 'unembeddable'
             )
             WHERE (CASE WHEN content_chars = 0 THEN min(tool_chars, ?2)
                         WHEN tool_chars = 0 THEN content_chars
                         ELSE content_chars + min(tool_chars, ?2) + 1 END) > ?3
             ORDER BY created_at DESC
             LIMIT ?4",
        )
        .map_err(SqliteError::query("unchunked messages"))?;
    let rows = statement
        .query_map(
            params![
                whitespace,
                i64::from(tool_result_cap),
                i64::from(chars_per_chunk),
                i64::from(limit)
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(SqliteError::query("unchunked messages"))?;
    let raw = rows
        .collect::<Result<Vec<(String, String, Option<String>)>, _>>()
        .map_err(SqliteError::query("unchunked messages"))?;
    raw.into_iter()
        .map(|(id, content, tool_result)| {
            Ok(UnchunkedMessage {
                id: parse_id(id)?,
                content,
                tool_result,
            })
        })
        .collect()
}

fn whitespace_chars() -> String {
    [' ', '\t', '\n', '\r', '\u{000c}', '\u{000b}']
        .into_iter()
        .collect()
}

fn write_chunk_rows(
    conn: &mut rusqlite::Connection,
    message_id: Uuid,
    chunks: &[StoredChunk],
    now: Timestamp,
) -> Result<ChunkWrite, SqliteError> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(SqliteError::query("write chunks"))?;
    let message = message_id.to_string();
    let present: Option<i64> = tx
        .query_row(
            "SELECT 1 FROM ros_messages WHERE id = ?1",
            [&message],
            |row| row.get(0),
        )
        .optional()
        .map_err(SqliteError::query("write chunks"))?;
    if present.is_none() {
        return Ok(ChunkWrite::Skipped);
    }
    let existing: i64 = tx
        .query_row(
            "SELECT count(*) FROM rh_message_chunks WHERE message_id = ?1",
            [&message],
            |row| row.get(0),
        )
        .map_err(SqliteError::query("write chunks"))?;
    if existing != 0 {
        return Ok(ChunkWrite::Skipped);
    }
    for chunk in chunks {
        chunk_rows::insert(&tx, &message, chunk, now)?;
    }
    tx.commit().map_err(SqliteError::query("write chunks"))?;
    Ok(ChunkWrite::Written)
}

fn chunk_targets(
    conn: &rusqlite::Connection,
    message_id: Uuid,
) -> Result<Vec<EmbedTarget>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT id FROM rh_message_chunks \
              WHERE message_id = ?1 AND embedding IS NULL ORDER BY idx",
        )
        .map_err(SqliteError::query("unembedded chunks"))?;
    let ids = statement
        .query_map([message_id.to_string()], |row| row.get::<_, String>(0))
        .map_err(SqliteError::query("unembedded chunks"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("unembedded chunks"))?;
    Ok(ids
        .into_iter()
        .map(|id| EmbedTarget {
            table: EmbedTable::MessageChunks,
            id,
        })
        .collect())
}

fn revive_wiki(
    conn: &rusqlite::Connection,
    limit: u32,
    now: Timestamp,
) -> Result<u64, SqliteError> {
    let stale = time::stamp(shift(now, REVIVE_AFTER_HOURS)?);
    let unlocked = time::stamp(shift(now, UNLOCK_AFTER_HOURS)?);
    let task = FixedJob::ExtractWiki.to_string();
    let dead = JobState::Dead.to_string();
    let mut statement = conn
        .prepare(
            "SELECT id FROM ros_jobs
              WHERE task = ?1 AND state = ?2 AND attempts >= max_attempts AND job_key IS NOT NULL
                AND rh_instant(updated_at) < rh_instant(?3)
                AND (locked_at IS NULL OR rh_instant(locked_at) < rh_instant(?4))
              ORDER BY run_at ASC
              LIMIT ?5",
        )
        .map_err(SqliteError::query("revive wiki"))?;
    let ids = statement
        .query_map(
            params![task, dead, stale, unlocked, i64::from(limit)],
            |row| row.get::<_, String>(0),
        )
        .map_err(SqliteError::query("revive wiki"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("revive wiki"))?;
    let mut revived = 0u64;
    for id in ids {
        revived += u64::from(u8::from(requeue_wiki(conn, &id, now)?));
    }
    Ok(revived)
}

fn requeue_wiki(
    conn: &rusqlite::Connection,
    id: &str,
    now: Timestamp,
) -> Result<bool, SqliteError> {
    let stamped = time::stamp(now);
    let queued = JobState::Queued.to_string();
    let dead = JobState::Dead.to_string();
    let changed = conn
        .execute(
            "UPDATE ros_jobs
                SET state = ?2, attempts = 0, locked_at = NULL, last_error = NULL,
                    run_at = ?3, updated_at = ?3
              WHERE id = ?1 AND state = ?4",
            params![id, queued, stamped, dead],
        )
        .map_err(SqliteError::query("revive wiki"))?;
    if changed == 1 {
        rank(conn, id, REVIVE_PRIORITY)?;
        Ok(true)
    } else {
        Ok(false)
    }
}

fn missing_rows(conn: &rusqlite::Connection, limit: u32) -> Result<Vec<MissingWiki>, SqliteError> {
    let kind = kernel::memory::SummaryKind::Leaf.to_string();
    let mut statement = conn
        .prepare(
            "SELECT s.id, s.conversation_id, s.created_at
               FROM ros_summaries s
               LEFT JOIN ros_conversations c ON c.id = s.conversation_id
              WHERE s.kind = ?1
                AND (c.id IS NULL OR c.session_key IS NULL OR c.session_key NOT GLOB 'heartbeat:*')
                AND NOT EXISTS (SELECT 1 FROM ros_wiki_extractions e WHERE e.summary_id = s.id)
                AND NOT EXISTS (
                        SELECT 1 FROM ros_jobs j WHERE j.job_key = 'wiki-ext-' || s.id)
                AND NOT EXISTS (
                        SELECT 1 FROM rh_flagged_jobs f WHERE f.job_key = 'wiki-ext-' || s.id)
              ORDER BY s.created_at DESC
              LIMIT ?2",
        )
        .map_err(SqliteError::query("missing wiki"))?;
    let rows = statement
        .query_map(params![kind, i64::from(limit)], read_missing)
        .map_err(SqliteError::query("missing wiki"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("missing wiki"))
}

fn read_missing(row: &rusqlite::Row<'_>) -> rusqlite::Result<MissingWiki> {
    let summary = row.get::<_, String>(0)?;
    let conversation = row.get::<_, Option<String>>(1)?;
    let created_at = row.get::<_, String>(2)?;
    let summary_id = parse_id(summary).map_err(rusqlite_uuid)?;
    let conversation_id = conversation
        .map(|id| parse_id(id).map_err(rusqlite_uuid))
        .transpose()?;
    let summarized_at = time::stored(&created_at, "created_at").map_err(rusqlite_time)?;
    Ok(MissingWiki {
        job: ExtractWiki {
            summary_id,
            conversation_id,
        },
        summarized_at,
    })
}

fn backfill_tools(
    conn: &mut rusqlite::Connection,
    cap: i64,
    dry_run: bool,
    now: Timestamp,
) -> Result<u64, SqliteError> {
    let role = Role::Assistant.to_string();
    let task = FixedJob::SynthesizeToolCall.to_string();
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(SqliteError::query("tool synth"))?;
    let ids = tool_ids(&tx, &role, &task, cap)?;
    if dry_run {
        tx.rollback().map_err(SqliteError::query("tool synth"))?;
        return count(ids.len());
    }
    let mut enqueued = 0u64;
    for id in ids {
        if queue_tool(&tx, &id, now)? {
            enqueued += 1;
        }
    }
    tx.commit().map_err(SqliteError::query("tool synth"))?;
    Ok(enqueued)
}

fn tool_ids(
    conn: &rusqlite::Connection,
    role: &str,
    task: &str,
    cap: i64,
) -> Result<Vec<String>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT id FROM ros_messages
              WHERE role = ?1
                AND trim(content) = ''
                AND tool_name IS NOT NULL
                AND NOT EXISTS (
                        SELECT 1 FROM ros_jobs
                         WHERE task = ?2 AND job_key = 'tool-synth-' || ros_messages.id)
              ORDER BY created_at ASC
              LIMIT ?3",
        )
        .map_err(SqliteError::query("tool synth"))?;
    let mapped = statement
        .query_map(params![role, task, cap], |row| row.get::<_, String>(0))
        .map_err(SqliteError::query("tool synth"))?;
    mapped
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("tool synth"))
}

fn queue_tool(conn: &rusqlite::Connection, id: &str, now: Timestamp) -> Result<bool, SqliteError> {
    let message_id = parse_id(id.to_owned())?;
    let payload = serde_json::to_string(&kernel::memory::jobs::SynthesizeToolCall { message_id })
        .map_err(|source| SqliteError::Encode {
        column: "payload",
        source,
    })?;
    let mut job = JobInsert::new(FixedJob::SynthesizeToolCall.to_string(), payload, now)
        .key(format!("tool-synth-{message_id}"));
    job.max_attempts = TOOL_ATTEMPTS;
    Ok(insert_job(conn, &job)?.is_some())
}

fn shift(now: Timestamp, hours: i64) -> Result<Timestamp, SqliteError> {
    let seconds = hours.saturating_mul(3_600);
    now.checked_sub(SignedDuration::from_secs(seconds))
        .map_err(|_| SqliteError::Clock)
}

fn count(len: usize) -> Result<u64, SqliteError> {
    u64::try_from(len).map_err(SqliteError::Count)
}

fn parse_id(text: String) -> Result<Uuid, SqliteError> {
    Uuid::parse_str(&text).map_err(|_| SqliteError::StoredValue {
        column: "id",
        value: text,
    })
}

fn rusqlite_uuid(error: SqliteError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

fn rusqlite_time(error: SqliteError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(error))
}

fn job_err(error: SqliteError) -> MemoryJobError {
    MemoryJobError::Query(Box::new(error))
}
