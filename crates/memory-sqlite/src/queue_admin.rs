use async_trait::async_trait;
use jiff::SignedDuration;
use jiff::Timestamp;
use kernel::queue::JobName;
use kernel::queue::JobQueueError;
use kernel::queue::admin::MAINTENANCE_TASKS;
use kernel::queue::admin::QueueAdmin;
use kernel::queue::admin::QueueHealth;
use kernel::queue::admin::empty_health;
use rusqlite::Connection;
use rusqlite::OptionalExtension;
use rusqlite::params;
use rusqlite::params_from_iter;
use rusqlite::types::Value;

use crate::SqliteStore;
use crate::error::SqliteError;
use crate::job_rank;
use crate::time;

const STALE_LOCK: i64 = 4;
const IDLE_DEAD: i64 = 24;
const REQUEUE_CAP: u32 = 1000;
const REQUEUE_PRIORITY: i16 = 10;
const ERROR_CHARS: i64 = 120;

struct Window {
    now: String,
    stale: String,
    idle: String,
}

#[async_trait]
impl QueueAdmin for SqliteStore {
    async fn admin_status(&self) -> Result<Vec<QueueHealth>, JobQueueError> {
        let window = window(self.clock.now()).map_err(admin_error)?;
        self.with_conn(move |conn| status(conn, &window))
            .await
            .map_err(admin_error)
    }

    async fn admin_retry(
        &self,
        tasks: &[String],
        error: Option<&str>,
        limit: Option<u32>,
        dry_run: bool,
    ) -> Result<u64, JobQueueError> {
        let tasks = tasks.to_vec();
        let error = error.map(str::to_owned);
        let now = time::stamp(self.clock.now());
        let reset = self
            .with_conn(move |conn| retry(conn, &tasks, error.as_deref(), limit, dry_run, &now))
            .await
            .map_err(admin_error)?;
        if reset > 0 && !dry_run {
            self.notifications.send_replace(());
        }
        Ok(reset)
    }

    async fn admin_requeue(
        &self,
        task: &JobName,
        limit: u32,
        dry_run: bool,
    ) -> Result<u64, JobQueueError> {
        if limit == 0 {
            return Err(JobQueueError::Stored(
                "requeue limit must be positive".to_owned(),
            ));
        }
        let task = task.to_string();
        let limit = limit.min(REQUEUE_CAP);
        let window = window(self.clock.now()).map_err(admin_error)?;
        let revived = self
            .with_conn(move |conn| requeue(conn, &task, limit, dry_run, &window))
            .await
            .map_err(admin_error)?;
        if revived > 0 && !dry_run {
            self.notifications.send_replace(());
        }
        Ok(revived)
    }
}

fn window(now: Timestamp) -> Result<Window, SqliteError> {
    Ok(Window {
        now: time::stamp(now),
        stale: time::stamp(earlier(now, STALE_LOCK)?),
        idle: time::stamp(earlier(now, IDLE_DEAD)?),
    })
}

fn earlier(now: Timestamp, hours: i64) -> Result<Timestamp, SqliteError> {
    now.checked_sub(SignedDuration::from_hours(hours))
        .map_err(|_| SqliteError::Clock)
}

fn status(conn: &Connection, window: &Window) -> Result<Vec<QueueHealth>, SqliteError> {
    let mut rows = Vec::with_capacity(MAINTENANCE_TASKS.len());
    for task in MAINTENANCE_TASKS {
        let name = JobName::Fixed(task);
        rows.push(health(conn, &name, window)?);
    }
    Ok(rows)
}

fn health(conn: &Connection, task: &JobName, window: &Window) -> Result<QueueHealth, SqliteError> {
    let counts = counts(conn, &task.to_string(), window)?;
    if counts.total == 0 {
        return Ok(empty_health(task.clone()));
    }
    Ok(QueueHealth {
        task: task.clone(),
        pending: count(counts.pending)?,
        running: count(counts.running)?,
        scheduled: count(counts.scheduled)?,
        dead: count(counts.dead)?,
        recent_dead: count(counts.recent_dead)?,
        oldest_pending_minutes: minutes(window, counts.oldest)?,
        last_error: latest_error(conn, &task.to_string())?,
    })
}

struct Counts {
    total: i64,
    pending: i64,
    running: i64,
    scheduled: i64,
    dead: i64,
    recent_dead: i64,
    oldest: Option<String>,
}

