use std::time::Duration;
use std::time::Instant;

use kernel::deadline::Deadline;
use kernel::memory::MemoryStoreError;
use kernel::memory::SessionCatalog as _;
use kernel::memory::SessionQuery;
use kernel::memory::SessionView;
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;

type TestResult = Result<(), Box<dyn std::error::Error>>;

async fn one_connection(pool: &PgPool) -> Result<PgPool, sqlx::Error> {
    memory_postgres::scoped(PgPoolOptions::new().max_connections(1))
        .connect_with(pool.connect_options().as_ref().clone())
        .await
}

async fn statement_timeout(pool: &PgPool) -> Result<String, sqlx::Error> {
    sqlx::query_scalar("SELECT current_setting('statement_timeout')")
        .fetch_one(pool)
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_bounded_request_stops_its_statement_at_the_deadline(pool: PgPool) -> TestResult {
    let pool = one_connection(&pool).await?;
    let unbounded = statement_timeout(&pool).await?;
    let started = Instant::now();
    let outcome = Deadline::after(Duration::from_millis(300))
        .run(sqlx::query("SELECT pg_sleep(30)").execute(&pool))
        .await;
    let waited = started.elapsed();
    let code = match outcome {
        Ok(_) => None,
        Err(error) => error
            .as_database_error()
            .and_then(|database| database.code().map(std::borrow::Cow::into_owned)),
    };
    assert_eq!(code.as_deref(), Some("57014"), "the statement is cancelled");
    assert!(
        waited < Duration::from_secs(10),
        "cancelled at the deadline, not after {waited:?}"
    );
    assert_eq!(
        statement_timeout(&pool).await?,
        unbounded,
        "the same connection outside a request keeps the server's own limit"
    );
    let bounded = Deadline::after(Duration::from_secs(60))
        .run(statement_timeout(&pool))
        .await?;
    assert_ne!(
        bounded, unbounded,
        "a request bounds every statement it runs"
    );
    let spent = Deadline::after(Duration::ZERO)
        .run(sqlx::query("SELECT pg_sleep(5)").execute(&pool))
        .await;
    assert!(
        spent.is_err(),
        "a request past its deadline runs nothing long"
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_catalog_read_past_its_deadline_is_unavailable(pool: PgPool) -> TestResult {
    let mut holder = pool.begin().await?;
    sqlx::query("LOCK TABLE rh_session_keeps IN ACCESS EXCLUSIVE MODE")
        .execute(&mut *holder)
        .await?;
    let memory = PostgresMemory::new(one_connection(&pool).await?);
    let started = Instant::now();
    let listed = Deadline::after(Duration::from_millis(300))
        .run(memory.list(&SessionQuery::new(SessionView::All)))
        .await;
    assert!(
        matches!(listed, Err(MemoryStoreError::Unavailable(_))),
        "a read cut off at its deadline is unavailable, not a failed query: {listed:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    holder.rollback().await?;
    Ok(())
}
