use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::flagged::FlaggedError;
use kernel::flagged::FlaggedId;
use kernel::flagged::FlaggedJob;
use kernel::flagged::FlaggedJobs;
use sqlx::PgPool;
use wire::JsonObject;

use crate::PostgresMemory;
use crate::error::Error;

const FAIL: &str = "WITH owned AS MATERIALIZED (
    SELECT id, job_queue_id, locked_by FROM graphile_worker._private_jobs
    WHERE id = $1 AND starts_with(locked_by, $3)
    FOR UPDATE
), failed AS (
    UPDATE graphile_worker._private_jobs j
    SET last_error = $2, locked_at = NULL, locked_by = NULL, updated_at = now(),
        run_at = greatest(now(), j.run_at) + interval '1 second' * exp(least(j.attempts, 10))
    FROM owned WHERE j.id = owned.id
    RETURNING j.id, j.key, j.attempts, j.max_attempts, owned.job_queue_id, owned.locked_by
), unlocked AS (
    UPDATE graphile_worker._private_job_queues q
    SET locked_at = NULL, locked_by = NULL
    FROM failed f WHERE q.id = f.job_queue_id AND q.locked_by = f.locked_by
), counted AS (
    INSERT INTO rh_job_failures (job_key, failures, first_failed_at, last_failed_at)
    SELECT coalesce(f.key, 'job:' || f.id), 1, now(), now() FROM failed f
    ON CONFLICT (job_key) DO UPDATE SET failures = rh_job_failures.failures + 1,
        last_failed_at = EXCLUDED.last_failed_at
    RETURNING failures
)
SELECT f.id, f.attempts >= f.max_attempts
           OR c.failures >= $4
           OR EXISTS (SELECT 1 FROM rh_flagged_jobs g WHERE g.job_key = f.key) AS flag
FROM failed f CROSS JOIN counted c";

const FLAG: &str = "WITH gone AS (
    DELETE FROM graphile_worker._private_jobs j
    WHERE j.id = ANY($1::bigint[])
      AND (j.locked_at IS NULL OR j.locked_at < now() - interval '4 hours')
    RETURNING j.id, j.key, j.task_id, j.payload, j.priority, j.max_attempts, j.attempts,
              j.last_error, j.updated_at
), ledger AS (
    SELECT g.id, c.failures, c.first_failed_at, c.last_failed_at
    FROM gone g JOIN rh_job_failures c ON c.job_key = coalesce(g.key, 'job:' || g.id)
), forgotten AS (
    DELETE FROM rh_job_failures c USING gone g
    WHERE g.key IS NULL AND c.job_key = 'job:' || g.id
), flagged AS (
    INSERT INTO rh_flagged_jobs (task, job_key, payload, priority, max_attempts, failures,
        last_error, first_failed_at, last_failed_at, flagged_at)
    SELECT t.identifier, g.key, g.payload::jsonb, g.priority, g.max_attempts,
           greatest(coalesce(l.failures, 0), g.attempts, 1), coalesce(g.last_error, ''),
           coalesce(l.first_failed_at, g.updated_at), coalesce(l.last_failed_at, g.updated_at), now()
    FROM gone g
    JOIN graphile_worker._private_tasks t ON t.id = g.task_id
    LEFT JOIN ledger l ON l.id = g.id
    ON CONFLICT (job_key) DO UPDATE SET task = EXCLUDED.task, payload = EXCLUDED.payload,
        priority = EXCLUDED.priority, max_attempts = EXCLUDED.max_attempts,
        failures = EXCLUDED.failures, last_error = EXCLUDED.last_error,
        last_failed_at = EXCLUDED.last_failed_at, flagged_at = EXCLUDED.flagged_at,
        dismissed_at = NULL
    RETURNING 1
)
SELECT count(*)::bigint FROM flagged";

const STUCK: &str = "SELECT j.id FROM graphile_worker._private_jobs j
    JOIN graphile_worker._private_tasks t ON t.id = j.task_id
    WHERE (j.attempts >= j.max_attempts OR j.attempts >= $1)
      AND t.identifier NOT LIKE 'run-task%'
      AND (j.locked_at IS NULL OR j.locked_at < now() - interval '4 hours')
    ORDER BY j.id LIMIT 500";

const RETRY: &str = "WITH taken AS (
    DELETE FROM rh_flagged_jobs WHERE id = $1
    RETURNING task, job_key, payload, priority, max_attempts
), cleared AS (
    DELETE FROM rh_job_failures c USING taken t WHERE c.job_key = t.job_key
)
SELECT (graphile_worker.add_job(t.task, t.payload::json, null, null, t.max_attempts,
        t.job_key, t.priority, null, 'replace')).id
FROM taken t";

#[derive(Debug, sqlx::FromRow)]
struct FailedRow {
    id: i64,
    flag: bool,
}

#[derive(Debug, sqlx::FromRow)]
struct FlaggedRow {
    id: i64,
    task: String,
    job_key: Option<String>,
    subject: Option<String>,
    payload: sqlx::types::Json<JsonObject>,
    last_error: String,
    failures: i32,
    first_failed_at: DateTime<Utc>,
    last_failed_at: DateTime<Utc>,
    flagged_at: DateTime<Utc>,
}