fn counts(conn: &Connection, task: &str, window: &Window) -> Result<Counts, SqliteError> {
    conn.query_row(
        "SELECT count(*),
                count(*) FILTER (WHERE state = 'queued'
                    AND attempts < max_attempts AND attempts < ?5
                    AND (locked_at IS NULL OR rh_instant(locked_at) < rh_instant(?2))
                    AND rh_instant(run_at) <= rh_instant(?3)),
                count(*) FILTER (WHERE state = 'running'
                    OR (state <> 'dead' AND locked_at IS NOT NULL
                        AND rh_instant(locked_at) >= rh_instant(?2))),
                count(*) FILTER (WHERE state = 'queued'
                    AND attempts < max_attempts AND attempts < ?5
                    AND locked_at IS NULL
                    AND rh_instant(run_at) > rh_instant(?3)),
                count(*) FILTER (WHERE state = 'dead'),
                count(*) FILTER (WHERE state = 'dead'
                    AND rh_instant(created_at) > rh_instant(?4)),
                min(run_at) FILTER (WHERE state = 'queued'
                    AND attempts < max_attempts AND attempts < ?5
                    AND (locked_at IS NULL OR rh_instant(locked_at) < rh_instant(?2))
                    AND rh_instant(run_at) <= rh_instant(?3))
           FROM ros_jobs WHERE task = ?1",
        params![
            task,
            window.stale,
            window.now,
            window.idle,
            kernel::flagged::FLAG_AFTER_FAILURES
        ],
        |row| {
            Ok(Counts {
                total: row.get(0)?,
                pending: row.get(1)?,
                running: row.get(2)?,
                scheduled: row.get(3)?,
                dead: row.get(4)?,
                recent_dead: row.get(5)?,
                oldest: row.get(6)?,
            })
        },
    )
    .map_err(SqliteError::query("queue health"))
}

fn latest_error(conn: &Connection, task: &str) -> Result<Option<String>, SqliteError> {
    conn.query_row(
        "SELECT substr(last_error, 1, ?2) FROM ros_jobs
          WHERE task = ?1 AND state = 'dead'
          ORDER BY rh_instant(updated_at) DESC LIMIT 1",
        params![task, ERROR_CHARS],
        |row| row.get::<_, Option<String>>(0),
    )
    .optional()
    .map(Option::flatten)
    .map_err(SqliteError::query("queue health"))
}

fn minutes(window: &Window, oldest: Option<String>) -> Result<Option<u64>, SqliteError> {
    let Some(oldest) = oldest else {
        return Ok(None);
    };
    let now = time::stored(&window.now, "run_at")?;
    let at = time::stored(&oldest, "run_at")?;
    let seconds = now.duration_since(at).as_secs();
    let minutes = u64::try_from(seconds / 60).map_err(SqliteError::Count)?;
    Ok(Some(minutes))
}

fn count(value: i64) -> Result<u64, SqliteError> {
    u64::try_from(value).map_err(SqliteError::Count)
}

fn retry(
    conn: &mut Connection,
    tasks: &[String],
    error: Option<&str>,
    limit: Option<u32>,
    dry_run: bool,
    now: &str,
) -> Result<u64, SqliteError> {
    if dry_run {
        return count_len(retry_ids(conn, tasks, error, limit)?.len());
    }
    let tx = crate::summary_replace::immediate(conn).map_err(SqliteError::query("retry failed"))?;
    let ids = retry_ids(&tx, tasks, error, limit)?;
    let mut reset = 0u64;
    for id in ids {
        let rows = reset_job(&tx, &id, now, error)?;
        reset += u64::try_from(rows).map_err(SqliteError::Count)?;
    }
    tx.commit().map_err(SqliteError::query("retry failed"))?;
    Ok(reset)
}

fn retry_ids(
    conn: &Connection,
    tasks: &[String],
    error: Option<&str>,
    limit: Option<u32>,
) -> Result<Vec<String>, SqliteError> {
    if tasks.is_empty() {
        return Ok(Vec::new());
    }
    let slots = (1..=tasks.len())
        .map(|index| format!("?{index}"))
        .collect::<Vec<_>>()
        .join(", ");
    let error_at = tasks.len() + 1;
    let (limit_sql, binds) = limit_clause(tasks, error, limit, error_at);
    let sql = format!(
        "SELECT id FROM ros_jobs WHERE task IN ({slots}) AND state = 'dead'
          AND attempts >= max_attempts
          AND (?{error_at} IS NULL OR instr(coalesce(last_error, ''), ?{error_at}) > 0)
          ORDER BY rh_instant(updated_at) ASC{limit_sql}"
    );
    ids(conn, &sql, binds)
}

