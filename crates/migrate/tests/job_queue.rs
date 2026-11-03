use sqlx::PgPool;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[sqlx::test(migrations = false)]
async fn migration_0052_creates_the_graphile_worker_schema(pool: PgPool) -> TestResult {
    migrate::postgres_pool(&pool).await?;
    let version: Option<i32> = sqlx::query_scalar("SELECT max(id) FROM graphile_worker.migrations")
        .fetch_one(&pool)
        .await?;
    assert_eq!(version, Some(19));
    let add_job: bool = sqlx::query_scalar(
        "SELECT to_regprocedure('graphile_worker.add_job(text,json,text,timestamptz,int,text,int,text[],text)') IS NOT NULL",
    )
    .fetch_one(&pool)
    .await?;
    assert!(add_job);
    let id: i64 = sqlx::query_scalar("SELECT (graphile_worker.add_job('embed-target', '{}')).id")
        .fetch_one(&pool)
        .await?;
    assert_eq!(id, 1);
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn migration_0052_leaves_an_existing_queue_schema_alone(pool: PgPool) -> TestResult {
    testkit::graphile_schema::install_graphile_worker(&pool).await?;
    migrate::postgres_pool(&pool).await?;
    let created: Option<String> =
        sqlx::query_scalar("SELECT to_regclass('graphile_worker.migrations')::text")
            .fetch_one(&pool)
            .await?;
    assert_eq!(created, None);
    let tracked: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM _rivetos_migrations WHERE name = '0052_job_queue.sql')",
    )
    .fetch_one(&pool)
    .await?;
    assert!(tracked);
    Ok(())
}
