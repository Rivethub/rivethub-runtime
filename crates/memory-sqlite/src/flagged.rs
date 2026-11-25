use async_trait::async_trait;
use jiff::Timestamp;
use kernel::flagged::FLAG_AFTER_FAILURES;
use kernel::flagged::FlaggedError;
use kernel::flagged::FlaggedId;
use kernel::flagged::FlaggedJob;
use kernel::flagged::FlaggedJobs;
use rusqlite::Transaction;
use rusqlite::params;
use rusqlite::{Connection, OptionalExtension as _};
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
    error: String,
    failures: i64,
    first_failed_at: String,
    last_failed_at: String,
    flagged_at: String,
}

pub(crate) fn record_failure(
    conn: &Connection,
    id: &str,
    now: Timestamp,
) -> Result<i64, SqliteError> {
    let stamped = crate::time::event(now);
    conn.query_row(
        "INSERT INTO rh_job_failures (job_key, failures, first_failed_at, last_failed_at)
         SELECT COALESCE(job_key, 'job:' || id), 1, ?2, ?2 FROM ros_jobs WHERE id = ?1
         ON CONFLICT (job_key) DO UPDATE SET
             failures = rh_job_failures.failures + 1,
             last_failed_at = excluded.last_failed_at
         RETURNING failures",
        params![id, stamped],
        |row| row.get(0),
    )
    .optional()
    .map_err(SqliteError::query("count job failure"))?
    .ok_or_else(|| SqliteError::MissingJob { id: id.to_owned() })
}

pub(crate) fn key_is_flagged(conn: &Connection, id: &str) -> Result<bool, SqliteError> {
    let found: i64 = conn
        .query_row(
            "SELECT EXISTS (
                SELECT 1 FROM rh_flagged_jobs f
                  JOIN ros_jobs j ON j.id = ?1
                 WHERE j.job_key IS NOT NULL AND f.job_key = j.job_key
             )",
            [id],
            |row| row.get(0),
        )
        .map_err(SqliteError::query("flagged key"))?;
    Ok(found == 1)
}

pub(crate) fn archive_job(
    conn: &Connection,
    id: &str,
    error: &str,
    now: Timestamp,
) -> Result<(), SqliteError> {
    let stamped = crate::time::event(now);
    conn.execute(
        "INSERT INTO rh_flagged_jobs
            (task, job_key, payload, priority, max_attempts, failures, last_error,
             first_failed_at, last_failed_at, flagged_at)
         SELECT j.task, j.job_key, j.payload, COALESCE(p.priority, 0), j.max_attempts,
                MAX(COALESCE(f.failures, 0), j.attempts, 1), ?2,
                COALESCE(f.first_failed_at, j.updated_at),
                COALESCE(f.last_failed_at, j.updated_at), ?3
           FROM ros_jobs j
           LEFT JOIN rh_job_priorities p ON p.job_id = j.id
           LEFT JOIN rh_job_failures f ON f.job_key = COALESCE(j.job_key, 'job:' || j.id)
          WHERE j.id = ?1
         ON CONFLICT (job_key) DO UPDATE SET
            task = excluded.task, payload = excluded.payload, priority = excluded.priority,
            max_attempts = excluded.max_attempts, failures = excluded.failures,
            last_error = excluded.last_error, last_failed_at = excluded.last_failed_at,
            flagged_at = excluded.flagged_at, dismissed_at = NULL",
        params![id, error, stamped],
    )
    .map_err(SqliteError::query("flag job"))?;
    conn.execute(
        "DELETE FROM rh_job_failures
          WHERE job_key = 'job:' || ?1
            AND EXISTS (SELECT 1 FROM ros_jobs j WHERE j.id = ?1 AND j.job_key IS NULL)",
        [id],
    )
    .map_err(SqliteError::query("flag job"))?;
    conn.execute("DELETE FROM ros_jobs WHERE id = ?1", [id])
        .map_err(SqliteError::query("flag job"))?;
    Ok(())
}

#[doc(hidden)]
pub fn archive_for_test(
    conn: &Connection,
    id: &str,
    error: &str,
    now: Timestamp,
) -> Result<(), SqliteError> {
    archive_job(conn, id, error, now)
}