fn limit_clause(
    tasks: &[String],
    error: Option<&str>,
    limit: Option<u32>,
    error_at: usize,
) -> (String, Vec<Value>) {
    let mut binds = tasks.iter().cloned().map(Value::Text).collect::<Vec<_>>();
    binds.push(error.map_or(Value::Null, |text| Value::Text(text.to_owned())));
    match limit {
        Some(limit) => {
            binds.push(Value::Integer(i64::from(limit)));
            (format!(" LIMIT ?{}", error_at + 1), binds)
        }
        None => (String::new(), binds),
    }
}

fn reset_job(
    conn: &Connection,
    id: &str,
    now: &str,
    error: Option<&str>,
) -> Result<usize, SqliteError> {
    conn.execute(
        "UPDATE ros_jobs SET attempts = 0, last_error = NULL, run_at = ?2, updated_at = ?2,
                state = 'queued', locked_at = NULL
          WHERE id = ?1 AND state = 'dead' AND attempts >= max_attempts
            AND (?3 IS NULL OR instr(coalesce(last_error, ''), ?3) > 0)",
        params![id, now, error],
    )
    .map_err(SqliteError::query("retry failed"))
}

fn requeue(
    conn: &mut Connection,
    task: &str,
    limit: u32,
    dry_run: bool,
    window: &Window,
) -> Result<u64, SqliteError> {
    if dry_run {
        return count_len(dead_ids(conn, task, limit, window)?.len());
    }
    let tx =
        crate::summary_replace::immediate(conn).map_err(SqliteError::query("requeue failed"))?;
    let ids = dead_ids(&tx, task, limit, window)?;
    let mut revived = 0u64;
    for id in ids {
        let rows = revive(&tx, &id, window)?;
        if rows > 0 {
            job_rank::rank(&tx, &id, REQUEUE_PRIORITY)?;
            revived += u64::try_from(rows).map_err(SqliteError::Count)?;
        }
    }
    tx.commit().map_err(SqliteError::query("requeue failed"))?;
    Ok(revived)
}

fn dead_ids(
    conn: &Connection,
    task: &str,
    limit: u32,
    window: &Window,
) -> Result<Vec<String>, SqliteError> {
    ids(
        conn,
        "SELECT id FROM ros_jobs
          WHERE task = ?1 AND state = 'dead' AND attempts >= max_attempts
            AND job_key IS NOT NULL
            AND rh_instant(updated_at) < rh_instant(?2)
            AND (locked_at IS NULL OR rh_instant(locked_at) < rh_instant(?3))
          ORDER BY rh_instant(run_at) ASC LIMIT ?4",
        vec![
            Value::Text(task.to_owned()),
            Value::Text(window.idle.clone()),
            Value::Text(window.stale.clone()),
            Value::Integer(i64::from(limit)),
        ],
    )
}

fn revive(conn: &Connection, id: &str, window: &Window) -> Result<usize, SqliteError> {
    conn.execute(
        "UPDATE ros_jobs SET state = 'queued', attempts = 0, locked_at = NULL,
                run_at = ?2, updated_at = ?2
          WHERE id = ?1 AND state = 'dead' AND attempts >= max_attempts
            AND job_key IS NOT NULL
            AND rh_instant(updated_at) < rh_instant(?3)
            AND (locked_at IS NULL OR rh_instant(locked_at) < rh_instant(?4))",
        params![id, window.now, window.idle, window.stale],
    )
    .map_err(SqliteError::query("requeue failed"))
}

fn ids(conn: &Connection, sql: &str, binds: Vec<Value>) -> Result<Vec<String>, SqliteError> {
    let mut statement = conn
        .prepare(sql)
        .map_err(SqliteError::query("queue admin"))?;
    let rows = statement
        .query_map(params_from_iter(binds), |row| row.get(0))
        .map_err(SqliteError::query("queue admin"))?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(SqliteError::query("queue admin"))
}

fn count_len(len: usize) -> Result<u64, SqliteError> {
    u64::try_from(len).map_err(SqliteError::Count)
}

fn admin_error(error: SqliteError) -> JobQueueError {
    if busy(&error) {
        return JobQueueError::Busy(Box::new(error));
    }
    JobQueueError::Operation {
        operation: "queue admin",
        source: Box::new(error),
    }
}

pub(crate) fn busy(error: &SqliteError) -> bool {
    let SqliteError::Query { source, .. } = error else {
        return false;
    };
    source.sqlite_error_code() == Some(rusqlite::ErrorCode::DatabaseBusy)
}
