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
            .contains("no opening from a user request"),
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