pub(crate) fn flag_stuck(conn: &Connection, now: Timestamp) -> Result<u64, SqliteError> {
    let stale = crate::time::event(crate::time::unlock_before(now)?);
    let tx = conn
        .unchecked_transaction()
        .map_err(SqliteError::query("flag stuck jobs"))?;
    let mut flagged = 0u64;
    for id in stuck_ids(&tx, &stale)? {
        if sweep_one(&tx, &id, now)? {
            flagged += 1;
        }
    }
    tx.commit().map_err(SqliteError::query("flag stuck jobs"))?;
    Ok(flagged)
}

fn stuck_ids(conn: &Connection, stale: &str) -> Result<Vec<String>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT id FROM ros_jobs
              WHERE (attempts >= max_attempts OR attempts >= ?1)
                AND task NOT LIKE 'run-task%'
                AND (locked_at IS NULL OR rh_instant(locked_at) < rh_instant(?2))
              ORDER BY id
              LIMIT 500",
        )
        .map_err(SqliteError::query("flag stuck jobs"))?;
    let rows = statement
        .query_map(params![i64::from(FLAG_AFTER_FAILURES), stale], |row| {
            row.get(0)
        })
        .map_err(SqliteError::query("flag stuck jobs"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("flag stuck jobs"))
}

fn sweep_one(tx: &Transaction<'_>, id: &str, now: Timestamp) -> Result<bool, SqliteError> {
    let found = tx
        .query_row(
            "SELECT task, payload, COALESCE(last_error, '') FROM ros_jobs WHERE id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            },
        )
        .optional()
        .map_err(SqliteError::query("flag stuck jobs"))?;
    let Some((task, payload, error)) = found else {
        return Ok(false);
    };
    if !crate::summary_replace::live_job(tx, &task, &payload)? {
        tx.execute("DELETE FROM ros_jobs WHERE id = ?1", [id])
            .map_err(SqliteError::query("discard expired summary job"))?;
        return Ok(false);
    }
    archive_job(tx, id, &error, now)?;
    Ok(true)
}

#[async_trait]
impl FlaggedJobs for SqliteStore {
    async fn flagged(&self) -> Result<Vec<FlaggedJob>, FlaggedError> {
        let rows = self.with_conn(dead_rows).await.map_err(unavailable)?;
        rows.into_iter().map(job).collect()
    }

    async fn retry(&self, id: FlaggedId) -> Result<(), FlaggedError> {
        let now = crate::time::stamp(Timestamp::now());
        let raw = i64::from(id);
        let changed = self
            .with_conn(move |conn| {
                let tx = crate::summary_replace::immediate(conn)
                    .map_err(SqliteError::query("retry flagged job"))?;
                let job = tx
                    .query_row(
                        "SELECT task, job_key, payload, priority, max_attempts
                           FROM rh_flagged_jobs WHERE id = ?1",
                        [raw],
                        |row| {
                            Ok(Revival {
                                task: row.get(0)?,
                                key: row.get(1)?,
                                payload: row.get(2)?,
                                priority: row.get(3)?,
                                max_attempts: row.get(4)?,
                            })
                        },
                    )
                    .optional()
                    .map_err(SqliteError::query("retry flagged job"))?;
                let Some(job) = job else {
                    return Ok(0);
                };
                if !crate::summary_replace::live_job(&tx, &job.task, &job.payload)? {
                    return Ok(0);
                }
                requeue(&tx, &job, &now)?;
                if let Some(key) = job.key.as_deref() {
                    tx.execute("DELETE FROM rh_job_failures WHERE job_key = ?1", [key])
                        .map_err(SqliteError::query("retry flagged job"))?;
                }
                let changed = tx
                    .execute("DELETE FROM rh_flagged_jobs WHERE id = ?1", [raw])
                    .map_err(SqliteError::query("retry flagged job"))?;
                tx.commit()
                    .map_err(SqliteError::query("retry flagged job"))?;
                Ok(changed)
            })
            .await
            .map_err(unavailable)?;
        if changed == 0 {
            return Err(FlaggedError::Missing(id));
        }
        self.notifications.send_replace(());
        Ok(())
    }

