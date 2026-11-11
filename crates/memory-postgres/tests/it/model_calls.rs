use kernel::model_calls::ModelCallStore as _;
use kernel::owner::Owner;
use kernel::tenancy::{Scope, Viewer};
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use testkit::model_calls::{self, CLAUDE_SESSION, call};
use wire::session_id::SessionId;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const PROBE: &str = "rh_calls_probe";

fn owner(name: &str) -> Result<Owner, Box<dyn std::error::Error>> {
    Ok(Owner::new(name.to_owned())?)
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_repeated_call_is_a_duplicate_and_changes_nothing(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool);
    model_calls::a_repeated_call_is_a_duplicate_and_changes_nothing(&store).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_batch_that_repeats_its_own_call_counts_the_second_as_duplicate(
    pool: PgPool,
) -> TestResult {
    let store = PostgresMemory::new(pool);
    model_calls::a_batch_that_repeats_its_own_call_counts_the_second_as_duplicate(&store).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_session_reads_its_own_calls_in_start_order_with_every_field(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    model_calls::a_session_reads_its_own_calls_in_start_order_with_every_field(&store).await?;
    let unclaimed: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rh_model_calls WHERE session_key IS NULL")
            .fetch_one(&pool)
            .await?;
    assert_eq!(unclaimed, 1);
    Ok(())
}

async fn probe(pool: &PgPool) -> Result<PgPool, Box<dyn std::error::Error>> {
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DO $$ BEGIN CREATE ROLE {PROBE} NOLOGIN; \
         EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$"
    )))
    .execute(pool)
    .await?;
    for grant in [
        format!("GRANT USAGE ON SCHEMA public TO {PROBE}"),
        format!("GRANT ALL ON ALL TABLES IN SCHEMA public TO {PROBE}"),
    ] {
        sqlx::query(sqlx::AssertSqlSafe(grant))
            .execute(pool)
            .await?;
    }
    let options = pool
        .connect_options()
        .as_ref()
        .clone()
        .options([("role", PROBE)]);
    Ok(
        memory_postgres::scoped(PgPoolOptions::new().max_connections(2))
            .connect_with(options)
            .await?,
    )
}

fn viewer(name: &str) -> Result<Scope, Box<dyn std::error::Error>> {
    Ok(Scope::Viewer(Viewer::new(owner(name)?, Vec::new())))
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_person_login_reads_and_writes_only_its_own_calls(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(probe(&pool).await?);
    let session: SessionId = CLAUDE_SESSION.parse()?;
    let alice_call = call(
        "00000000-0000-4000-8000-0000000000e1",
        0,
        Some(CLAUDE_SESSION),
    )?;
    let bob_call = call(
        "00000000-0000-4000-8000-0000000000e2",
        1,
        Some(CLAUDE_SESSION),
    )?;
    let alice = owner("alice")?;
    let bob = owner("bob")?;

    viewer("alice")?
        .run(store.record(&alice, std::slice::from_ref(&alice_call)))
        .await?;
    viewer("bob")?
        .run(store.record(&bob, std::slice::from_ref(&bob_call)))
        .await?;

    let seen_by_alice = viewer("alice")?.run(store.session_calls(&session)).await?;
    assert_eq!(seen_by_alice, [alice_call]);
    let seen_by_bob = viewer("bob")?.run(store.session_calls(&session)).await?;
    assert_eq!(seen_by_bob, [bob_call.clone()]);
    assert!(
        Scope::Closed
            .run(store.session_calls(&session))
            .await?
            .is_empty()
    );
    assert_eq!(
        Scope::Maintenance
            .run(store.session_calls(&session))
            .await?
            .len(),
        2
    );

    let forged = call(
        "00000000-0000-4000-8000-0000000000e3",
        2,
        Some(CLAUDE_SESSION),
    )?;
    let refused = viewer("bob")?
        .run(store.record(&alice, std::slice::from_ref(&forged)))
        .await;
    let message = refused
        .err()
        .map(|error| format!("{error:?}"))
        .unwrap_or_default();
    assert!(
        message.contains("row-level security"),
        "a login cannot write a row for another person: {message}"
    );
    Ok(())
}
