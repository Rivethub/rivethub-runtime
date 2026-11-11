use std::path::Path;

use chrono::DateTime;
use chrono::SecondsFormat;
use kernel::transcript::TranscriptReaderError;
use rusqlite::Connection;
use rusqlite::OpenFlags;
use rusqlite::Row;
use rusqlite::types::Value as SqlValue;
use thiserror::Error;
use wire::message::Role;

use crate::listing::time::RawTime;
use crate::listing::time::UNKNOWN_TIME;
use crate::listing::time::epoch_flex;

const HERMES_SESSIONS: &str = "SELECT DISTINCT session_id FROM messages ORDER BY session_id";

const HERMES_MESSAGES: &str = "\
SELECT rowid AS rowid, session_id, role, content, timestamp \
FROM messages \
WHERE session_id = ?1 AND rowid > ?2 \
ORDER BY timestamp ASC, rowid ASC";

const HERMES_SESSION_ROWS: &str = "\
SELECT s.id AS id, s.started_at AS started_at, s.ended_at AS ended_at, \
       (SELECT m.content FROM messages m WHERE m.session_id = s.id AND m.role = 'user' \
        ORDER BY m.timestamp ASC LIMIT 1) AS title \
FROM sessions s";

const RECENT_HERMES: &str = " WHERE EXISTS (SELECT 1 FROM messages m WHERE m.session_id = s.id AND m.role = 'user') \
ORDER BY COALESCE(s.ended_at, s.started_at) DESC LIMIT ?1";
const ONE_HERMES: &str =
    " WHERE s.id = ?1 ORDER BY COALESCE(s.ended_at, s.started_at) DESC LIMIT 1";

const OPENCODE_SESSION_ROWS: &str =
    "SELECT id, title, model, time_created, time_updated FROM session";
const OPENCODE_SESSION_ROWS_WITHOUT_MODEL: &str =
    "SELECT id, title, NULL AS model, time_created, time_updated FROM session";
const RECENT_OPENCODE: &str = " WHERE EXISTS (SELECT 1 FROM message WHERE message.session_id = session.id) \
ORDER BY time_updated DESC LIMIT ?1";
const RECENT_OPENCODE_EVENTS: &str = " WHERE EXISTS (SELECT 1 FROM session_message WHERE session_message.session_id = session_v2.id) \
ORDER BY time_updated DESC LIMIT ?1";
const ONE_OPENCODE: &str = " WHERE id = ?1 ORDER BY time_updated DESC LIMIT 1";

const OPENCODE_SESSIONS: &str = "SELECT DISTINCT session_id FROM message ORDER BY session_id";

const OPENCODE_EVENT_SESSION_ROWS: &str =
    "SELECT id, title, model, time_created, time_updated FROM session_v2";

const OPENCODE_EVENT_SESSIONS: &str =
    "SELECT DISTINCT session_id FROM session_message ORDER BY session_id";

const OPENCODE_EVENT_MESSAGES: &str = "\
SELECT seq, type, time_created, data \
FROM session_message \
WHERE session_id = ?1 AND seq >= ?2 \
ORDER BY seq ASC";

const OPENCODE_LAYOUT: &str =
    "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'session_v2'";

const OPENCODE_PARTS: &str = "\
SELECT part.rowid AS rowid, \
       part.id AS part_id, \
       part.session_id AS session_id, \
       part.message_id AS message_id, \
       message.role AS role, \
       part.type AS part_type, \
       part.text AS text, \
       part.tool_name AS tool_name, \
       part.tool_arguments AS tool_arguments, \
       part.tool_result AS tool_result, \
       NULL AS tool_status, \
       part.created_at AS created_at \
FROM part \
LEFT JOIN message ON message.id = part.message_id \
WHERE part.session_id = ?1 AND part.rowid > ?2 \
ORDER BY part.created_at ASC, part.rowid ASC";

const OPENCODE_DOCUMENTS: &str =
    "SELECT COUNT(*) FROM pragma_table_info('part') WHERE name = 'data'";

