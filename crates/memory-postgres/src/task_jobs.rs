use std::str::FromStr;

use sqlx::PgConnection;
use sqlx::Row;
use uuid::Uuid;

use kernel::queue::ClaimSelector;
use kernel::queue::ClaimedJob;
use kernel::queue::Job;
use kernel::queue::JobId;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueueError;
use kernel::queue::StoredJob;
use kernel::tasks::TaskError;

pub(crate) fn map_sql(operation: &'static str, source: sqlx::Error) -> TaskError {
    if queue_absent(&source) {
        return TaskError::QueueAbsent;
    }
    if crate::loss::unreachable(&source) {
        return TaskError::Unavailable(Box::new(source));
    }
    TaskError::Operation {
        operation,
        source: Box::new(source),
    }
}

fn queue_absent(source: &sqlx::Error) -> bool {
    source.as_database_error().is_some_and(|database| {
        database
            .code()
            .as_deref()
            .is_some_and(|code| code == "42883" || code == "42P01" || code == "3F000")
    })
}

pub(crate) async fn enqueue_job(
    conn: &mut PgConnection,
    job: &Job,
) -> Result<JobId, JobQueueError> {
    if !crate::graphile::GraphileQueue::schema_present(&mut *conn).await? {
        return Err(JobQueueError::Absent);
    }
    let payload = sqlx::types::Json(&job.payload);
    let attempts = match job.max_attempts {
        Some(attempts) => attempts,
        None => crate::graphile::attempts_for(&job.name),
    };
    let row = sqlx::query(
        "SELECT id FROM graphile_worker.add_job($1, $2::json, null::text, null::timestamptz, $3, $4, $5, null::text[], $6)",
    )
    .bind(job.name.to_string())
    .bind(payload)
    .bind(attempts)
    .bind(&job.key)
    .bind(i32::from(job.priority))
    .bind(key_mode_text(job.key_mode))
    .fetch_one(&mut *conn)
    .await
    .map_err(|source| map_queue("enqueue", source))?;
    let id: i64 = queue_cell(&row, "enqueue", "id")?;
    Ok(JobId::new(id))
}

pub(crate) async fn claim_job(
    conn: &mut PgConnection,
    selector: &ClaimSelector,
    worker: &str,
) -> Result<Option<ClaimedJob>, JobQueueError> {
    let names: Vec<String> = selector.names.iter().map(ToString::to_string).collect();
    let locked_by = format!("{worker}-{}", Uuid::new_v4());
    let row = sqlx::query(crate::graphile_sql::CLAIM)
        .bind(&names)
        .bind(selector.only_id.map(i64::from))
        .bind(locked_by)
        .bind(None::<i16>)
        .fetch_optional(&mut *conn)
        .await
        .map_err(|source| map_queue("claim", source))?;
    row.as_ref().map(claimed).transpose()
}

pub(crate) async fn complete_job(
    conn: &mut PgConnection,
    id: JobId,
    worker: &str,
) -> Result<(), JobQueueError> {
    let row = sqlx::query(crate::graphile_sql::COMPLETE)
        .bind(i64::from(id))
        .bind(format!("{worker}-"))
        .fetch_one(&mut *conn)
        .await
        .map_err(|source| map_queue("complete", source))?;
    let removed: i64 = queue_cell(&row, "complete", "removed")?;
    if removed == 0 {
        return Err(JobQueueError::Missing { id });
    }
    Ok(())
}

pub(crate) async fn release_job(
    conn: &mut PgConnection,
    id: JobId,
    worker: &str,
) -> Result<(), JobQueueError> {
    let released: i64 = sqlx::query_scalar(crate::graphile_sql::RELEASE)
        .bind(i64::from(id))
        .bind(format!("{worker}-"))
        .fetch_one(&mut *conn)
        .await
        .map_err(|source| map_queue("release", source))?;
    if released == 0 {
        return Err(JobQueueError::Missing { id });
    }
    Ok(())
}

