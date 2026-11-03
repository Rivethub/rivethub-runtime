use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::flagged::FLAG_AFTER_FAILURES;
use kernel::flagged::FlaggedError;
use kernel::flagged::FlaggedId;
use kernel::flagged::FlaggedJob;
use kernel::flagged::FlaggedJobs;
use rusqlite::Connection;
use rusqlite::params;
use serde_json::Value;
use wire::JsonObject;

use crate::SqliteStore;
use crate::error::SqliteError;

#[derive(serde::Serialize)]
struct Unwrapped {
    value: Value,
}

struct DeadRow {
    id: i64,
    task: String,
    key: Option<String>,
    subject: Option<String>,
    payload: String,
    error: Option<String>,
    attempts: i64,
    updated_at: String,
}

pub(crate) fn flag_stuck(conn: &Connection, now: &str) -> Result<u64, SqliteError> {
    let changed = conn
        .execute(
            "UPDATE ros_jobs SET state = 'dead', updated_at = ?1
              WHERE state = 'queued' AND attempts >= ?2 AND task NOT LIKE 'run-task%'",
            params![now, FLAG_AFTER_FAILURES],
        )
        .map_err(SqliteError::query("flag stuck jobs"))?;
    u64::try_from(changed).map_err(SqliteError::Count)
}

#[async_trait]
impl FlaggedJobs for SqliteStore {
    async fn flagged(&self) -> Result<Vec<FlaggedJob>, FlaggedError> {
        let rows = self.with_conn(dead_rows).await.map_err(unavailable)?;
        rows.into_iter().map(job).collect()
    }

    async fn retry(&self, id: FlaggedId) -> Result<(), FlaggedError> {
        let now = crate::time::stamp(Utc::now());
        let raw = i64::from(id);
        let changed = self
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE ros_jobs
                        SET state = 'queued', attempts = 0, run_at = ?2, last_error = NULL,
                            locked_at = NULL, updated_at = ?2
                      WHERE rowid = ?1 AND state = 'dead'",
                    params![raw, now],
                )
                .map_err(SqliteError::query("retry flagged job"))
            })
            .await
            .map_err(unavailable)?;
        if changed == 0 {
            return Err(FlaggedError::Missing(id));
        }
        self.notifications.send_replace(());
        Ok(())
    }

    async fn dismiss(&self, id: FlaggedId, _at: DateTime<Utc>) -> Result<(), FlaggedError> {
        let raw = i64::from(id);
        let changed = self
            .with_conn(move |conn| {
                conn.execute(
                    "DELETE FROM ros_jobs WHERE rowid = ?1 AND state = 'dead'",
                    [raw],
                )
                .map_err(SqliteError::query("dismiss flagged job"))
            })
            .await
            .map_err(unavailable)?;
        if changed == 0 {
            return Err(FlaggedError::Missing(id));
        }
        Ok(())
    }
}

fn dead_rows(conn: &mut Connection) -> Result<Vec<DeadRow>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT j.id, j.task, j.job_key, COALESCE(c.title, w.title), j.payload,
                    j.last_error, j.attempts, j.updated_at
               FROM (SELECT rowid AS id, task, job_key, payload, last_error, attempts, updated_at,
                            CASE WHEN json_valid(payload) THEN payload ELSE '{}' END AS body
                       FROM ros_jobs WHERE state = 'dead') AS j
               LEFT JOIN ros_messages m
                      ON m.id = COALESCE(json_extract(j.body, '$.messageId'),
                                         CASE WHEN json_extract(j.body, '$.targetTable') = 'ros_messages'
                                              THEN json_extract(j.body, '$.targetId') END)
               LEFT JOIN ros_conversations c
                      ON c.id = COALESCE(json_extract(j.body, '$.conversationId'), m.conversation_id)
               LEFT JOIN ros_wiki_topics w
                      ON w.slug = CASE WHEN json_extract(j.body, '$.targetTable') = 'ros_wiki_topics'
                                       THEN json_extract(j.body, '$.targetId') END
              ORDER BY j.updated_at DESC, j.id",
        )
        .map_err(SqliteError::query("flagged jobs"))?;
    statement
        .query_map([], |row| {
            Ok(DeadRow {
                id: row.get(0)?,
                task: row.get(1)?,
                key: row.get(2)?,
                subject: row.get(3)?,
                payload: row.get(4)?,
                error: row.get(5)?,
                attempts: row.get(6)?,
                updated_at: row.get(7)?,
            })
        })
        .map_err(SqliteError::query("flagged jobs"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("flagged jobs"))
}

fn job(row: DeadRow) -> Result<FlaggedJob, FlaggedError> {
    let id = FlaggedId::new(row.id);
    let invalid = |reason: String| FlaggedError::Invalid { id, reason };
    let last_failed_at = crate::time::stored(&row.updated_at, "updated_at")
        .map_err(|error| invalid(error.to_string()))?;
    let payload = match serde_json::from_str::<Value>(&row.payload) {
        Ok(Value::Null) => Ok(JsonObject::default()),
        Ok(Value::Object(_)) => serde_json::from_str(&row.payload),
        Ok(value) => JsonObject::from_serialize(&Unwrapped { value }),
        Err(error) => Err(error),
    }
    .map_err(|error| invalid(error.to_string()))?;
    Ok(FlaggedJob {
        id,
        task: row.task,
        key: row.key,
        subject: row.subject,
        payload,
        error: row.error.unwrap_or_default(),
        failures: u32::try_from(row.attempts).map_err(|error| invalid(error.to_string()))?,
        first_failed_at: None,
        last_failed_at,
        flagged_at: last_failed_at,
    })
}

fn unavailable(error: SqliteError) -> FlaggedError {
    FlaggedError::Unavailable(Box::new(error))
}
