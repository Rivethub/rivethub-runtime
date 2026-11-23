use jiff::SignedDuration;
use jiff::Timestamp;
use kernel::queue::ClaimSelector;
use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueue as _;
use kernel::queue::admin::QueueAdmin as _;
use memory_sqlite::SqliteStore;
use uuid::Uuid;
use wire::time::TimestampFormat;

use crate::common::TestResult;
use crate::common::fresh_store;
use crate::common::seed;

#[derive(Debug)]
struct ClaimRow {
    state: String,
    attempts: i64,
    locked_at: Option<String>,
}

struct JobRow {
    attempts: i64,
    max_attempts: i64,
    last_error: Option<String>,
    priority: Option<i64>,
}

#[tokio::test]
async fn a_claimed_final_attempt_without_an_error_counts_as_running() -> TestResult {
    let fresh = fresh_store()?;
    let name = JobName::Fixed(FixedJob::ExtractWiki);
    fresh
        .store
        .enqueue(Job {
            name: name.clone(),
            payload: wire::JsonObject::default(),
            key: None,
            key_mode: JobKeyMode::Replace,
            priority: 0,
            run_at: None,
            max_attempts: Some(1),
        })
        .await?;
    let claimed = fresh
        .store
        .claim(&ClaimSelector::matching(vec![name.clone()]))
        .await?;
    assert!(claimed.is_some());
    let status = fresh.store.admin_status().await?;
    let row = status
        .into_iter()
        .find(|row| row.task == name)
        .ok_or("extract-wiki status")?;
    assert_eq!(row.running, 1);
    assert_eq!(row.dead, 0);
    assert_eq!(row.pending, 0);
    Ok(())
}

#[tokio::test]
async fn a_dead_job_below_its_attempt_limit_is_not_pending() -> TestResult {
    let fresh = fresh_store()?;
    let name = JobName::Fixed(FixedJob::ExtractWiki);
    fresh
        .store
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO ros_jobs (
                    id, task, job_key, payload, run_at, attempts, max_attempts, state,
                    last_error, created_at, updated_at
                 ) VALUES (
                    'below-max', 'extract-wiki', 'below-max', '{}',
                    '2026-01-01T00:00:00Z', 5, 8, 'dead', 'fifth failure',
                    '2026-01-01T00:00:00Z', '2026-01-01T00:00:00Z'
                 )",
                [],
            )
            .map_err(seed)?;
            Ok(())
        })
        .await?;
    let row = fresh
        .store
        .admin_status()
        .await?
        .into_iter()
        .find(|row| row.task == name)
        .ok_or("extract-wiki status")?;
    assert_eq!(row.pending, 0);
    assert_eq!(row.dead, 1);
    assert_eq!(row.running, 0);
    assert_eq!(row.last_error.as_deref(), Some("fifth failure"));
    let tasks = vec!["extract-wiki".to_owned()];
    assert_eq!(fresh.store.admin_retry(&tasks, None, None, false).await?, 0);
    assert_eq!(fresh.store.admin_requeue(&name, 10, false).await?, 0);
    let stored = job(&fresh.store, "below-max").await?;
    assert_eq!(stored.attempts, 5);
    assert_eq!(stored.last_error.as_deref(), Some("fifth failure"));
    Ok(())
}

#[tokio::test]
async fn retry_and_requeue_leave_a_claimed_job_running() -> TestResult {
    let fresh = fresh_store()?;
    let name = JobName::Fixed(FixedJob::ExtractWiki);
    fresh
        .store
        .enqueue(Job {
            name: name.clone(),
            payload: wire::JsonObject::default(),
            key: Some("claimed".to_owned()),
            key_mode: JobKeyMode::Replace,
            priority: 0,
            run_at: None,
            max_attempts: Some(1),
        })
        .await?;
    assert!(
        fresh
            .store
            .claim(&ClaimSelector::matching(vec![name.clone()]))
            .await?
            .is_some()
    );
    running_with_a_stale_lock(&fresh.store).await?;
    let tasks = vec!["extract-wiki".to_owned()];
    let revived = fresh.store.admin_requeue(&name, 10, false).await?;
    let reset = fresh.store.admin_retry(&tasks, None, None, false).await?;
    assert_eq!((revived, reset), (0, 0));
    let stored = claim_row(&fresh.store, "claimed").await?;
    assert_eq!(stored.state, "running");
    assert_eq!(stored.attempts, 1);
    assert!(stored.locked_at.is_some());
    let stale = claim_row(&fresh.store, "stale-running").await?;
    assert_eq!(stale.state, "running");
    assert_eq!(stale.attempts, 2);
    assert!(stale.locked_at.is_some());
    Ok(())
}

async fn running_with_a_stale_lock(store: &SqliteStore) -> TestResult {
    let now = Timestamp::now();
    let updated = hours(now, 25)?.rfc3339_millis();
    let locked = hours(now, 5)?.rfc3339_millis();
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_jobs (
                    id, task, job_key, payload, run_at, attempts, max_attempts, state,
                    last_error, locked_at, created_at, updated_at
                 ) VALUES (
                    'stale-running', 'extract-wiki', 'stale-running', '{}', ?1,
                    2, 2, 'running', 'held', ?2, ?1, ?1
                 )",
                (updated, locked),
            )
            .map_err(seed)?;
            Ok(())
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn maintenance_queue_admin_matches_the_shared_rules() -> TestResult {
    let fresh = fresh_store()?;
    testkit::maintenance::maintenance_tasks_start_empty(&fresh.store).await?;
    seed_retry(&fresh.store).await?;
    testkit::maintenance::retry_resets_only_wiki_errors(&fresh.store).await?;
    assert_eq!(job(&fresh.store, "wiki-open").await?.attempts, 0);
    assert_eq!(job(&fresh.store, "wiki-locked").await?.attempts, 0);
    assert_eq!(job(&fresh.store, "wiki-open").await?.last_error, None);
    assert_eq!(job(&fresh.store, "wiki-locked").await?.last_error, None);
    stayed_dead(&fresh.store, "other", "other boom").await?;
    stayed_dead(&fresh.store, "other-task", "wiki failed").await?;
    Ok(())
}

