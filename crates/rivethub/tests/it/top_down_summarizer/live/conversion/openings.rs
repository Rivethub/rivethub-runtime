use kernel::queue::ClaimSelector;

use super::*;

const IMPORTED: Uuid = Uuid::from_u128(1);
const NO_REQUEST: Uuid = Uuid::from_u128(2);

fn dump() -> String {
    format!(
        "COPY public.ros_conversations (id, session_key, agent, channel, created_at, updated_at, owner_user_id) FROM stdin;
{IMPORTED}\tcodex:imported-fixture\tfixture\tcli\t2024-01-01T00:00:00Z\t2024-01-01T00:00:00Z\talice
{NO_REQUEST}\tcodex:assistant-only\tfixture\tcli\t2024-01-01T00:00:00Z\t2024-01-01T00:00:00Z\talice
\\.
COPY public.ros_messages (id, conversation_id, agent, channel, role, content, created_at, owner_user_id) FROM stdin;
{}\t{IMPORTED}\tfixture\tcli\tassistant\tRead the inventory.\t2024-01-01T00:01:00Z\talice
{}\t{IMPORTED}\tfixture\tcli\tuser\t \\t\t2024-01-01T00:02:00Z\talice
{}\t{IMPORTED}\tfixture\tcli\tuser\t<user_query>Measure the brass pipe</user_query>\t2024-01-01T00:03:00Z\talice
{}\t{IMPORTED}\tfixture\tcli\tuser\tLabel the replacement pipe\t2024-01-01T00:04:00Z\talice
{}\t{NO_REQUEST}\tfixture\tcli\tassistant\tAn assistant-only note.\t2024-01-01T00:01:00Z\talice
\\.
-- PostgreSQL database dump complete
",
        Uuid::from_u128(101),
        Uuid::from_u128(102),
        Uuid::from_u128(103),
        Uuid::from_u128(104),
        Uuid::from_u128(105),
    )
}

const UNFILABLE: Uuid = Uuid::from_u128(33);
const MISSING: Uuid = Uuid::from_u128(34);

async fn verify_arm_diagnostics(store: &impl SummaryStore) -> TestResult {
    let capability = store.summary_conversion().ok_or("conversion support")?;
    for _ in 0..2 {
        for (conversation, expected) in [
            (UNFILABLE, "cannot be filed because it is a sandbox run"),
            (NO_REQUEST, "has no qualifying user request"),
            (MISSING, "was not found"),
        ] {
            let Err(error) = capability
                .control(ConversionControl::ArmOpening { conversation })
                .await
            else {
                return Err(format!("opening should be refused for {conversation}").into());
            };
            assert!(error.to_string().contains(expected), "{error}");
        }
        testkit::settlement::expire(store, Timestamp::now(), 2).await?;
    }
    let opening = store
        .opening_title(UNFILABLE)
        .await?
        .ok_or("dormant sandbox opening")?;
    assert!(!opening.armed);
    assert_eq!(store.undelivered_openings().await?, [] as [Job; 0]);
    Ok(())
}

fn sandbox_dump() -> String {
    format!(
        "COPY public.ros_conversations (id, session_key, agent, channel, created_at, updated_at, owner_user_id) FROM stdin;
{UNFILABLE}\tgrokbot-run-sand-subagent-fixture\tfixture\tcli\t2024-01-01T00:00:00Z\t2024-01-01T00:00:00Z\talice
{NO_REQUEST}\tcodex:assistant-only-fixture\tfixture\tcli\t2024-01-01T00:00:00Z\t2024-01-01T00:00:00Z\talice
\\.
COPY public.ros_messages (id, conversation_id, agent, channel, role, content, created_at, owner_user_id) FROM stdin;
{}\t{UNFILABLE}\tfixture\tcli\tuser\tRecord the copper tags\t2024-01-01T00:01:00Z\talice
{}\t{NO_REQUEST}\tfixture\tcli\tassistant\tAn assistant-only note\t2024-01-01T00:01:00Z\talice
\\.
-- PostgreSQL database dump complete
",
        Uuid::from_u128(106),
        Uuid::from_u128(107),
    )
}

