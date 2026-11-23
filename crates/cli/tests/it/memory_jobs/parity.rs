use kernel::memory::jobs::MemoryJobStore;
use kernel::model::ModelProvider;
use kernel::queue::JobQueue;
use memory_postgres::PostgresMemory;
use memory_postgres::PostgresQueue;
use memory_sqlite::SqliteError;
use memory_sqlite::SqliteStore;
use rusqlite::params;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use wire::JsonObject;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn sqlite_a_tool_sentence_records_the_model_request() -> TestResult {
    let (_dir, store) = sqlite_store()?;
    let jobs = store.clone();
    testkit::memory_jobs::a_tool_sentence_records_the_model_request(
        &store,
        workers::tool_synthesis::TOOL_SYNTH_SYSTEM_PROMPT,
        async move |provider, payload| synthesize(&jobs, provider, payload).await,
    )
    .await
}

#[tokio::test]
async fn sqlite_synthesis_drops_missing_populated_and_nameless_calls() -> TestResult {
    let (_dir, store) = sqlite_store()?;
    let jobs = store.clone();
    testkit::memory_jobs::synthesis_drops_missing_populated_and_nameless_calls(
        &store,
        async move |provider, payload| synthesize(&jobs, provider, payload).await,
    )
    .await
}

#[tokio::test]
async fn sqlite_synthesis_rejects_bad_sentences_and_accepts_a_long_unicode_one() -> TestResult {
    let (_dir, store) = sqlite_store()?;
    let jobs = store.clone();
    testkit::memory_jobs::synthesis_rejects_bad_sentences_and_accepts_a_long_unicode_one(
        &store,
        async move |provider, payload| synthesize(&jobs, provider, payload).await,
    )
    .await
}

#[tokio::test]
async fn sqlite_a_recent_leaf_is_queued_ahead_of_an_old_one() -> TestResult {
    let (_dir, store) = sqlite_store()?;
    let jobs = store.clone();
    let queue = store.clone();
    testkit::memory_jobs::a_recent_leaf_is_queued_ahead_of_an_old_one(
        &store,
        &store,
        async move |enabled, cap| sweep(&jobs, &queue, enabled, cap).await,
    )
    .await
}

