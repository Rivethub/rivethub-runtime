use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueueError;
use kernel::queue::admin::MAINTENANCE_TASKS;
use kernel::queue::admin::QueueAdmin;
use uuid::Uuid;
use wire::JsonObject;

use crate::PostgresMemory;
use crate::error::Error;
use crate::graphile::GraphileQueue;
use crate::queue::QueueHealth;

impl PostgresMemory {
    pub async fn retry_failed(
        &self,
        tasks: &[String],
        error: Option<&str>,
        limit: Option<u32>,
        dry_run: bool,
    ) -> Result<u64, Error> {
        self.queue_ready().await?;
        let limit = limit.map(i64::from);
        if dry_run {
            count_rows(&self.pool, RETRY_COUNT, tasks, error, limit, "retry failed").await
        } else {
            let result = sqlx::query(RETRY_UPDATE)
                .bind(tasks)
                .bind(error)
                .bind(limit)
                .execute(&self.pool)
                .await
                .map_err(|source| Error::operation("retry failed", source))?;
            Ok(result.rows_affected())
        }
    }

    pub async fn requeue_failed(
        &self,
        task: &JobName,
        limit: u32,
        dry_run: bool,
    ) -> Result<u64, Error> {
        self.queue_ready().await?;
        if limit == 0 {
            return Err(Error::DumpMessage("requeue limit must be positive".into()));
        }
        let queue = GraphileQueue::new(self.pool.clone());
        let ids = queue.dead_job_ids(task, limit.min(1000)).await?;
        if dry_run {
            return Ok(u64::try_from(ids.len())?);
        }
        queue.reschedule(&ids, 10).await
    }

    pub async fn backfill_tool_synth(
        &self,
        limit: Option<u32>,
        dry_run: bool,
    ) -> Result<u64, Error> {
        self.queue_ready().await?;
        let ids = tool_messages(&self.pool, limit.map(i64::from)).await?;
        if dry_run {
            return Ok(u64::try_from(ids.len())?);
        }
        let queue = GraphileQueue::new(self.pool.clone());
        let mut enqueued = 0u64;
        for id in ids {
            queue.enqueue(&tool_job(id)?).await?;
            enqueued += 1;
        }
        Ok(enqueued)
    }

    pub async fn queue_status(&self) -> Result<Vec<QueueHealth>, Error> {
        let queue = self.queue().await?;
        let mut rows = Vec::with_capacity(MAINTENANCE_TASKS.len());
        for task in MAINTENANCE_TASKS {
            let name = JobName::Fixed(task);
            let health = queue.queue_health(&name).await?;
            rows.push(match health {
                Some(health) => health,
                None => empty_health(name),
            });
        }
        Ok(rows)
    }

    async fn queue_ready(&self) -> Result<(), Error> {
        if GraphileQueue::schema_present(&self.pool).await? {
            Ok(())
        } else {
            Err(Error::QueueAbsent)
        }
    }
}

fn empty_health(task: JobName) -> QueueHealth {
    kernel::queue::admin::empty_health(task)
}

#[async_trait::async_trait]
impl QueueAdmin for PostgresMemory {
    async fn admin_status(&self) -> Result<Vec<QueueHealth>, JobQueueError> {
        self.queue_status().await.map_err(JobQueueError::from)
    }

    async fn admin_retry(
        &self,
        tasks: &[String],
        error: Option<&str>,
        limit: Option<u32>,
        dry_run: bool,
    ) -> Result<u64, JobQueueError> {
        self.retry_failed(tasks, error, limit, dry_run)
            .await
            .map_err(JobQueueError::from)
    }

    async fn admin_requeue(
        &self,
        task: &JobName,
        limit: u32,
        dry_run: bool,
    ) -> Result<u64, JobQueueError> {
        self.requeue_failed(task, limit, dry_run)
            .await
            .map_err(JobQueueError::from)
    }
}

fn tool_job(id: Uuid) -> Result<Job, Error> {
    Ok(Job {
        name: JobName::Fixed(FixedJob::SynthesizeToolCall),
        payload: JsonObject::from_serialize(&kernel::memory::jobs::SynthesizeToolCall {
            message_id: id,
        })?,
        key: Some(format!("tool-synth-{id}")),
        key_mode: JobKeyMode::KeepRunAt,
        priority: 0,
        run_at: None,
        max_attempts: Some(3),
    })
}

async fn count_rows(
    pool: &sqlx::PgPool,
    sql: &'static str,
    tasks: &[String],
    error: Option<&str>,
    limit: Option<i64>,
    operation: &'static str,
) -> Result<u64, Error> {
    let row: CountRow = sqlx::query_as(sql)
        .bind(tasks)
        .bind(error)
        .bind(limit)
        .fetch_one(pool)
        .await
        .map_err(|source| Error::operation(operation, source))?;
    u64::try_from(row.count).map_err(|source| Error::protocol(source.to_string()))
}

async fn tool_messages(pool: &sqlx::PgPool, limit: Option<i64>) -> Result<Vec<Uuid>, Error> {
    let rows: Vec<MessageRow> = sqlx::query_as(TOOL_MESSAGES)
        .bind(limit)
        .fetch_all(pool)
        .await
        .map_err(|source| Error::operation("tool synth", source))?;
    Ok(rows.into_iter().map(|row| row.id).collect())
}

#[derive(Debug, sqlx::FromRow)]
struct CountRow {
    count: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct MessageRow {
    id: Uuid,
}

const RETRY_COUNT: &str = "\
SELECT count(*)::bigint AS count FROM (
    SELECT j2.id
      FROM graphile_worker._private_jobs j2
      JOIN graphile_worker._private_tasks t ON t.id = j2.task_id
     WHERE t.identifier = ANY($1::text[])
       AND j2.attempts >= j2.max_attempts
       AND ($2::text IS NULL OR strpos(coalesce(j2.last_error, ''), $2) > 0)
     ORDER BY j2.updated_at ASC
     LIMIT $3
) chosen";

const RETRY_UPDATE: &str = "\
WITH chosen AS (
    SELECT j2.id
      FROM graphile_worker._private_jobs j2
      JOIN graphile_worker._private_tasks t ON t.id = j2.task_id
     WHERE t.identifier = ANY($1::text[])
       AND j2.attempts >= j2.max_attempts
       AND ($2::text IS NULL OR strpos(coalesce(j2.last_error, ''), $2) > 0)
     ORDER BY j2.updated_at ASC
     LIMIT $3
)
UPDATE graphile_worker._private_jobs j
   SET attempts = 0,
       last_error = NULL,
       run_at = now(),
       updated_at = now(),
       locked_at = NULL,
       locked_by = NULL
  FROM chosen
 WHERE j.id = chosen.id";

const TOOL_MESSAGES: &str = "\
SELECT m.id
  FROM rh_messages m
 WHERE m.role = 'assistant'
   AND btrim(m.content) = ''
   AND m.tool_name IS NOT NULL
   AND NOT EXISTS (
        SELECT 1
          FROM graphile_worker._private_jobs j
          JOIN graphile_worker._private_tasks t ON t.id = j.task_id
         WHERE t.identifier = 'synthesize-tool-call'
           AND j.key = 'tool-synth-' || m.id::text
   )
 ORDER BY m.created_at ASC
 LIMIT $1";
