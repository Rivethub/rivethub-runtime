use jiff::Timestamp;
use kernel::flagged::FlaggedJobs as _;
use kernel::queue::ClaimSelector;
use kernel::queue::FixedJob;
use kernel::queue::JobName;
use kernel::queue::JobQueue as _;
use memory_sqlite::SqliteStore;
use rusqlite::params;

use crate::common::TestResult;
use crate::common::execute;
use crate::common::fresh_store;

#[tokio::test]
async fn a_retried_flag_does_not_hand_its_id_to_the_next_key() -> TestResult {
    let fresh = fresh_store()?;
    let store = &fresh.store;
    store
        .enqueue(testkit::memory_jobs::ledger_job("alpha", 1)?)
        .await?;
    let first = claim(store).await?.ok_or("alpha")?;
    store.fail(first.id, "alpha").await?;
    let alpha = only(store).await?;
    store.retry(alpha.id).await?;
    let retried = claim(store).await?.ok_or("alpha returned")?;
    store.complete(retried.id).await?;
    store
        .enqueue(testkit::memory_jobs::ledger_job("beta", 1)?)
        .await?;
    let second = claim(store).await?.ok_or("beta")?;
    store.fail(second.id, "beta").await?;
    let beta = only(store).await?;
    assert_ne!(beta.id, alpha.id);
    Ok(())
}

#[tokio::test]
async fn exhausted_dead_queued_and_stale_running_jobs_leave_the_queue() -> TestResult {
    let fresh = fresh_store()?;
    let store = &fresh.store;
    execute(
        store,
        "INSERT INTO ros_jobs
            (id, task, job_key, payload, run_at, attempts, max_attempts, state, last_error,
             locked_at, created_at, updated_at)
         VALUES
            ('dead', 'embed-target', 'dead', '{}', '2020-01-01T00:00:00.000Z', 1, 1, 'dead',
             'boom', NULL, '2020-01-01T00:00:00.000Z', '2020-01-02T00:00:00.000Z'),
            ('queued', 'embed-target', 'queued', '{}', '2020-01-01T00:00:00.000Z', 1, 1, 'queued',
             'boom', NULL, '2020-01-01T00:00:00.000Z', '2020-01-02T00:00:00.000Z'),
            ('stale', 'embed-target', 'stale', '{}', '2020-01-01T00:00:00.000Z', 1, 1, 'running',
             'boom', '2020-01-01T00:00:00.000Z', '2020-01-01T00:00:00.000Z',
             '2020-01-02T00:00:00.000Z'),
            ('fresh', 'embed-target', 'fresh', '{}', '2020-01-01T00:00:00.000Z', 1, 1, 'running',
             'boom', '2099-01-01T00:00:00.000Z', '2020-01-01T00:00:00.000Z',
             '2020-01-02T00:00:00.000Z'),
            ('wide', 'embed-target', 'wide', '{}', '2020-01-01T00:00:00.000Z', 5, 25, 'queued',
             'boom', NULL, '2020-01-01T00:00:00.000Z', '2020-01-02T00:00:00.000Z'),
            ('kept', 'run-task-note', 'kept', '{}', '2020-01-01T00:00:00.000Z', 9, 9, 'queued',
             'boom', NULL, '2020-01-01T00:00:00.000Z', '2020-01-02T00:00:00.000Z');",
    )
    .await?;
    assert_eq!(store.flag_stuck().await?, 4);
    let mut keys = store
        .flagged()
        .await?
        .into_iter()
        .filter_map(|job| job.key)
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(
        keys,
        [
            "dead".to_owned(),
            "queued".to_owned(),
            "stale".to_owned(),
            "wide".to_owned()
        ]
    );
    assert_eq!(claim(store).await?, None);
    let left: i64 = store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT count(*) FROM ros_jobs WHERE job_key IN ('fresh', 'kept')",
                [],
                |row| row.get(0),
            )
            .map_err(crate::common::seed)
        })
        .await?;
    assert_eq!(left, 2);
    Ok(())
}