pub(crate) async fn fail(
    pool: &PgPool,
    id: i64,
    error: &str,
    worker: &str,
    after: u32,
) -> Result<(), Error> {
    let mut tx = pool
        .begin()
        .await
        .map_err(|source| Error::operation("fail", source))?;
    let failed: Option<FailedRow> = sqlx::query_as(FAIL)
        .bind(id)
        .bind(error)
        .bind(worker)
        .bind(i64::from(after))
        .fetch_optional(&mut *tx)
        .await
        .map_err(|source| Error::operation("fail", source))?;
    if let Some(failed) = failed.filter(|failed| failed.flag) {
        sqlx::query(FLAG)
            .bind(vec![failed.id])
            .execute(&mut *tx)
            .await
            .map_err(|source| Error::operation("flag job", source))?;
    }
    tx.commit()
        .await
        .map_err(|source| Error::operation("fail", source))
}

pub(crate) async fn flag_stuck(pool: &PgPool, after: u32) -> Result<u64, Error> {
    let ids: Vec<i64> = sqlx::query_scalar(STUCK)
        .bind(i64::from(after))
        .fetch_all(pool)
        .await
        .map_err(|source| Error::operation("stuck jobs", source))?;
    if ids.is_empty() {
        return Ok(0);
    }
    let flagged: i64 = sqlx::query_scalar(FLAG)
        .bind(ids)
        .fetch_one(pool)
        .await
        .map_err(|source| Error::operation("flag jobs", source))?;
    Ok(u64::try_from(flagged)?)
}

const FLAGGED: &str = "
    WITH open AS MATERIALIZED (
        SELECT f.id, f.task, f.job_key, f.payload, f.last_error, f.failures, f.first_failed_at,
               f.last_failed_at, f.flagged_at,
               CASE WHEN target.message ~* '^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$'
                    THEN target.message::uuid END AS message,
               CASE WHEN target.conversation ~* '^[0-9a-f]{8}(-[0-9a-f]{4}){3}-[0-9a-f]{12}$'
                    THEN target.conversation::uuid END AS conversation,
               target.page
          FROM rh_flagged_jobs f
         CROSS JOIN LATERAL (
               SELECT COALESCE(f.payload->>'messageId',
                               CASE WHEN f.payload->>'targetTable' = 'ros_messages'
                                    THEN f.payload->>'targetId' END) AS message,
                      f.payload->>'conversationId' AS conversation,
                      CASE WHEN f.payload->>'targetTable' = 'ros_wiki_topics'
                           THEN f.payload->>'targetId' END AS page) AS target
         WHERE f.dismissed_at IS NULL
    ), placed AS MATERIALIZED (
        SELECT o.*, COALESCE(o.conversation,
                   (SELECT m.conversation_id FROM rh_messages m WHERE m.id = o.message)) AS owner
          FROM open o
    )
    SELECT p.id, p.task, p.job_key,
           COALESCE((SELECT c.title FROM rh_conversations c WHERE c.id = p.owner),
                    (SELECT w.title FROM rh_wiki_topics w WHERE w.slug = p.page)) AS subject,
           p.payload, p.last_error, p.failures, p.first_failed_at, p.last_failed_at, p.flagged_at
      FROM placed p
     ORDER BY p.last_failed_at DESC, p.id";

#[async_trait]
impl FlaggedJobs for PostgresMemory {
    async fn flagged(&self) -> Result<Vec<FlaggedJob>, FlaggedError> {
        let rows: Vec<FlaggedRow> = sqlx::query_as(FLAGGED)
            .fetch_all(&self.pool)
            .await
            .map_err(unavailable)?;
        rows.into_iter().map(job).collect()
    }

    async fn retry(&self, id: FlaggedId) -> Result<(), FlaggedError> {
        let queued: Option<i64> = sqlx::query_scalar(RETRY)
            .bind(i64::from(id))
            .fetch_optional(&self.pool)
            .await
            .map_err(unavailable)?;
        queued.map(|_| ()).ok_or(FlaggedError::Missing(id))
    }

    async fn dismiss(&self, id: FlaggedId, at: DateTime<Utc>) -> Result<(), FlaggedError> {
        let dismissed = sqlx::query(
            "UPDATE rh_flagged_jobs SET dismissed_at = $2 WHERE id = $1 AND dismissed_at IS NULL",
        )
        .bind(i64::from(id))
        .bind(at)
        .execute(&self.pool)
        .await
        .map_err(unavailable)?;
        if dismissed.rows_affected() == 0 {
            return Err(FlaggedError::Missing(id));
        }
        Ok(())
    }
}

fn job(row: FlaggedRow) -> Result<FlaggedJob, FlaggedError> {
    let id = FlaggedId::new(row.id);
    Ok(FlaggedJob {
        id,
        task: row.task,
        key: row.job_key,
        subject: row.subject,
        payload: row.payload.0,
        error: row.last_error,
        failures: u32::try_from(row.failures).map_err(|error| FlaggedError::Invalid {
            id,
            reason: error.to_string(),
        })?,
        first_failed_at: Some(row.first_failed_at),
        last_failed_at: row.last_failed_at,
        flagged_at: row.flagged_at,
    })
}

fn unavailable(error: sqlx::Error) -> FlaggedError {
    FlaggedError::Unavailable(Box::new(error))
}
