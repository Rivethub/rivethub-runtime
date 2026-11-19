use jiff::Timestamp;
use kernel::filing::Filable;
use kernel::memory::summary_opening::{OpeningConsumer, opening_text};
use kernel::memory::summary_order::{MessagePosition, order_messages};
use kernel::memory::{MemoryBrowseOrder, needs_title};
use kernel::queue::Job;
use rusqlite::{Connection, OptionalExtension as _, params};
use uuid::Uuid;
use wire::{JsonObject, message::Role};

use crate::message::{stored_role, transcript_metadata};
use crate::queue::enqueue_in;
use crate::summary_port::parse_id;
use crate::{SqliteError, SqliteStore};

struct Turn {
    role: Role,
    metadata: Option<JsonObject>,
    created_at: Timestamp,
    content: String,
}

pub(crate) fn refresh(
    conn: &Connection,
    id: &str,
    now: Timestamp,
    language_available: bool,
) -> Result<bool, SqliteError> {
    let Some(content) = projection(conn, id)? else {
        return Ok(false);
    };
    let old = crate::title::load(conn, id)?;
    if old.as_ref().is_some_and(|old| old.content == content) {
        return Ok(false);
    }
    let (head, supplied) = crate::title::head(conn, id)?;
    let filing = Filable::of(&head) != Filable::Never;
    let title = filing && !supplied && needs_title(head.title.as_deref());
    conn.execute(
        "INSERT INTO rh_summary_openings (conversation_id, content, filing_armed, title_armed)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (conversation_id) DO UPDATE SET content = excluded.content",
        params![id, content, filing, title],
    )
    .map_err(failure)?;
    if !supplied {
        crate::title::seed(conn, &head, old.as_ref(), &content)?;
    }
    if old.is_some() || !language_available {
        return Ok(false);
    }
    for (arm, consumer) in [
        (filing, OpeningConsumer::Filing),
        (title, OpeningConsumer::Title),
    ] {
        if arm {
            let job = consumer.job(head.id).map_err(payload_error)?;
            let payload = serde_json::to_string(&job.payload).map_err(payload_error)?;
            let insert = crate::port::job_insert(&job, payload, now).map_err(|error| {
                SqliteError::Invalid {
                    message: error.to_string(),
                }
            })?;
            enqueue_in(conn, &insert)?;
        }
    }
    Ok(filing || title)
}

fn projection(conn: &Connection, id: &str) -> Result<Option<String>, SqliteError> {
    let mut stmt = conn
        .prepare(
            "SELECT role, metadata, created_at, CASE WHEN role = 'user' THEN content ELSE '' END
           FROM ros_messages WHERE conversation_id = ?1 ORDER BY rh_instant(created_at), id",
        )
        .map_err(failure)?;
    let rows = stmt
        .query_map([id], |row| {
            Ok((
                stored_role(row, 0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })
        .map_err(failure)?;
    let turns = rows
        .map(|row| {
            let (role, metadata, created_at, content) = row.map_err(failure)?;
            Ok(Turn {
                role,
                metadata: transcript_metadata(&metadata),
                created_at: crate::time::stored(&created_at, "created_at")?,
                content,
            })
        })
        .collect::<Result<Vec<_>, SqliteError>>()?;
    let ordered = order_messages(turns, MemoryBrowseOrder::Asc, |turn| {
        MessagePosition::new(turn.created_at, turn.role, turn.metadata.as_ref())
    });
    Ok(opening_text(
        ordered
            .iter()
            .map(|turn| (turn.role, turn.content.as_str())),
    ))
}

impl SqliteStore {
    pub(crate) async fn undelivered_openings(&self) -> Result<Vec<Job>, SqliteError> {
        let expired = crate::time::stamp(self.now() - jiff::SignedDuration::from_hours(4));
        self.with_conn(move |conn| {
            conn.execute(
                "DELETE FROM ros_jobs
                  WHERE task IN ('file-conversation', 'title-conversation')
                    AND state = 'running' AND locked_at < ?1",
                [expired],
            )
            .map_err(failure)?;
            let mut stmt = conn
                .prepare(
                    "WITH work AS (
                     SELECT conversation_id, 1 AS filing, 'file-' AS prefix
                       FROM rh_summary_openings WHERE filing_armed
                     UNION ALL
                     SELECT conversation_id, 0 AS filing, 'title-' AS prefix
                       FROM rh_summary_openings WHERE title_armed
                 )
                 SELECT work.conversation_id, work.filing FROM work
                  WHERE NOT EXISTS (
                      SELECT 1 FROM ros_jobs j
                       WHERE j.job_key = work.prefix || work.conversation_id AND j.state <> 'dead'
                  )
                  ORDER BY work.conversation_id, work.filing LIMIT 256",
                )
                .map_err(failure)?;
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?))
                })
                .map_err(failure)?;
            rows.map(|row| {
                let (id, filing) = row.map_err(failure)?;
                let consumer = if filing {
                    OpeningConsumer::Filing
                } else {
                    OpeningConsumer::Title
                };
                consumer.job(parse_id(&id)?).map_err(payload_error)
            })
            .collect()
        })
        .await
    }

    pub(crate) async fn opening_filing(&self, id: Uuid) -> Result<Option<bool>, SqliteError> {
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT filing_armed FROM rh_summary_openings WHERE conversation_id = ?1",
                [id.to_string()],
                |row| row.get(0),
            )
            .optional()
            .map_err(failure)
        })
        .await
    }

    pub(crate) async fn finish_opening_filing(&self, id: Uuid) -> Result<(), SqliteError> {
        self.with_conn(move |conn| {
            conn.execute(
                "UPDATE rh_summary_openings SET filing_armed = 0 WHERE conversation_id = ?1 AND filing_armed",
                [id.to_string()],
            ).map_err(failure)?;
            Ok(())
        }).await
    }
}

fn failure(source: rusqlite::Error) -> SqliteError {
    SqliteError::query("summary opening")(source)
}

fn payload_error(source: serde_json::Error) -> SqliteError {
    SqliteError::Encode {
        column: "opening job payload",
        source,
    }
}
