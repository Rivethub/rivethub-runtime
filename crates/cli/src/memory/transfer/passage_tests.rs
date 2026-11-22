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

async fn seed_vectors(source: &SqliteStore, value: f32) -> TestResult {
    fixture(source).await?;
    let bytes: Vec<u8> = vec![value; SUMMARY_EMBEDDING_WIDTH]
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
    seed_vectors(&source, 0.25).await?;
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
    seed_vectors(&source, 0.25).await?;
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

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn carried_vectors_round_and_keep_destination_vectors(pool: PgPool) -> TestResult {
    let dir = tempfile::tempdir()?;
    let source = store(&dir, "original.sqlite")?;
    let restored = store(&dir, "restored.sqlite")?;
    fixture(&source).await?;
    authorization(&pool).await?;
    copy(&pool, &source, Direction::ToPostgres, SOURCE_KEY).await?;
    copy(&pool, &restored, Direction::ToSqlite, SOURCE_KEY).await?;
    let rounded = 0.099_975_586_f32.to_le_bytes();
    assert_vector_prefixes(&restored, rounded).await?;
    let owner = kernel::owner::Owner::new("alice".into())?;
    let token = tokio_util::sync::CancellationToken::new();
    for (local, direction) in [
        (&source, Direction::ToSqlite),
        (&source, Direction::ToPostgres),
        (&restored, Direction::ToSqlite),
        (&restored, Direction::ToPostgres),
    ] {
        let copied = run(
            &pool,
            local,
            [&owner, &owner],
            direction,
            (false, Some(SOURCE_KEY)),
            &token,
        )
        .await?;
        assert_eq!(copied.added.values().sum::<u64>(), 0);
    }
    assert_vector_prefixes(&source, 0.1_f32.to_le_bytes()).await?;
    assert_vector_prefixes(&restored, rounded).await?;
    assert_existing_and_missing_vectors(&pool, &source, &restored).await?;
    Ok(())
}

async fn assert_vector_prefixes(local: &SqliteStore, expected: [u8; 4]) -> TestResult {
    let prefixes = local
        .with_conn(|conn| {
            conn.prepare(
                "SELECT substr(embedding, 1, 4) FROM rh_entity_embeddings
                      UNION ALL SELECT substr(embedding, 1, 4) FROM ros_summary_passages",
            )
            .and_then(|mut stmt| {
                stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(SqliteError::Pragma)
        })
        .await?;
    assert_eq!(prefixes, vec![expected.to_vec(); 3]);
    Ok(())
}

async fn assert_existing_and_missing_vectors(
    pool: &PgPool,
    source: &SqliteStore,
    restored: &SqliteStore,
) -> TestResult {
    sqlx::raw_sql(
        "UPDATE rh_entity_embeddings SET embedding = array_fill(0.25::real, ARRAY[1024])::halfvec;
         UPDATE rh_summary_passages SET embedding = CASE ordinal
            WHEN 0 THEN array_fill(0.25::real, ARRAY[1024])::halfvec ELSE NULL END",
    )
    .execute(pool)
    .await?;
    copy(pool, source, Direction::ToPostgres, SOURCE_KEY).await?;
    let values: Vec<f32> = sqlx::query_scalar(
        "SELECT (embedding::vector::real[])[1] FROM rh_entity_embeddings
         UNION ALL SELECT (embedding::vector::real[])[1] FROM rh_summary_passages ORDER BY 1",
    )
    .fetch_all(pool)
    .await?;
    assert_eq!(
        values.into_iter().map(f32::to_bits).collect::<Vec<_>>(),
        [0.099_975_586_f32, 0.25, 0.25].map(f32::to_bits)
    );

    restored
        .with_conn(|conn| {
            conn.execute(
                "UPDATE ros_summary_passages SET embedding = NULL WHERE ordinal = 0",
                [],
            )
            .map_err(SqliteError::Pragma)
        })
        .await?;
    copy(pool, restored, Direction::ToSqlite, SOURCE_KEY).await?;
    let prefixes = restored
        .with_conn(|conn| {
            conn.prepare(
                "SELECT substr(embedding, 1, 4) FROM ros_summary_passages ORDER BY ordinal",
            )
            .and_then(|mut stmt| {
                stmt.query_map([], |row| row.get::<_, Vec<u8>>(0))?
                    .collect::<Result<Vec<_>, _>>()
            })
            .map_err(SqliteError::Pragma)
        })
        .await?;
    assert_eq!(
        prefixes,
        [0.25_f32.to_le_bytes(), 0.099_975_586_f32.to_le_bytes()]
    );
    Ok(())
}