    async fn dismiss(&self, id: FlaggedId, at: Timestamp) -> Result<(), FlaggedError> {
        let raw = i64::from(id);
        let stamped = crate::time::stamp(at);
        let changed = self
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE rh_flagged_jobs SET dismissed_at = ?2
                      WHERE id = ?1 AND dismissed_at IS NULL",
                    params![raw, stamped],
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

    async fn holds_key(&self, key: &str) -> Result<bool, FlaggedError> {
        let key = key.to_owned();
        self.with_conn(move |conn| {
            conn.query_row(
                "SELECT EXISTS (SELECT 1 FROM rh_flagged_jobs WHERE job_key = ?1)",
                [key],
                |row| row.get(0),
            )
            .map_err(SqliteError::query("flagged job key"))
        })
        .await
        .map_err(unavailable)
    }
}

fn dead_rows(conn: &mut Connection) -> Result<Vec<DeadRow>, SqliteError> {
    let mut statement = conn
        .prepare(
            "SELECT j.id, j.task, j.job_key, COALESCE(c.title, w.title), j.payload,
                    j.last_error, j.failures, j.first_failed_at, j.last_failed_at, j.flagged_at
               FROM (SELECT id, task, job_key, payload, last_error, failures,
                            first_failed_at, last_failed_at, flagged_at,
                            CASE WHEN json_valid(payload) THEN payload ELSE '{}' END AS body
                       FROM rh_flagged_jobs WHERE dismissed_at IS NULL) AS j
               LEFT JOIN ros_messages m
                      ON m.id = COALESCE(json_extract(j.body, '$.messageId'),
                                         CASE WHEN json_extract(j.body, '$.targetTable') = 'ros_messages'
                                              THEN json_extract(j.body, '$.targetId') END)
               LEFT JOIN ros_conversations c
                      ON c.id = COALESCE(json_extract(j.body, '$.conversationId'), m.conversation_id)
               LEFT JOIN ros_wiki_topics w
                      ON w.slug = CASE WHEN json_extract(j.body, '$.targetTable') = 'ros_wiki_topics'
                                       THEN json_extract(j.body, '$.targetId') END
              ORDER BY j.last_failed_at DESC, j.id",
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
                failures: row.get(6)?,
                first_failed_at: row.get(7)?,
                last_failed_at: row.get(8)?,
                flagged_at: row.get(9)?,
            })
        })
        .map_err(SqliteError::query("flagged jobs"))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("flagged jobs"))
}

fn job(row: DeadRow) -> Result<FlaggedJob, FlaggedError> {
    let id = FlaggedId::new(row.id);
    let invalid = |reason: String| FlaggedError::Invalid { id, reason };
    let first_failed_at = crate::time::stored(&row.first_failed_at, "first_failed_at")
        .map_err(|error| invalid(error.to_string()))?;
    let last_failed_at = crate::time::stored(&row.last_failed_at, "last_failed_at")
        .map_err(|error| invalid(error.to_string()))?;
    let flagged_at = crate::time::stored(&row.flagged_at, "flagged_at")
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
        error: row.error,
        failures: u32::try_from(row.failures).map_err(|error| invalid(error.to_string()))?,
        first_failed_at: Some(first_failed_at),
        last_failed_at,
        flagged_at,
    })
}

struct Revival {
    task: String,
    key: Option<String>,
    payload: String,
    priority: i64,
    max_attempts: i64,
}

fn requeue(conn: &Connection, revival: &Revival, now: &str) -> Result<(), SqliteError> {
    let at = crate::time::stored(now, "run_at")?;
    let priority = i16::try_from(revival.priority).map_err(SqliteError::Count)?;
    let max_attempts = u32::try_from(revival.max_attempts).map_err(SqliteError::Count)?;
    let mut job = crate::queue::JobInsert::new(&revival.task, revival.payload.clone(), at);
    job.max_attempts = max_attempts;
    job.priority = priority;
    if let Some(key) = revival.key.as_deref() {
        job = job.key(key);
    }
    crate::queue::enqueue_in(conn, &job)
}

fn unavailable(error: SqliteError) -> FlaggedError {
    FlaggedError::Unavailable(Box::new(error))
}