async fn exercise(store: &impl SummaryStore, jobs: &dyn JobQueue) -> TestResult {
    assert!(store.opening_title(IMPORTED).await?.is_none());
    testkit::settlement::expire(store, Timestamp::now(), 2).await?;
    let opening = store
        .opening_title(IMPORTED)
        .await?
        .ok_or("imported opening")?;
    assert_eq!(opening.content, "Measure the brass pipe");
    assert!(!opening.armed);
    assert_eq!(store.undelivered_openings().await?, [] as [Job; 0]);

    let capability = store.summary_conversion().ok_or("conversion support")?;
    capability
        .control(ConversionControl::Start {
            rate: None,
            include_giants: false,
        })
        .await?;
    let provider = Provider::new();
    provider.chat_gate.store(false, SeqCst);
    let permits = Semaphore::new(3);
    for _ in 0..3 {
        tick(store, &provider, &permits).await?;
    }
    assert_eq!(capability.status().await?.complete, 2);
    capability
        .control(ConversionControl::Release {
            conversation: IMPORTED,
        })
        .await?;
    capability
        .control(ConversionControl::ArmOpening {
            conversation: IMPORTED,
        })
        .await?;
    enqueue_and_claim(store, jobs).await?;

    let Err(missing) = capability
        .control(ConversionControl::ArmOpening {
            conversation: NO_REQUEST,
        })
        .await
    else {
        return Err("assistant-only conversation accepted opening work".into());
    };
    assert!(
        missing
            .to_string()
            .contains("has no qualifying user request"),
        "{missing}"
    );
    Ok(())
}

async fn enqueue_and_claim(store: &dyn SummaryStore, jobs: &dyn JobQueue) -> TestResult {
    for job in store.undelivered_openings().await? {
        jobs.enqueue(job).await?;
    }
    for name in [FixedJob::FileConversation, FixedJob::TitleConversation] {
        let job = jobs
            .claim(&ClaimSelector::matching(vec![JobName::Fixed(name)]))
            .await?
            .ok_or("opening job")?;
        let request: kernel::memory::summary_opening::OpeningJob = job.payload.deserialize()?;
        assert_eq!(request.conversation_id, IMPORTED);
    }
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_conversion_imported_opening_postgres(pool: sqlx::PgPool) -> TestResult {
    testkit::install_graphile_worker(&pool).await?;
    let scoped = memory_postgres::scoped(sqlx::postgres::PgPoolOptions::new())
        .connect_with(pool.connect_options().as_ref().clone())
        .await?;
    let store = memory_postgres::PostgresMemory::new(scoped).with_summary_index_key([7; 32]);
    Scope::Maintenance
        .run(async {
            store
                .import_memory_dump(&dump(), false, &CancellationToken::new())
                .await?;
            let queue = store.queue().await?;
            exercise(&store, &queue).await
        })
        .await
}

#[tokio::test]
async fn top_down_conversion_imported_opening_sqlite() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("s.sqlite");
    migrate::sqlite(&path)?;
    let store = memory_sqlite::SqliteStore::open(&path)?.with_summary_index_key([7; 32]);
    let archive = dir.path().join("dump.sql");
    std::fs::write(&archive, dump())?;
    store
        .import_pg_dump(archive, CancellationToken::new())
        .await?;
    exercise(&store, &store).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_arm_opening_diagnostics_postgres(pool: sqlx::PgPool) -> TestResult {
    let scoped = memory_postgres::scoped(sqlx::postgres::PgPoolOptions::new())
        .connect_with(pool.connect_options().as_ref().clone())
        .await?;
    let store = memory_postgres::PostgresMemory::new(scoped);
    Scope::Maintenance
        .run(async {
            store
                .import_memory_dump(&sandbox_dump(), false, &CancellationToken::new())
                .await?;
            verify_arm_diagnostics(&store).await
        })
        .await
}

#[tokio::test]
async fn top_down_arm_opening_diagnostics_sqlite() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("s.sqlite");
    migrate::sqlite(&path)?;
    let store = memory_sqlite::SqliteStore::open(&path)?;
    let archive = dir.path().join("dump.sql");
    std::fs::write(&archive, sandbox_dump())?;
    store
        .import_pg_dump(archive, CancellationToken::new())
        .await?;
    verify_arm_diagnostics(&store).await
}

