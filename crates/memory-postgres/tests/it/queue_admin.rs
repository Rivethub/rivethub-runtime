use memory_postgres::PostgresMemory;
use sqlx::PgPool;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, sqlx::FromRow)]
struct JobRow {
    attempts: i16,
    max_attempts: i16,
    priority: i16,
    last_error: Option<String>,
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn maintenance_queue_admin_matches_the_shared_rules(pool: PgPool) -> TestResult {
    testkit::graphile_schema::install_graphile_worker(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    testkit::maintenance::maintenance_tasks_start_empty(&store).await?;
    seed_retry(&pool).await?;
    testkit::maintenance::retry_resets_only_wiki_errors(&store).await?;
    assert_eq!(job(&pool, "wiki-open").await?.attempts, 0);
    assert_eq!(job(&pool, "wiki-locked").await?.attempts, 0);
    assert_eq!(job(&pool, "wiki-open").await?.last_error, None);
    assert_eq!(job(&pool, "wiki-locked").await?.last_error, None);
    stayed_dead(&pool, "other", "other boom").await?;
    stayed_dead(&pool, "other-task", "wiki failed").await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn requeue_revives_only_the_idle_dead_jobs(pool: PgPool) -> TestResult {
    testkit::graphile_schema::install_graphile_worker(&pool).await?;
    seed_requeue(&pool).await?;
    let store = PostgresMemory::new(pool.clone());
    testkit::maintenance::requeue_revives_two_idle_jobs(&store).await?;
    revived(&pool, "old-open").await?;
    revived(&pool, "stale-lock").await?;
    stayed_dead(&pool, "fresh-lock", "stale").await?;
    stayed_dead(&pool, "recent", "stale").await?;
    Ok(())
}

async fn seed_retry(pool: &PgPool) -> TestResult {
    dead_job(pool, "extract-wiki", "wiki-open", "wiki failed", 30, None).await?;
    dead_job(
        pool,
        "extract-wiki",
        "wiki-locked",
        "wiki failed",
        28,
        Some(1),
    )
    .await?;
    dead_job(pool, "extract-wiki", "other", "other boom", 26, None).await?;
    dead_job(
        pool,
        "compact-conversation",
        "other-task",
        "wiki failed",
        24,
        None,
    )
    .await?;
    Ok(())
}

async fn seed_requeue(pool: &PgPool) -> TestResult {
    dead_job(pool, "extract-wiki", "old-open", "stale", 25, None).await?;
    dead_job(pool, "extract-wiki", "stale-lock", "stale", 25, Some(5)).await?;
    dead_job(pool, "extract-wiki", "fresh-lock", "stale", 25, Some(0)).await?;
    dead_job(pool, "extract-wiki", "recent", "stale", 0, None).await?;
    Ok(())
}

async fn dead_job(
    pool: &PgPool,
    task: &str,
    key: &str,
    error: &str,
    idle_hours: i32,
    lock_hours: Option<i32>,
) -> TestResult {
    sqlx::query(
        "SELECT (graphile_worker.add_job($1, '{}'::json, null, now(), 1, $2, 0, null::text[], 'replace')).id",
    )
    .bind(task)
    .bind(key)
    .execute(pool)
    .await?;
    sqlx::query(
        "UPDATE graphile_worker._private_jobs
            SET attempts = max_attempts,
                last_error = $2,
                updated_at = now() - make_interval(hours => $3),
                locked_at = CASE WHEN $4::bool THEN now() - make_interval(hours => $5) ELSE NULL END,
                locked_by = CASE WHEN $4::bool THEN 'worker' ELSE NULL END
          WHERE key = $1",
    )
    .bind(key)
    .bind(error)
    .bind(idle_hours)
    .bind(lock_hours.is_some())
    .bind(lock_hours.unwrap_or(0))
    .execute(pool)
    .await?;
    Ok(())
}

async fn job(pool: &PgPool, key: &str) -> Result<JobRow, sqlx::Error> {
    sqlx::query_as(
        "SELECT attempts, max_attempts, priority, last_error
           FROM graphile_worker._private_jobs WHERE key = $1",
    )
    .bind(key)
    .fetch_one(pool)
    .await
}

async fn stayed_dead(pool: &PgPool, key: &str, error: &str) -> TestResult {
    let row = job(pool, key).await?;
    assert_eq!(row.attempts, row.max_attempts);
    assert_eq!(row.last_error.as_deref(), Some(error));
    Ok(())
}

async fn revived(pool: &PgPool, key: &str) -> TestResult {
    let row = job(pool, key).await?;
    assert_eq!(row.attempts, 0);
    assert_eq!(row.priority, 10);
    assert_eq!(row.last_error.as_deref(), Some("stale"));
    Ok(())
}
