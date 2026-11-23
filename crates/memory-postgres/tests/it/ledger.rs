use memory_postgres::PostgresMemory;
use memory_postgres::PostgresQueue;
use sqlx::PgPool;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_re_added_key_is_flagged_on_the_next_failure(pool: PgPool) -> TestResult {
    let (memory, queue, due) = ready(pool).await?;
    testkit::memory_jobs::a_re_added_key_is_flagged_on_the_next_failure(&memory, &queue, due).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn failure_times_move_forward_and_a_completed_retry_drops_its_count(
    pool: PgPool,
) -> TestResult {
    let counted = pool.clone();
    let (memory, queue, due) = ready(pool).await?;
    testkit::memory_jobs::failure_times_move_forward_and_a_completed_retry_drops_its_count(
        &memory,
        &queue,
        due,
        async move || failure_rows(&counted).await,
    )
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_dismissed_ledger_id_can_be_retried(pool: PgPool) -> TestResult {
    let (memory, queue, due) = ready(pool).await?;
    testkit::memory_jobs::a_dismissed_ledger_id_can_be_retried(&memory, &queue, due).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn replace_resets_attempts_and_the_maximum(pool: PgPool) -> TestResult {
    let (memory, queue, due) = ready(pool).await?;
    testkit::memory_jobs::replace_resets_attempts_and_the_maximum(&memory, &queue, due).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn replace_detaches_a_job_that_is_still_running(pool: PgPool) -> TestResult {
    let (_memory, queue, due) = ready(pool).await?;
    testkit::memory_jobs::replace_detaches_a_job_that_is_still_running(&queue, due).await
}

async fn ready(
    pool: PgPool,
) -> TestResult<(
    PostgresMemory,
    PostgresQueue,
    impl AsyncFnMut() -> TestResult,
)> {
    let memory = PostgresMemory::new(pool.clone());
    let queue = PostgresQueue::connect(pool.clone()).await?;
    let due = async move || make_due(&pool).await;
    Ok((memory, queue, due))
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_swept_job_keeps_the_queue_time(pool: PgPool) -> TestResult {
    let planted = pool.clone();
    let (memory, queue, due) = ready(pool).await?;
    testkit::memory_jobs::a_swept_job_keeps_the_queue_time(&memory, &queue, due, async move || {
        plant_updated_at(&planted, "swept").await
    })
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_job_past_the_flag_threshold_is_claimed_before_the_sweep(pool: PgPool) -> TestResult {
    let planted = pool.clone();
    let (memory, queue, _due) = ready(pool).await?;
    testkit::memory_jobs::a_job_past_the_flag_threshold_is_claimed_before_the_sweep(
        &memory,
        &queue,
        async move || plant_claimable(&planted).await,
    )
    .await
}

async fn plant_updated_at(pool: &PgPool, key: &str) -> TestResult {
    let changed = sqlx::query(
        "UPDATE graphile_worker._private_jobs
            SET attempts = 5, updated_at = '2020-01-02T03:04:05Z', locked_at = NULL,
                last_error = NULL
          WHERE key = $1",
    )
    .bind(key)
    .execute(pool)
    .await?
    .rows_affected();
    assert_eq!(changed, 1);
    Ok(())
}

async fn plant_claimable(pool: &PgPool) -> TestResult {
    let changed = sqlx::query(
        "UPDATE graphile_worker._private_jobs
            SET attempts = 5, locked_at = NULL, last_error = NULL,
                run_at = now() - interval '1 minute'
          WHERE key = 'early'",
    )
    .execute(pool)
    .await?
    .rows_affected();
    assert_eq!(changed, 1);
    Ok(())
}

async fn make_due(pool: &PgPool) -> TestResult {
    sqlx::query("UPDATE graphile_worker._private_jobs SET run_at = now()")
        .execute(pool)
        .await?;
    Ok(())
}

async fn failure_rows(pool: &PgPool) -> TestResult<i64> {
    Ok(sqlx::query_scalar("SELECT count(*) FROM rh_job_failures")
        .fetch_one(pool)
        .await?)
}