const OPENCODE_JSON_PARTS: &str = "\
SELECT part.rowid AS rowid, \
       part.id AS part_id, \
       part.session_id AS session_id, \
       part.message_id AS message_id, \
       json_extract(message.data, '$.role') AS role, \
       json_extract(part.data, '$.type') AS part_type, \
       json_extract(part.data, '$.text') AS text, \
       json_extract(part.data, '$.tool') AS tool_name, \
       json_extract(part.data, '$.state.input') AS tool_arguments, \
       COALESCE(json_extract(part.data, '$.state.output'), json_extract(part.data, '$.state.error')) AS tool_result, \
       json_extract(part.data, '$.state.status') AS tool_status, \
       part.time_created AS created_at \
FROM part \
LEFT JOIN message ON message.id = part.message_id \
WHERE part.session_id = ?1 AND part.rowid > ?2 \
ORDER BY part.time_created ASC, part.rowid ASC";

#[derive(Debug, Error)]
pub(crate) enum HarnessDbError {
    #[error("harness database query failed: {message}")]
    Query {
        message: String,
        #[source]
        source: kernel::BoxError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DbRows<T> {
    Ready(Vec<T>),
    Unreadable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Speaker {
    Known(Role),
    Unrecognized { text: String, parse_error: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HermesMessage {
    pub(crate) row_id: i64,
    pub(crate) session_id: String,
    pub(crate) speaker: Speaker,
    pub(crate) content: String,
    pub(crate) at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PartKind {
    Text,
    Tool,
    Reasoning,
    Ignored,
    Unrecognized,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpencodePart {
    pub(crate) row_id: i64,
    pub(crate) part_id: String,
    pub(crate) session_id: String,
    pub(crate) message_id: String,
    pub(crate) speaker: Speaker,
    pub(crate) kind: PartKind,
    pub(crate) text: Option<String>,
    pub(crate) tool_name: Option<String>,
    pub(crate) tool_arguments: Option<String>,
    pub(crate) tool_result: Option<String>,
    pub(crate) tool_failed: bool,
    pub(crate) at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OpencodeEvent {
    pub(crate) seq: i64,
    pub(crate) record_type: String,
    pub(crate) time_created: i64,
    pub(crate) data: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OpencodeTranscript {
    Parts(Vec<OpencodePart>),
    Events(Vec<OpencodeEvent>),
    Unreadable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct OpencodeCursor {
    pub(crate) after_row: i64,
    pub(crate) from_seq: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpencodeLayout {
    Parts,
    JsonParts,
    Events,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionQuery<'a> {
    Recent(usize),
    One(&'a str),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct HermesSessionRow {
    pub(crate) id: SqlValue,
    pub(crate) started_at: SqlValue,
    pub(crate) ended_at: SqlValue,
    pub(crate) title: SqlValue,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct OpencodeSessionRow {
    pub(crate) id: SqlValue,
    pub(crate) title: SqlValue,
    pub(crate) model: SqlValue,
    pub(crate) time_created: SqlValue,
    pub(crate) time_updated: SqlValue,
}

pub(crate) fn hermes_session_rows(
    path: &Path,
    query: SessionQuery<'_>,
) -> Result<DbRows<HermesSessionRow>, HarnessDbError> {
    let Opened::Ready(connection) = open_read(path) else {
        return Ok(DbRows::Unreadable);
    };
    let rows = session_rows(
        &connection,
        HERMES_SESSION_ROWS,
        RECENT_HERMES,
        ONE_HERMES,
        query,
        |row| {
            Ok(HermesSessionRow {
                id: row.get("id")?,
                started_at: row.get("started_at")?,
                ended_at: row.get("ended_at")?,
                title: row.get("title")?,
            })
        },
    );
    rows.map(DbRows::Ready)
        .map_err(|error| query_error(&error.to_string(), Box::new(error)))
}

pub(crate) fn opencode_session_rows(
    path: &Path,
    query: SessionQuery<'_>,
) -> Result<DbRows<OpencodeSessionRow>, HarnessDbError> {
    let Opened::Ready(connection) = open_read(path) else {
        return Ok(DbRows::Unreadable);
    };
    let read = |row: &Row<'_>| {
        Ok(OpencodeSessionRow {
            id: row.get("id")?,
            title: row.get("title")?,
            model: row.get("model")?,
            time_created: row.get("time_created")?,
            time_updated: row.get("time_updated")?,
        })
    };
    let rows = opencode_layout(&connection).and_then(|layout| match layout {
        OpencodeLayout::Events => session_rows(
            &connection,
            OPENCODE_EVENT_SESSION_ROWS,
            RECENT_OPENCODE_EVENTS,
            ONE_OPENCODE,
            query,
            read,
        ),
        OpencodeLayout::Parts | OpencodeLayout::JsonParts => {
            part_session_rows(&connection, query, read)
        }
    });
    rows.map(DbRows::Ready)
        .map_err(|error| query_error(&error.to_string(), Box::new(error)))
}

fn part_session_rows(
    connection: &Connection,
    query: SessionQuery<'_>,
    read: impl Fn(&Row<'_>) -> rusqlite::Result<OpencodeSessionRow>,
) -> rusqlite::Result<Vec<OpencodeSessionRow>> {
    match session_rows(
        connection,
        OPENCODE_SESSION_ROWS,
        RECENT_OPENCODE,
        ONE_OPENCODE,
        query,
        &read,
    ) {
        Ok(rows) => Ok(rows),
        Err(error) => {
            tracing::debug!(%error, "opencode session table read failed, retrying without the model column");
            session_rows(
                connection,
                OPENCODE_SESSION_ROWS_WITHOUT_MODEL,
                RECENT_OPENCODE,
                ONE_OPENCODE,
                query,
                read,
            )
        }
    }
}

fn opencode_layout(connection: &Connection) -> rusqlite::Result<OpencodeLayout> {
    let tables: i64 = connection.query_row(OPENCODE_LAYOUT, [], |row| row.get(0))?;
    if tables > 0 {
        return Ok(OpencodeLayout::Events);
    }
    let documents: i64 = connection.query_row(OPENCODE_DOCUMENTS, [], |row| row.get(0))?;
    Ok(if documents > 0 {
        OpencodeLayout::JsonParts
    } else {
        OpencodeLayout::Parts
    })
}

fn session_rows<T>(
    connection: &Connection,
    select: &str,
    recent: &str,
    one: &str,
    query: SessionQuery<'_>,
    read: impl Fn(&Row<'_>) -> rusqlite::Result<T>,
) -> rusqlite::Result<Vec<T>> {
    match query {
        SessionQuery::Recent(limit) => {
            let limit = i64::try_from(limit)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            connection
                .prepare(&format!("{select}{recent}"))?
                .query_map(rusqlite::params![limit], read)?
                .collect()
        }
        SessionQuery::One(id) => connection
            .prepare(&format!("{select}{one}"))?
            .query_map(rusqlite::params![id], read)?
            .collect(),
    }
}

#[derive(Debug)]
pub(crate) struct HarnessDb;

impl HarnessDb {
    pub(crate) async fn hermes_sessions(database: &Path) -> Result<DbRows<String>, HarnessDbError> {
        let database = database.to_path_buf();
        spawn(move || read_ids(&database, HERMES_SESSIONS)).await
    }

    pub(crate) async fn hermes_messages(
        database: &Path,
        session_id: &str,
        after_row: i64,
    ) -> Result<DbRows<HermesMessage>, HarnessDbError> {
        let database = database.to_path_buf();
        let session_id = session_id.to_owned();
        spawn(move || read_hermes(&database, &session_id, after_row)).await
    }

    pub(crate) async fn opencode_sessions(
        database: &Path,
    ) -> Result<DbRows<String>, HarnessDbError> {
        let database = database.to_path_buf();
        spawn(move || read_opencode_ids(&database)).await
    }

    pub(crate) async fn opencode_transcript(
        database: &Path,
        session_id: &str,
        cursor: OpencodeCursor,
    ) -> Result<OpencodeTranscript, HarnessDbError> {
        let database = database.to_path_buf();
        let session_id = session_id.to_owned();
        spawn_blocking(move || read_opencode(&database, &session_id, cursor)).await
    }
}

pub(crate) fn read_error(error: HarnessDbError) -> TranscriptReaderError {
    TranscriptReaderError::Read {
        message: error.to_string(),
        source: Box::new(error),
    }
}

enum Opened {
    Ready(Connection),
    Unreadable,
}

fn read_only() -> OpenFlags {
    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX
}

fn open_read(path: &Path) -> Opened {
    match Connection::open_with_flags(path, read_only()) {
        Ok(connection) => Opened::Ready(connection),
        Err(error) => {
            tracing::warn!(
                %error,
                path = %path.display(),
                "harness database could not be opened"
            );
            Opened::Unreadable
        }
    }
}

async fn spawn<T: Send + 'static>(
    work: impl FnOnce() -> Result<DbRows<T>, HarnessDbError> + Send + 'static,
) -> Result<DbRows<T>, HarnessDbError> {
    spawn_blocking(work).await
}

async fn spawn_blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, HarnessDbError> + Send + 'static,
) -> Result<T, HarnessDbError> {
    match tokio::task::spawn_blocking(work).await {
        Ok(result) => result,
        Err(error) => Err(query_error(
            "harness database worker stopped",
            Box::new(error),
        )),
    }
}

fn read_ids(path: &Path, sql: &str) -> Result<DbRows<String>, HarnessDbError> {
    let Opened::Ready(connection) = open_read(path) else {
        return Ok(DbRows::Unreadable);
    };
    rows_or_unreadable(session_ids(&connection, sql))
}

fn read_opencode_ids(path: &Path) -> Result<DbRows<String>, HarnessDbError> {
    let Opened::Ready(connection) = open_read(path) else {
        return Ok(DbRows::Unreadable);
    };
    let queried = opencode_layout(&connection).and_then(|layout| match layout {
        OpencodeLayout::Parts | OpencodeLayout::JsonParts => {
            session_ids(&connection, OPENCODE_SESSIONS)
        }
        OpencodeLayout::Events => session_ids(&connection, OPENCODE_EVENT_SESSIONS),
    });
    rows_or_unreadable(queried)
}

fn session_ids(connection: &Connection, sql: &str) -> rusqlite::Result<Vec<String>> {
    connection.prepare(sql).and_then(|mut statement| {
        statement
            .query_map([], |row| row.get::<_, String>("session_id"))?
            .collect::<Result<Vec<_>, _>>()
    })
}

fn read_hermes(
    path: &Path,
    session_id: &str,
    after_row: i64,
) -> Result<DbRows<HermesMessage>, HarnessDbError> {
    let Opened::Ready(connection) = open_read(path) else {
        return Ok(DbRows::Unreadable);
    };
    let queried = connection
        .prepare(HERMES_MESSAGES)
        .and_then(|mut statement| {
            statement
                .query_map(rusqlite::params![session_id, after_row], hermes_row)?
                .collect::<Result<Vec<_>, _>>()
        });
    rows_or_unreadable(queried)
}

fn hermes_row(row: &Row<'_>) -> rusqlite::Result<HermesMessage> {
    let role: String = row.get("role")?;
    Ok(HermesMessage {
        row_id: row.get("rowid")?,
        session_id: row.get("session_id")?,
        speaker: speaker_from_text(role),
        content: row.get("content")?,
        at: instant(&row.get("timestamp")?),
    })
}

fn instant(value: &SqlValue) -> Option<String> {
    let millis = epoch_flex(&RawTime::from_sql(value));
    DateTime::from_timestamp_millis(millis)
        .filter(|_| millis != UNKNOWN_TIME)
        .map(|at| at.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn read_opencode(
    path: &Path,
    session_id: &str,
    cursor: OpencodeCursor,
) -> Result<OpencodeTranscript, HarnessDbError> {
    let Opened::Ready(connection) = open_read(path) else {
        return Ok(OpencodeTranscript::Unreadable);
    };
    let queried = opencode_layout(&connection).and_then(|layout| match layout {
        OpencodeLayout::Parts | OpencodeLayout::JsonParts => connection
            .prepare(if layout == OpencodeLayout::JsonParts {
                OPENCODE_JSON_PARTS
            } else {
                OPENCODE_PARTS
            })?
            .query_map(
                rusqlite::params![session_id, cursor.after_row],
                opencode_row,
            )?
            .collect::<Result<Vec<_>, _>>()
            .map(OpencodeTranscript::Parts),
        OpencodeLayout::Events => connection
            .prepare(OPENCODE_EVENT_MESSAGES)?
            .query_map(rusqlite::params![session_id, cursor.from_seq], event_row)?
            .collect::<Result<Vec<_>, _>>()
            .map(OpencodeTranscript::Events),
    });
    readable(queried, OpencodeTranscript::Unreadable)
}

fn event_row(row: &Row<'_>) -> rusqlite::Result<OpencodeEvent> {
    Ok(OpencodeEvent {
        seq: row.get("seq")?,
        record_type: row.get("type")?,
        time_created: row.get("time_created")?,
        data: row.get("data")?,
    })
}

fn opencode_row(row: &Row<'_>) -> rusqlite::Result<OpencodePart> {
    let role: Option<String> = row.get("role")?;
    let kind: String = row.get("part_type")?;
    Ok(OpencodePart {
        row_id: row.get("rowid")?,
        part_id: row.get("part_id")?,
        session_id: row.get("session_id")?,
        message_id: row.get("message_id")?,
        speaker: speaker(role),
        kind: part_kind(&kind),
        text: row.get("text")?,
        tool_name: row.get("tool_name")?,
        tool_arguments: row.get("tool_arguments")?,
        tool_result: row.get("tool_result")?,
        tool_failed: row.get::<_, Option<String>>("tool_status")?.as_deref() == Some("error"),
        at: instant(&row.get("created_at")?),
    })
}

fn speaker_from_text(text: String) -> Speaker {
    match text.parse::<Role>() {
        Ok(role) => Speaker::Known(role),
        Err(error) => Speaker::Unrecognized {
            text,
            parse_error: error.to_string(),
        },
    }
}

fn part_kind(text: &str) -> PartKind {
    match text {
        "text" => PartKind::Text,
        "tool" => PartKind::Tool,
        "reasoning" => PartKind::Reasoning,
        "step-start" | "step-finish" | "patch" | "compaction" => PartKind::Ignored,
        _ => PartKind::Unrecognized,
    }
}

fn speaker(role: Option<String>) -> Speaker {
    match role {
        Some(text) => speaker_from_text(text),
        None => Speaker::Unrecognized {
            text: String::new(),
            parse_error: "part has no message row".to_owned(),
        },
    }
}

fn rows_or_unreadable<T>(
    result: Result<Vec<T>, rusqlite::Error>,
) -> Result<DbRows<T>, HarnessDbError> {
    readable(result.map(DbRows::Ready), DbRows::Unreadable)
}

fn readable<T>(result: Result<T, rusqlite::Error>, unreadable: T) -> Result<T, HarnessDbError> {
    match result {
        Ok(rows) => Ok(rows),
        Err(error) if not_a_database(&error) => {
            tracing::warn!(%error, "harness database is not a sqlite file");
            Ok(unreadable)
        }
        Err(error) => Err(query_error(&error.to_string(), Box::new(error))),
    }
}

fn not_a_database(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(failure, _)
            if failure.code == rusqlite::ErrorCode::NotADatabase
    )
}

fn query_error(message: &str, source: kernel::BoxError) -> HarnessDbError {
    HarnessDbError::Query {
        message: message.to_owned(),
        source,
    }
}