#[tokio::test]
async fn sqlite_dead_wiki_jobs_outside_the_windows_stay_put() -> TestResult {
    let (_dir, store) = sqlite_store()?;
    let jobs = store.clone();
    let queue = store.clone();
    let aged = store.clone();
    testkit::memory_jobs::dead_wiki_jobs_outside_the_windows_stay_put(
        &store,
        &store,
        async move |dead| age_sqlite(&aged, dead.key, dead.idle_hours, dead.locked_hours).await,
        async move |enabled, cap| sweep(&jobs, &queue, enabled, cap).await,
    )
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn postgres_a_tool_sentence_records_the_model_request(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let jobs = PostgresMemory::new(pool);
    testkit::memory_jobs::a_tool_sentence_records_the_model_request(
        &memory,
        workers::tool_synthesis::TOOL_SYNTH_SYSTEM_PROMPT,
        async move |provider, payload| synthesize(&jobs, provider, payload).await,
    )
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn postgres_synthesis_drops_missing_populated_and_nameless_calls(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let jobs = PostgresMemory::new(pool);
    testkit::memory_jobs::synthesis_drops_missing_populated_and_nameless_calls(
        &memory,
        async move |provider, payload| synthesize(&jobs, provider, payload).await,
    )
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn postgres_synthesis_rejects_bad_sentences_and_accepts_a_long_unicode_one(
    pool: PgPool,
) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let jobs = PostgresMemory::new(pool);
    testkit::memory_jobs::synthesis_rejects_bad_sentences_and_accepts_a_long_unicode_one(
        &memory,
        async move |provider, payload| synthesize(&jobs, provider, payload).await,
    )
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn postgres_a_recent_leaf_is_queued_ahead_of_an_old_one(pool: PgPool) -> TestResult {
    testkit::install_graphile_worker(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    let jobs = PostgresMemory::new(pool.clone());
    let queue = PostgresQueue::connect(pool.clone()).await?;
    let sweep_queue = PostgresQueue::connect(pool).await?;
    testkit::memory_jobs::a_recent_leaf_is_queued_ahead_of_an_old_one(
        &memory,
        &queue,
        async move |enabled, cap| sweep(&jobs, &sweep_queue, enabled, cap).await,
    )
    .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn postgres_dead_wiki_jobs_outside_the_windows_stay_put(pool: PgPool) -> TestResult {
    testkit::install_graphile_worker(&pool).await?;
    let memory = PostgresMemory::new(pool.clone());
    let jobs = PostgresMemory::new(pool.clone());
    let queue = PostgresQueue::connect(pool.clone()).await?;
    let sweep_queue = PostgresQueue::connect(pool.clone()).await?;
    testkit::memory_jobs::dead_wiki_jobs_outside_the_windows_stay_put(
        &memory,
        &queue,
        async move |dead| age_postgres(&pool, dead.key, dead.idle_hours, dead.locked_hours).await,
        async move |enabled, cap| sweep(&jobs, &sweep_queue, enabled, cap).await,
    )
    .await
}

fn sqlite_store() -> Result<(tempfile::TempDir, SqliteStore), Box<dyn std::error::Error>> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    Ok((dir, SqliteStore::open(&path)?))
}

async fn synthesize(
    store: &dyn MemoryJobStore,
    provider: &dyn ModelProvider,
    payload: &JsonObject,
) -> Result<(), String> {
    workers::tool_synthesis::handle_synthesize_tool_call(
        store,
        provider,
        "summary-model",
        payload,
        &CancellationToken::new(),
    )
    .await
    .map_err(|error| error.to_string())
}

async fn sweep(
    store: &dyn MemoryJobStore,
    queue: &dyn JobQueue,
    enabled: bool,
    cap: u32,
) -> Result<(u64, u64), Box<dyn std::error::Error + Send + Sync>> {
    let sweep = workers::embedding_sweep::handle_enqueue_stale_wiki(
        store,
        queue,
        enabled,
        cap,
        &CancellationToken::new(),
    )
    .await
    .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?;
    Ok((
        sweep.revived,
        u64::try_from(sweep.added)
            .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?,
    ))
}

async fn age_sqlite(
    store: &SqliteStore,
    key: String,
    idle_hours: i64,
    locked_hours: Option<i64>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let updated = ago(idle_hours)?;
    let locked = match locked_hours {
        Some(hours) => Some(ago(hours)?),
        None => None,
    };
    let changed = store
        .with_conn(move |conn| {
            conn.execute(
                "UPDATE ros_jobs
                    SET state = 'dead', attempts = max_attempts, updated_at = ?2,
                        run_at = ?2, locked_at = ?3
                  WHERE job_key = ?1",
                params![key, updated, locked],
            )
            .map_err(|source| SqliteError::Query {
                operation: "age wiki job",
                source,
            })
        })
        .await
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?;
    if changed == 1 {
        Ok(())
    } else {
        Err(format!("aged {changed} wiki jobs").into())
    }
}

fn ago(hours: i64) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    use wire::time::TimestampFormat as _;
    let at = jiff::Timestamp::now()
        .checked_sub(jiff::SignedDuration::from_hours(hours))
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?;
    Ok(at.rfc3339_millis())
}

async fn age_postgres(
    pool: &PgPool,
    key: String,
    idle_hours: i64,
    locked_hours: Option<i64>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let idle = i32::try_from(idle_hours)
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?;
    let locked = locked_hours
        .map(i32::try_from)
        .transpose()
        .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?;
    let changed = sqlx::query(
        "UPDATE graphile_worker._private_jobs
            SET attempts = max_attempts, priority = 3,
                run_at = now() - make_interval(hours => $2),
                updated_at = now() - make_interval(hours => $2),
                locked_at = CASE WHEN $3::int IS NULL THEN NULL
                                 ELSE now() - make_interval(hours => $3) END,
                locked_by = CASE WHEN $3::int IS NULL THEN NULL ELSE 'gone' END
          WHERE key = $1",
    )
    .bind(key)
    .bind(idle)
    .bind(locked)
    .execute(pool)
    .await
    .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)?
    .rows_affected();
    if changed == 1 {
        Ok(())
    } else {
        Err(format!("aged {changed} wiki jobs").into())
    }
}
