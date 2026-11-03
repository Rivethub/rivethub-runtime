use crate::error::Error;
use kernel::queue::ClaimedJob;
use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobId;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use sqlx::PgPool;
use std::str::FromStr;
use wire::JsonObject;

struct Notifications(crate::listen::Listener);

#[async_trait::async_trait]
impl kernel::queue::JobNotifications for Notifications {
    async fn recv(&mut self) -> Result<(), kernel::queue::JobQueueError> {
        match self.0.recv().await {
            Ok(()) => Ok(()),
            Err(crate::listen::Stopped) => Err(kernel::queue::JobQueueError::Unavailable(
                Box::new(crate::listen::Stopped),
            )),
        }
    }
}

#[derive(Debug, sqlx::FromRow)]
struct QueuedJob {
    id: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct RemovedJobs {
    removed: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct DeadJob {
    id: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct RescheduledJobs {
    rescheduled: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct ReapedJobs {
    count: i64,
}

#[derive(Debug, sqlx::FromRow)]
struct HealthRow {
    total: i64,
    pending: i64,
    running: i64,
    scheduled: i64,
    dead: i64,
    recent_dead: i64,
    oldest_pending_minutes: Option<i64>,
    last_error: Option<String>,
}

#[derive(Debug, sqlx::FromRow)]
struct ClaimedRow {
    id: i64,
    identifier: String,
    payload: sqlx::types::Json<JsonObject>,
    attempts: i64,
}

#[derive(Debug)]
pub(crate) struct GraphileQueue {
    pool: PgPool,
    worker: String,
    flag_after: u32,
    claim_ceiling: Option<i16>,
}

impl GraphileQueue {
    pub(crate) fn new(pool: PgPool) -> Self {
        Self {
            pool,
            worker: uuid::Uuid::new_v4().to_string(),
            flag_after: kernel::flagged::FLAG_AFTER_FAILURES,
            claim_ceiling: None,
        }
    }

    pub(crate) fn flagging_after(mut self, failures: u32) -> Self {
        self.flag_after = failures;
        self
    }

    pub(crate) fn claiming_at_most(mut self, priority: Option<i16>) -> Self {
        self.claim_ceiling = priority;
        self
    }

    pub(crate) async fn subscribe(
        &self,
    ) -> Result<Box<dyn kernel::queue::JobNotifications>, Error> {
        listen(&self.pool).await
    }

    pub(crate) async fn schema_present<'e>(
        executor: impl sqlx::PgExecutor<'e>,
    ) -> Result<bool, Error> {
        sqlx::query_scalar(crate::graphile_sql::REACHABLE)
            .fetch_one(executor)
            .await
            .map_err(|source| Error::operation("queue probe", source))
    }
    pub(crate) async fn enqueue(&self, job: &Job) -> Result<JobId, Error> {
        if job.key_mode == JobKeyMode::Arm {
            return self.arm(job).await;
        }
        let payload = sqlx::types::Json(&job.payload);
        let queued: QueuedJob = sqlx::query_as(
            "select (graphile_worker.add_job($1, $2::json, $8, $3, $4, $5, $6, null::text[], $7)).id",
        )
        .bind(job.name.to_string())
        .bind(payload)
        .bind(job.run_at)
        .bind(match job.max_attempts {
            Some(attempts) => attempts,
            None => attempts_for(&job.name),
        })
        .bind(&job.key)
        .bind(i32::from(job.priority))
        .bind(key_mode(job.key_mode))
        .bind(single_flight(job))
        .fetch_one(&self.pool)
        .await
        .map_err(|source| Error::operation("enqueue", source))?;
        Ok(JobId::new(queued.id))
    }

    async fn arm(&self, job: &Job) -> Result<JobId, Error> {
        let queued: QueuedJob = sqlx::query_as(crate::graphile_sql::ARM)
            .bind(job.name.to_string())
            .bind(sqlx::types::Json(&job.payload))
            .bind(job.run_at)
            .bind(match job.max_attempts {
                Some(attempts) => attempts,
                None => attempts_for(&job.name),
            })
            .bind(&job.key)
            .bind(i32::from(job.priority))
            .bind(single_flight(job))
            .fetch_one(&self.pool)
            .await
            .map_err(|source| Error::operation("enqueue", source))?;
        Ok(JobId::new(queued.id))
    }

    pub(crate) async fn claim(
        &self,
        tasks: &[JobName],
        only_id: Option<JobId>,
    ) -> Result<Option<ClaimedJob>, Error> {
        let tasks: Vec<String> = tasks.iter().map(ToString::to_string).collect();
        let locked_by = format!("{}-{}", self.worker, uuid::Uuid::new_v4());
        let row: Option<ClaimedRow> = sqlx::query_as(crate::graphile_sql::CLAIM)
            .bind(&tasks)
            .bind(only_id.map(i64::from))
            .bind(&locked_by)
            .bind(self.claim_ceiling)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| Error::operation("claim", source))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let name = JobName::from_str(&row.identifier).map_err(|job_error| {
            Error::protocol(format!(
                "queued task {} is not a kernel job name: {job_error}",
                row.identifier
            ))
        })?;
        let attempt =
            u32::try_from(row.attempts).map_err(|source| Error::protocol(source.to_string()))?;
        Ok(Some(ClaimedJob {
            id: JobId::new(row.id),
            name,
            payload: row.payload.0,
            attempt,
        }))
    }

    pub(crate) async fn read(
        &self,
        id: JobId,
    ) -> Result<Option<kernel::queue::StoredJob>, kernel::queue::JobQueueError> {
        let mut conn = self
            .pool
            .acquire()
            .await
            .map_err(|source| kernel::queue::JobQueueError::Unavailable(Box::new(source)))?;
        crate::task_jobs::read_job(&mut conn, id).await
    }

    pub(crate) async fn complete(&self, id: JobId) -> Result<u64, Error> {
        let removed: RemovedJobs = sqlx::query_as(crate::graphile_sql::COMPLETE)
            .bind(i64::from(id))
            .bind(format!("{}-", self.worker))
            .fetch_one(&self.pool)
            .await
            .map_err(|source| Error::operation("complete", source))?;
        count(removed.removed)
    }

    pub(crate) async fn release(
        &self,
        id: JobId,
        delay: Option<std::time::Duration>,
    ) -> Result<u64, Error> {
        let released: i64 = sqlx::query_scalar(crate::graphile_sql::RELEASE)
            .bind(i64::from(id))
            .bind(format!("{}-", self.worker))
            .bind(delay.map(|delay| delay.as_secs_f64()))
            .fetch_one(&self.pool)
            .await
            .map_err(|source| Error::operation("release", source))?;
        count(released)
    }

    pub(crate) async fn fail(&self, id: JobId, error: &str) -> Result<(), Error> {
        crate::flagged::fail(
            &self.pool,
            i64::from(id),
            error,
            &format!("{}-", self.worker),
            self.flag_after,
        )
        .await
    }

    pub(crate) async fn flag_stuck(&self) -> Result<u64, Error> {
        crate::flagged::flag_stuck(&self.pool, self.flag_after).await
    }

    pub(crate) async fn dead_job_ids(
        &self,
        task: &JobName,
        limit: u32,
    ) -> Result<Vec<JobId>, Error> {
        let rows: Vec<DeadJob> = sqlx::query_as(
            "select j.id from graphile_worker._private_jobs j
             join graphile_worker._private_tasks t on t.id = j.task_id
             where t.identifier = $1 and j.attempts >= j.max_attempts
               and j.key is not null
               and j.updated_at < now() - interval '24 hours'
               and (j.locked_at is null or j.locked_at < now() - interval '4 hours')
             order by j.run_at asc limit $2",
        )
        .bind(task.to_string())
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(|source| Error::operation("dead jobs", source))?;
        Ok(rows.into_iter().map(|job| JobId::new(job.id)).collect())
    }

    pub(crate) async fn reschedule_dead(
        &self,
        task: &JobName,
        limit: u32,
        priority: i16,
    ) -> Result<u64, Error> {
        let ids = self.dead_job_ids(task, limit).await?;
        self.reschedule(&ids, priority).await
    }

    pub(crate) async fn reschedule(&self, ids: &[JobId], priority: i16) -> Result<u64, Error> {
        if ids.is_empty() {
            return Ok(0);
        }
        let raw: Vec<i64> = ids.iter().map(|id| i64::from(*id)).collect();
        sqlx::query(
            "update graphile_worker._private_jobs
                set locked_at = null, locked_by = null
              where id = any($1::bigint[])
                and locked_at < now() - interval '4 hours'",
        )
        .bind(&raw)
        .execute(&self.pool)
        .await
        .map_err(|source| Error::operation("reschedule unlock", source))?;
        let rescheduled: RescheduledJobs = sqlx::query_as(
            "select count(*)::bigint as rescheduled from graphile_worker.reschedule_jobs($1::bigint[], now(), $2, 0)",
        )
        .bind(&raw)
        .bind(i32::from(priority))
        .fetch_one(&self.pool)
        .await
        .map_err(|source| Error::operation("reschedule dead", source))?;
        count(rescheduled.rescheduled)
    }

    pub(crate) async fn reap_dead_corpses(
        &self,
        allowlist: &[JobName],
        limit: u32,
    ) -> Result<u64, Error> {
        let allowlist: Vec<String> = allowlist.iter().map(ToString::to_string).collect();
        let reaped: ReapedJobs = sqlx::query_as(
            "with deleted as (
                delete from graphile_worker._private_jobs
                 where ctid in (
                   select ctid from graphile_worker._private_jobs j
                    where j.key is null
                      and j.attempts >= j.max_attempts
                      and j.locked_at is null
                      and j.updated_at < now() - interval '7 days'
                      and j.task_id in (select id from graphile_worker._private_tasks where identifier = any($1::text[]))
                    order by j.updated_at asc
                    limit $2
                 )
                 returning 1
             ) select count(*)::bigint as count from deleted",
        )
        .bind(&allowlist)
        .bind(i64::from(limit.min(200)))
        .fetch_one(&self.pool)
        .await
        .map_err(|source| Error::operation("reap dead jobs", source))?;
        count(reaped.count)
    }

    pub(crate) async fn queue_health(
        &self,
        task: &JobName,
    ) -> Result<Option<crate::queue::QueueHealth>, Error> {
        let Some(row) = sqlx::query_as::<_, HealthRow>(
            "select
                count(*)::bigint as total,
                count(*) filter (where j.attempts < j.max_attempts
                                  and (j.locked_at is null or j.locked_at < now() - interval '4 hours')
                                  and j.run_at <= now())::bigint as pending,
                count(*) filter (where j.locked_at >= now() - interval '4 hours')::bigint as running,
                count(*) filter (where j.locked_at is null and j.run_at > now()
                                  and j.attempts < j.max_attempts)::bigint as scheduled,
                count(*) filter (where j.attempts >= j.max_attempts)::bigint as dead,
                count(*) filter (where j.attempts >= j.max_attempts
                                  and j.created_at > now() - interval '24 hours')::bigint as recent_dead,
                floor(extract(epoch from (now() - min(j.run_at) filter (where j.attempts < j.max_attempts
                                  and (j.locked_at is null or j.locked_at < now() - interval '4 hours')
                                  and j.run_at <= now()))) / 60)::bigint as oldest_pending_minutes,
                (select left(j2.last_error, 120) from graphile_worker._private_jobs j2
                  join graphile_worker._private_tasks t2 on t2.id = j2.task_id
                  where t2.identifier = $1 and j2.attempts >= j2.max_attempts
                  order by j2.updated_at desc limit 1) as last_error
             from graphile_worker._private_jobs j
             join graphile_worker._private_tasks t on t.id = j.task_id
             where t.identifier = $1",
        )
        .bind(task.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|source| Error::operation("queue health", source))?
        else {
            return Ok(None);
        };
        if row.total == 0 {
            return Ok(None);
        }
        let oldest_pending_minutes = row.oldest_pending_minutes.map(u64::try_from).transpose()?;
        Ok(Some(crate::queue::QueueHealth {
            task: task.clone(),
            pending: count(row.pending)?,
            running: count(row.running)?,
            scheduled: count(row.scheduled)?,
            dead: count(row.dead)?,
            recent_dead: count(row.recent_dead)?,
            oldest_pending_minutes,
            last_error: row.last_error,
        }))
    }
}

pub(crate) async fn listen(
    pool: &PgPool,
) -> Result<Box<dyn kernel::queue::JobNotifications>, Error> {
    let listener = crate::listen::Listener::connect_resilient(
        pool.clone(),
        &["jobs:insert"],
        "job notifications",
        tokio_util::sync::CancellationToken::new(),
    )
    .await
    .map_err(|source| Error::operation("queue listen", source))?;
    Ok(Box::new(Notifications(listener)))
}

fn count(value: i64) -> Result<u64, Error> {
    u64::try_from(value).map_err(|source| Error::protocol(source.to_string()))
}

fn key_mode(mode: JobKeyMode) -> &'static str {
    match mode {
        JobKeyMode::KeepRunAt => "preserve_run_at",
        JobKeyMode::Replace | JobKeyMode::Arm => "replace",
    }
}

fn single_flight(job: &Job) -> Option<&str> {
    if job.name == JobName::Fixed(FixedJob::SummarizeConversation) {
        job.key.as_deref()
    } else {
        None
    }
}

pub(crate) fn attempts_for(name: &JobName) -> i32 {
    match name {
        JobName::Fixed(fixed) => match fixed {
            FixedJob::EmbedTarget | FixedJob::SyncTopicPage => 5,
            FixedJob::SummarizeConversation | FixedJob::SynthesizeToolCall => 3,
            FixedJob::FileConversation | FixedJob::ExtractWiki => 2,
            FixedJob::RunTask => 1,
            FixedJob::EnqueueUnembedded
            | FixedJob::EnqueueUnchunked
            | FixedJob::EnqueueIdle
            | FixedJob::EnqueueWikiBackfill
            | FixedJob::ConsolidateWiki
            | FixedJob::RecompileWiki
            | FixedJob::EnqueueStaleWiki
            | FixedJob::EnqueueStaleSummaries
            | FixedJob::ReapDeadJobs
            | FixedJob::ProposeTidy
            | FixedJob::PurgeDeletedSessions => 25,
        },
        JobName::RunTaskOn { .. } | JobName::Heartbeat { .. } => 1,
    }
}