fn non_request_dump() -> String {
    dump().replace(
        "assistant\tAn assistant-only note.",
        "user\t/model choose a different model",
    )
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn top_down_sweep_skips_examined_non_request_postgres(pool: sqlx::PgPool) -> TestResult {
    let scoped = memory_postgres::scoped(sqlx::postgres::PgPoolOptions::new())
        .connect_with(pool.connect_options().as_ref().clone())
        .await?;
    let store = memory_postgres::PostgresMemory::new(scoped);
    Scope::Maintenance.run(async {
        store.import_memory_dump(&non_request_dump(), false, &CancellationToken::new()).await?;
        let now = Timestamp::now();
        testkit::settlement::expire(&store, now, 2).await?;
        assert!(store.opening_title(NO_REQUEST).await?.is_none());

        let mut lock = pool.begin().await?;
        sqlx::query("SELECT id FROM rh_conversations WHERE id = $1 FOR UPDATE")
            .bind(NO_REQUEST).execute(&mut *lock).await?;
        tokio::time::timeout(Duration::from_secs(2), testkit::settlement::expire(&store, now, 2))
            .await
            .map_err(|_| "second sweep tried to lock a conversation it had already examined")??;
        lock.rollback().await?;

        sqlx::query(
            "INSERT INTO rh_messages (id, conversation_id, agent, channel, role, content, created_at, owner_user_id)
             VALUES ($1, $2, 'fixture', 'cli', 'user', 'Review the copper tags', '2023-12-31', 'alice')",
        )
        .bind(Uuid::from_u128(108))
        .bind(NO_REQUEST)
        .execute(&pool)
        .await?;
        testkit::settlement::expire(&store, now, 2).await?;
        let opening = store.opening_title(NO_REQUEST).await?.ok_or("opening after new user message")?;
        assert_eq!(opening.content, "Review the copper tags");
        assert!(!opening.armed);
        Ok(())
    }).await
}

#[tokio::test]
async fn top_down_sweep_skips_examined_non_request_sqlite() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("s.sqlite");
    migrate::sqlite(&path)?;
    let store = memory_sqlite::SqliteStore::open(&path)?;
    let archive = dir.path().join("dump.sql");
    std::fs::write(&archive, non_request_dump())?;
    store
        .import_pg_dump(archive, CancellationToken::new())
        .await?;
    let now = Timestamp::now();
    testkit::settlement::expire(&store, now, 2).await?;
    assert!(store.opening_title(NO_REQUEST).await?.is_none());

    store
        .with_conn(|conn| {
            let checked: i64 = conn.query_row(
            "SELECT opening_checked_user_count FROM rh_summary_state WHERE conversation_id = ?1",
            [NO_REQUEST.to_string()],
            |row| row.get(0),
        ).map_err(|source| memory_sqlite::SqliteError::Query { operation: "read examined user count", source })?;
            assert_eq!(checked, 1);
            conn.execute_batch(
                "CREATE TEMP TRIGGER reject_reexamining_openings
             BEFORE UPDATE OF opening_checked_user_count ON rh_summary_state
             BEGIN SELECT raise(ABORT, 'opening already examined'); END;",
            )
            .map_err(|source| memory_sqlite::SqliteError::Query {
                operation: "guard examined user count",
                source,
            })?;
            Ok(())
        })
        .await?;
    testkit::settlement::expire(&store, now, 2).await?;

    store.with_conn(|conn| {
        conn.execute_batch("DROP TRIGGER reject_reexamining_openings")
            .map_err(|source| memory_sqlite::SqliteError::Query { operation: "remove examined user count guard", source })?;
        conn.execute(
            "INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, created_at)
             VALUES (?1, ?2, 'fixture', 'cli', 'user', 'Review the copper tags', '2023-12-31T00:00:00Z')",
            [Uuid::from_u128(108).to_string(), NO_REQUEST.to_string()],
        ).map_err(|source| memory_sqlite::SqliteError::Query { operation: "append a qualifying user message", source })?;
        Ok(())
    }).await?;
    testkit::settlement::expire(&store, now, 2).await?;
    let opening = store
        .opening_title(NO_REQUEST)
        .await?
        .ok_or("opening after new user message")?;
    assert_eq!(opening.content, "Review the copper tags");
    assert!(!opening.armed);
    Ok(())
}
