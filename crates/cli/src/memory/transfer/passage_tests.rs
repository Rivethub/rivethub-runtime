use kernel::memory::summary_backend::{SUMMARY_EMBEDDING_WIDTH, SummaryIndex, SummaryPassage};
use kernel::memory::{Limit, MemoryScope, MemoryStore, SearchMode, SearchQuery, SummaryStore};
use memory_postgres::PostgresMemory;
use memory_sqlite::{SqliteError, SqliteStore};
use sqlx::PgPool;
use uuid::Uuid;

use super::{TestResult, authorization, fixture, store};
use crate::memory::transfer::{Direction, run};

const RECORD: Uuid = Uuid::from_u128(8);
const SOURCE_KEY: [u8; 32] = [0; 32];
const OTHER_KEY: [u8; 32] = [0xab; 32];

async fn seed_vectors(source: &SqliteStore) -> TestResult {
    fixture(source).await?;
    let bytes: Vec<u8> = vec![0.25_f32; SUMMARY_EMBEDDING_WIDTH]
        .into_iter()
        .flat_map(f32::to_le_bytes)
        .collect();
    source
        .with_conn(move |conn| {
            conn.execute("UPDATE ros_summary_passages SET embedding = ?1", [bytes])
                .map_err(SqliteError::Pragma)?;
            Ok(())
        })
        .await?;
    Ok(())
}

async fn copy(
    pool: &PgPool,
    local: &SqliteStore,
    direction: Direction,
    key: [u8; 32],
) -> TestResult {
    let owner = kernel::owner::Owner::new("alice".into())?;
    run(
        pool,
        local,
        [&owner, &owner],
        direction,
        (false, Some(key)),
        &tokio_util::sync::CancellationToken::new(),
    )
    .await?;
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn compatible_passages_reuse_vectors_and_are_searchable(pool: PgPool) -> TestResult {
    let dir = tempfile::tempdir()?;
    let source = store(&dir, "source.sqlite")?;
    let output = store(&dir, "compatible.sqlite")?.with_summary_index_key(SOURCE_KEY);
    seed_vectors(&source).await?;
    authorization(&pool).await?;
    copy(&pool, &source, Direction::ToPostgres, SOURCE_KEY).await?;
    let postgres = PostgresMemory::new(pool.clone()).with_summary_index_key(SOURCE_KEY);
    assert!(postgres.repair_input(RECORD).await?.is_none());
    assert_searchable(&postgres, "sion").await?;
    copy(&pool, &output, Direction::ToSqlite, SOURCE_KEY).await?;
    assert!(output.repair_input(RECORD).await?.is_none());
    assert_searchable(&output, "sion").await?;
    assert_eq!(sqlite_vectors(&output).await?, 2);
    let mismatch = store(&dir, "mismatch.sqlite")?.with_summary_index_key(OTHER_KEY);
    copy(&pool, &mismatch, Direction::ToSqlite, OTHER_KEY).await?;
    assert_eq!(sqlite_vectors(&mismatch).await?, 0);
    repair_and_search(&mismatch).await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn incompatible_sqlite_passages_are_repaired_after_hub_import(pool: PgPool) -> TestResult {
    let dir = tempfile::tempdir()?;
    let source = store(&dir, "source.sqlite")?;
    seed_vectors(&source).await?;
    authorization(&pool).await?;
    copy(&pool, &source, Direction::ToPostgres, OTHER_KEY).await?;
    let vectors: i64 =
        sqlx::query_scalar("SELECT count(*) FROM rh_summary_passages WHERE embedding IS NOT NULL")
            .fetch_one(&pool)
            .await?;
    assert_eq!(vectors, 0);
    repair_and_search(&PostgresMemory::new(pool).with_summary_index_key(OTHER_KEY)).await
}

async fn sqlite_vectors(local: &SqliteStore) -> Result<i64, SqliteError> {
    local
        .with_conn(|conn| {
            conn.query_row(
                "SELECT count(*) FROM ros_summary_passages WHERE embedding IS NOT NULL",
                [],
                |row| row.get(0),
            )
            .map_err(SqliteError::Pragma)
        })
        .await
}

async fn repair_and_search(store: &(impl SummaryStore + MemoryStore)) -> TestResult {
    let input = store.repair_input(RECORD).await?.ok_or("missing repair")?;
    let earliest = input.sources.first().ok_or("missing first source")?.1;
    let latest = input.sources.last().ok_or("missing last source")?.1;
    let chars = i32::try_from(input.content.chars().count())?;
    let vector = vec![0.25; SUMMARY_EMBEDDING_WIDTH];
    let index = SummaryIndex {
        passages: vec![SummaryPassage {
            content: input.content.clone(),
            chars: 0..chars,
            earliest_at: earliest,
            latest_at: latest,
            vector: vector.clone(),
        }],
        pooled: vector,
    };
    store.repair_index(input, index).await?;
    assert!(store.repair_input(RECORD).await?.is_none());
    assert_searchable(store, "Session").await
}

async fn assert_searchable(store: &impl MemoryStore, text: &str) -> TestResult {
    let result = store
        .search(SearchQuery {
            text: text.into(),
            mode: SearchMode::Fts,
            scope: MemoryScope::Summaries,
            agent: None,
            role: None,
            exclude_roles: Vec::new(),
            tool_name: None,
            tag: None,
            limit: Limit::clamped(Some(10), 10, 1..=50),
            since: None,
            before: None,
            window: None,
            expand: Some(false),
            synthesize: Some(false),
        })
        .await?;
    assert!(
        result
            .results
            .iter()
            .any(|row| row.id == RECORD.to_string() && row.passage.is_some()),
        "{result:?}"
    );
    Ok(())
}
