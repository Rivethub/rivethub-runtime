use std::time::Duration;

use kernel::queue::pause::PausableRole;
use kernel::queue::pause::WorkerPauses as _;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use sqlx::postgres::PgListener;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn pausing_twice_keeps_the_first_pause_and_resuming_twice_is_harmless(
    pool: PgPool,
) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    assert_eq!(store.paused().await?, Vec::new());

    let first = store.pause(PausableRole::EntityBuilder, "owner").await?;
    let again = store
        .pause(PausableRole::EntityBuilder, "someone-else")
        .await?;
    assert_eq!(again, first);
    assert_eq!(first.paused_by.as_deref(), Some("owner"));
    store.pause(PausableRole::Categorizer, "owner").await?;
    let roles: Vec<_> = store
        .paused()
        .await?
        .into_iter()
        .map(|pause| pause.role)
        .collect();
    assert_eq!(
        roles,
        [PausableRole::Categorizer, PausableRole::EntityBuilder]
    );

    store.resume(PausableRole::EntityBuilder).await?;
    store.resume(PausableRole::EntityBuilder).await?;
    store.resume(PausableRole::WikiBuilder).await?;
    let roles: Vec<_> = store
        .paused()
        .await?
        .into_iter()
        .map(|pause| pause.role)
        .collect();
    assert_eq!(roles, [PausableRole::Categorizer]);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn resuming_a_paused_role_wakes_the_job_listeners(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    store.pause(PausableRole::WikiBuilder, "owner").await?;
    let mut listener = PgListener::connect_with(&pool).await?;
    listener.listen("jobs:insert").await?;

    store.resume(PausableRole::WikiBuilder).await?;
    let woken = tokio::time::timeout(Duration::from_secs(5), listener.recv()).await??;
    assert_eq!(woken.channel(), "jobs:insert");
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_table_refuses_a_role_that_cannot_pause(pool: PgPool) -> TestResult {
    let refused =
        sqlx::query("INSERT INTO rh_worker_pauses (role, paused_at) VALUES ('summarizer', now())")
            .execute(&pool)
            .await;
    let error = refused.err().ok_or("summarizer row was accepted")?;
    assert!(error.to_string().contains("check constraint"), "{error}");
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_pause_racing_another_pause_returns_the_committed_one(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let mut racing = pool.begin().await?;
    sqlx::query(
        "INSERT INTO rh_worker_pauses (role, paused_at, paused_by)
         VALUES ('entity-builder', now(), 'first')",
    )
    .execute(&mut *racing)
    .await?;

    let second =
        tokio::spawn(async move { store.pause(PausableRole::EntityBuilder, "second").await });
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity
                 WHERE wait_event_type = 'Lock' AND query LIKE '%rh_worker_pauses%'",
            )
            .fetch_one(&pool)
            .await?;
            if blocked > 0 {
                return Ok::<_, sqlx::Error>(());
            }
        }
    })
    .await??;
    racing.commit().await?;

    let pause = tokio::time::timeout(Duration::from_secs(5), second).await???;
    assert_eq!(pause.paused_by.as_deref(), Some("first"));
    Ok(())
}