pub(crate) async fn fail_job(
    conn: &mut PgConnection,
    id: JobId,
    error: &str,
    worker: &str,
) -> Result<(), JobQueueError> {
    let failed: i64 = sqlx::query_scalar(crate::graphile_sql::FAIL)
        .bind(i64::from(id))
        .bind(error)
        .bind(format!("{worker}-"))
        .fetch_one(&mut *conn)
        .await
        .map_err(|source| map_queue("fail", source))?;
    if failed == 0 {
        return Err(JobQueueError::Missing { id });
    }
    Ok(())
}

pub(crate) async fn read_job(
    conn: &mut PgConnection,
    id: JobId,
) -> Result<Option<StoredJob>, JobQueueError> {
    let row = sqlx::query(
        "SELECT jobs.id, known.identifier, jobs.payload::text AS payload, jobs.key,
                jobs.priority::bigint AS priority, jobs.max_attempts::bigint AS max_attempts,
                jobs.attempts::bigint AS attempts, jobs.last_error
           FROM graphile_worker._private_jobs AS jobs
           JOIN graphile_worker._private_tasks AS known ON known.id = jobs.task_id
          WHERE jobs.id = $1",
    )
    .bind(i64::from(id))
    .fetch_optional(&mut *conn)
    .await
    .map_err(|source| map_queue("read job", source))?;
    row.as_ref().map(stored).transpose()
}

fn claimed(row: &sqlx::postgres::PgRow) -> Result<ClaimedJob, JobQueueError> {
    let attempts: i64 = queue_cell(row, "claim", "attempts")?;
    let id: i64 = queue_cell(row, "claim", "id")?;
    let identifier: String = queue_cell(row, "claim", "identifier")?;
    Ok(ClaimedJob {
        id: JobId::new(id),
        name: job_name(&identifier)?,
        payload: queue_cell::<sqlx::types::Json<wire::JsonObject>>(row, "claim", "payload")?.0,
        attempt: narrow(attempts, "attempt")?,
    })
}

fn stored(row: &sqlx::postgres::PgRow) -> Result<StoredJob, JobQueueError> {
    let payload: String = queue_cell(row, "read job", "payload")?;
    let priority: i64 = queue_cell(row, "read job", "priority")?;
    let max_attempts: i64 = queue_cell(row, "read job", "max_attempts")?;
    let attempts: i64 = queue_cell(row, "read job", "attempts")?;
    let id: i64 = queue_cell(row, "read job", "id")?;
    let identifier: String = queue_cell(row, "read job", "identifier")?;
    Ok(StoredJob {
        id: JobId::new(id),
        name: job_name(&identifier)?,
        payload: serde_json::from_str(&payload)
            .map_err(|source| JobQueueError::Payload(Box::new(source)))?,
        key: queue_cell(row, "read job", "key")?,
        priority: Some(narrow(priority, "priority")?),
        max_attempts: narrow(max_attempts, "max_attempts")?,
        attempts: narrow(attempts, "attempts")?,
        last_error: queue_cell(row, "read job", "last_error")?,
    })
}

fn job_name(identifier: &str) -> Result<JobName, JobQueueError> {
    JobName::from_str(identifier).map_err(|error| JobQueueError::Stored(error.to_string()))
}

fn narrow<T>(value: i64, field: &'static str) -> Result<T, JobQueueError>
where
    T: TryFrom<i64>,
{
    T::try_from(value).map_err(|_| JobQueueError::Stored(field.to_string()))
}

fn map_queue(operation: &'static str, source: sqlx::Error) -> JobQueueError {
    if queue_absent(&source) {
        return JobQueueError::Absent;
    }
    if crate::loss::unreachable(&source) {
        return JobQueueError::Unavailable(Box::new(source));
    }
    JobQueueError::Operation {
        operation,
        source: Box::new(source),
    }
}

fn queue_cell<'a, T>(
    row: &'a sqlx::postgres::PgRow,
    operation: &'static str,
    name: &'static str,
) -> Result<T, JobQueueError>
where
    T: sqlx::Decode<'a, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
{
    row.try_get(name)
        .map_err(|source| map_queue(operation, source))
}

fn key_mode_text(mode: JobKeyMode) -> &'static str {
    match mode {
        JobKeyMode::KeepRunAt | JobKeyMode::Arm => "preserve_run_at",
        JobKeyMode::Replace => "replace",
    }
}
