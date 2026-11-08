use chrono::{DateTime, TimeDelta, Utc};
use kernel::model_calls::{ModelCall, ModelCallStore as _, TurnMetadata};
use kernel::owner::Owner;
use kernel::tenancy::{Scope, Viewer};
use memory_postgres::PostgresMemory;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use wire::model_call::{CallOutcome, CallUsage};
use wire::session_id::SessionId;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const PROBE: &str = "rh_calls_probe";
const FIRST: &str = "claude-code:00000000-0000-4000-8000-0000000000c1";
const SECOND: &str = "codex:00000000-0000-7000-8000-0000000000c2";

fn at(seconds: i64) -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH + TimeDelta::seconds(1_700_000_000 + seconds)
}

fn call(
    id: &str,
    seconds: i64,
    session: Option<&str>,
) -> Result<ModelCall, Box<dyn std::error::Error>> {
    Ok(ModelCall {
        call_id: id.to_owned(),
        started_at: at(seconds),
        finished_at: None,
        provider: "anthropic".to_owned(),
        model_requested: None,
        model_served: None,
        account: None,
        status_code: None,
        outcome: CallOutcome::Completed,
        retry_after_secs: None,
        usage: None,
        session: session.map(str::parse::<SessionId>).transpose()?,
        turn: None,
    })
}

fn owner(name: &str) -> Result<Owner, Box<dyn std::error::Error>> {
    Ok(Owner::new(name.to_owned())?)
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_repeated_call_is_a_duplicate_and_changes_nothing(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let alice = owner("alice")?;
    let first = call("00000000-0000-4000-8000-0000000000e1", 0, Some(FIRST))?;
    let recorded = store.record(&alice, std::slice::from_ref(&first)).await?;
    assert_eq!((recorded.inserted, recorded.duplicate), (1, 0));

    let mut changed = first.clone();
    changed.provider = "openai".to_owned();
    changed.outcome = CallOutcome::ServerError;
    changed.usage = Some(CallUsage {
        input: 9,
        ..CallUsage::default()
    });
    let repeat = store.record(&alice, &[changed.clone(), changed]).await?;
    assert_eq!((repeat.inserted, repeat.duplicate), (0, 2));

    let session: SessionId = FIRST.parse()?;
    assert_eq!(store.session_calls(&session).await?, [first]);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_batch_that_repeats_its_own_call_counts_the_second_as_duplicate(
    pool: PgPool,
) -> TestResult {
    let store = PostgresMemory::new(pool);
    let one = call("00000000-0000-4000-8000-0000000000e1", 0, Some(FIRST))?;
    let recorded = store.record(&owner("alice")?, &[one.clone(), one]).await?;
    assert_eq!((recorded.inserted, recorded.duplicate), (1, 1));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_session_reads_its_own_calls_in_start_order_with_every_field(pool: PgPool) -> TestResult {
    let store = PostgresMemory::new(pool.clone());
    let mut late = call("00000000-0000-4000-8000-0000000000e3", 20, Some(SECOND))?;
    late.finished_at = Some(at(22));
    late.model_requested = Some("alias".to_owned());
    late.model_served = Some("model-a".to_owned());
    late.account = Some("work".to_owned());
    late.status_code = Some(429);
    late.outcome = CallOutcome::RateLimited;
    late.retry_after_secs = Some(30);
    late.usage = Some(CallUsage {
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 4,
        reasoning: 5,
    });
    late.turn = Some(TurnMetadata {
        turn_id: Some("00000000-0000-7000-8000-0000000000d1".to_owned()),
        request_kind: Some("turn".to_owned()),
    });
    let early = call("00000000-0000-4000-8000-0000000000e2", 10, Some(SECOND))?;
    let other = call("00000000-0000-4000-8000-0000000000e4", 5, Some(FIRST))?;
    let unmatched = call("00000000-0000-4000-8000-0000000000e5", 1, None)?;
    store
        .record(
            &owner("alice")?,
            &[late.clone(), early.clone(), other, unmatched],
        )
        .await?;

    let session: SessionId = SECOND.parse()?;
    assert_eq!(store.session_calls(&session).await?, [early, late]);

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
    let session: SessionId = FIRST.parse()?;
    let alice_call = call("00000000-0000-4000-8000-0000000000e1", 0, Some(FIRST))?;
    let bob_call = call("00000000-0000-4000-8000-0000000000e2", 1, Some(FIRST))?;
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

    let forged = call("00000000-0000-4000-8000-0000000000e3", 2, Some(FIRST))?;
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