#[tokio::test]
async fn requeue_revives_only_the_idle_dead_jobs() -> TestResult {
    let fresh = fresh_store()?;
    seed_requeue(&fresh.store).await?;
    testkit::maintenance::requeue_revives_two_idle_jobs(&fresh.store).await?;
    revived(&fresh.store, "old-open").await?;
    revived(&fresh.store, "stale-lock").await?;
    stayed_dead(&fresh.store, "fresh-lock", "stale").await?;
    stayed_dead(&fresh.store, "recent", "stale").await?;
    Ok(())
}

async fn seed_retry(store: &SqliteStore) -> TestResult {
    let now = Timestamp::now();
    dead_job(
        store,
        "extract-wiki",
        "wiki-open",
        "wiki failed",
        hours(now, 30)?,
        None,
    )
    .await?;
    dead_job(
        store,
        "extract-wiki",
        "wiki-locked",
        "wiki failed",
        hours(now, 28)?,
        Some(hours(now, 1)?),
    )
    .await?;
    dead_job(
        store,
        "extract-wiki",
        "other",
        "other boom",
        hours(now, 26)?,
        None,
    )
    .await?;
    dead_job(
        store,
        "compact-conversation",
        "other-task",
        "wiki failed",
        hours(now, 24)?,
        None,
    )
    .await?;
    Ok(())
}

async fn seed_requeue(store: &SqliteStore) -> TestResult {
    let now = Timestamp::now();
    dead_job(
        store,
        "extract-wiki",
        "old-open",
        "stale",
        hours(now, 25)?,
        None,
    )
    .await?;
    dead_job(
        store,
        "extract-wiki",
        "stale-lock",
        "stale",
        hours(now, 25)?,
        Some(hours(now, 5)?),
    )
    .await?;
    dead_job(
        store,
        "extract-wiki",
        "fresh-lock",
        "stale",
        hours(now, 25)?,
        Some(now),
    )
    .await?;
    dead_job(store, "extract-wiki", "recent", "stale", now, None).await?;
    Ok(())
}

fn hours(now: Timestamp, back: i64) -> Result<Timestamp, jiff::Error> {
    now.checked_sub(SignedDuration::from_hours(back))
}

async fn dead_job(
    store: &SqliteStore,
    task: &str,
    key: &str,
    error: &str,
    updated: Timestamp,
    locked: Option<Timestamp>,
) -> TestResult {
    let id = Uuid::new_v4().to_string();
    let task = task.to_owned();
    let key = key.to_owned();
    let error = error.to_owned();
    let updated = updated.rfc3339_millis();
    let locked = locked.map(TimestampFormat::rfc3339_millis);
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_jobs (
                    id, task, job_key, payload, run_at, attempts, max_attempts, state,
                    last_error, locked_at, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, '{}', ?4, 2, 2, 'dead', ?5, ?6, ?4, ?4)",
                (id, task, key, updated, error, locked),
            )
            .map_err(seed)?;
            Ok(())
        })
        .await?;
    Ok(())
}

async fn claim_row(store: &SqliteStore, key: &str) -> TestResult<ClaimRow> {
    let key = key.to_owned();
    let row = store
        .with_conn(move |conn| {
            conn.query_row(
                "SELECT state, attempts, locked_at FROM ros_jobs WHERE job_key = ?1",
                [key],
                |row| {
                    Ok(ClaimRow {
                        state: row.get(0)?,
                        attempts: row.get(1)?,
                        locked_at: row.get(2)?,
                    })
                },
            )
            .map_err(seed)
        })
        .await?;
    Ok(row)
}

async fn job(store: &SqliteStore, key: &str) -> TestResult<JobRow> {
    let key = key.to_owned();
    let row = store
        .with_conn(move |conn| {
            conn.query_row(
                "SELECT attempts, max_attempts, last_error,
                        (SELECT priority FROM rh_job_priorities
                          WHERE job_id = ros_jobs.id)
                   FROM ros_jobs WHERE job_key = ?1",
                [key],
                |row| {
                    Ok(JobRow {
                        attempts: row.get(0)?,
                        max_attempts: row.get(1)?,
                        last_error: row.get(2)?,
                        priority: row.get(3)?,
                    })
                },
            )
            .map_err(seed)
        })
        .await?;
    Ok(row)
}

async fn stayed_dead(store: &SqliteStore, key: &str, error: &str) -> TestResult {
    let row = job(store, key).await?;
    assert_eq!(row.attempts, row.max_attempts);
    assert_eq!(row.last_error.as_deref(), Some(error));
    Ok(())
}

async fn revived(store: &SqliteStore, key: &str) -> TestResult {
    let row = job(store, key).await?;
    assert_eq!(row.attempts, 0);
    assert_eq!(row.priority, Some(10));
    assert_eq!(row.last_error.as_deref(), Some("stale"));
    Ok(())
}