#[tokio::test]
async fn a_re_added_key_is_flagged_on_the_next_failure() -> TestResult {
    let fresh = fresh_store()?;
    let store = fresh.store.clone();
    testkit::memory_jobs::a_re_added_key_is_flagged_on_the_next_failure(
        &fresh.store,
        &fresh.store,
        async move || due(&store).await,
    )
    .await
}

#[tokio::test]
async fn failure_times_move_forward_and_a_completed_retry_drops_its_count() -> TestResult {
    let fresh = fresh_store()?;
    let due_store = fresh.store.clone();
    let count_store = fresh.store.clone();
    testkit::memory_jobs::failure_times_move_forward_and_a_completed_retry_drops_its_count(
        &fresh.store,
        &fresh.store,
        async move || due(&due_store).await,
        async move || failure_rows(&count_store).await,
    )
    .await
}

#[tokio::test]
async fn a_dismissed_ledger_id_can_be_retried() -> TestResult {
    let fresh = fresh_store()?;
    let store = fresh.store.clone();
    testkit::memory_jobs::a_dismissed_ledger_id_can_be_retried(
        &fresh.store,
        &fresh.store,
        async move || due(&store).await,
    )
    .await
}

#[tokio::test]
async fn replace_resets_attempts_and_the_maximum() -> TestResult {
    let fresh = fresh_store()?;
    let store = fresh.store.clone();
    testkit::memory_jobs::replace_resets_attempts_and_the_maximum(
        &fresh.store,
        &fresh.store,
        async move || due(&store).await,
    )
    .await
}

#[tokio::test]
async fn replace_detaches_a_job_that_is_still_running() -> TestResult {
    let fresh = fresh_store()?;
    let store = fresh.store.clone();
    testkit::memory_jobs::replace_detaches_a_job_that_is_still_running(
        &fresh.store,
        async move || due(&store).await,
    )
    .await
}

#[tokio::test]
async fn rolling_back_a_flag_leaves_the_job_queued_and_the_ledger_empty() -> TestResult {
    let fresh = fresh_store()?;
    let store = &fresh.store;
    store
        .enqueue(testkit::memory_jobs::ledger_job("rollback", 25)?)
        .await?;
    store
        .with_conn(|conn| {
            let tx = conn.unchecked_transaction().map_err(crate::common::seed)?;
            let id: String = tx
                .query_row("SELECT id FROM ros_jobs", [], |row| row.get(0))
                .map_err(crate::common::seed)?;
            memory_sqlite::archive_for_test(&tx, &id, "boom", Timestamp::now())?;
            tx.rollback().map_err(crate::common::seed)?;
            Ok(())
        })
        .await?;
    assert_eq!(store.flagged().await?, Vec::new());
    let state: String = store
        .with_conn(|conn| {
            conn.query_row("SELECT state FROM ros_jobs", [], |row| row.get(0))
                .map_err(crate::common::seed)
        })
        .await?;
    assert_eq!(state, "queued");
    Ok(())
}

async fn due(store: &SqliteStore) -> TestResult {
    store
        .with_conn(|conn| {
            conn.execute(
                "UPDATE ros_jobs SET run_at = ?1 WHERE state = 'queued'",
                params!["2020-01-01T00:00:00.000Z"],
            )
            .map(|_| ())
            .map_err(crate::common::seed)
        })
        .await?;
    Ok(())
}

async fn failure_rows(store: &SqliteStore) -> TestResult<i64> {
    store
        .with_conn(|conn| {
            conn.query_row("SELECT count(*) FROM rh_job_failures", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(crate::common::seed)
        })
        .await
        .map_err(|error| Box::new(error) as _)
}

async fn claim(
    store: &SqliteStore,
) -> Result<Option<kernel::queue::ClaimedJob>, kernel::queue::JobQueueError> {
    store
        .claim(&ClaimSelector::matching(vec![JobName::Fixed(
            FixedJob::EmbedTarget,
        )]))
        .await
}

async fn only(store: &SqliteStore) -> TestResult<kernel::flagged::FlaggedJob> {
    let mut rows = store.flagged().await?;
    let job = rows.pop().ok_or("no flagged job")?;
    assert_eq!(rows, Vec::new());
    Ok(job)
}
