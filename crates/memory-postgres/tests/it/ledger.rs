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
