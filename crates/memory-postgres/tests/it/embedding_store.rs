#![cfg(test)]

use kernel::memory::EmbedTable;
use kernel::memory::EmbedTarget;
use kernel::memory::EmbeddingStore;
use kernel::memory::EmbeddingStoreError;
use kernel::memory::StoredChunk;
use kernel::memory::StoredEmbedding;
use kernel::memory::Unembeddable;
use memory_postgres::PostgresMemory;
use pgvector::HalfVector;
use sqlx::PgPool;
use uuid::Uuid;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const WIDTH: usize = 1024;

const SUMMARY_STATUS: &str = "select embed_status, embed_error, embed_failures,
        (embedding = $2) as embedded, (embedding is null) as cleared
   from rh_summaries where id = $1";

const CHUNK_STATUS: &str = "select embed_status, embed_error, embed_failures,
        (embedding = $2) as embedded, (embedding is null) as cleared
   from rh_message_chunks where id = $1";

const WIKI_STATUS: &str = "select embed_status, embed_error, embed_failures,
        (embedding = $2) as embedded, (embedding is null) as cleared
   from rh_wiki_topics where slug = $1";

fn ones() -> Vec<f32> {
    vec![1.0; WIDTH]
}

fn half_ones() -> HalfVector {
    HalfVector::from_f32_slice(&ones())
}

fn message_target(id: &Uuid) -> EmbedTarget {
    EmbedTarget {
        table: EmbedTable::Messages,
        id: id.to_string(),
    }
}

fn stored(target: EmbedTarget, hash: &str, chunks: Vec<StoredChunk>) -> StoredEmbedding {
    StoredEmbedding {
        target,
        vector: ones(),
        content_hash: hash.to_owned(),
        chunks,
    }
}

fn chunk(index: u32, content: &str) -> StoredChunk {
    StoredChunk {
        index,
        char_start: index,
        char_end: index + 1,
        content: content.to_owned(),
        vector: Some(ones()),
    }
}

async fn begin_deferred(
    pool: &PgPool,
) -> Result<sqlx::Transaction<'static, sqlx::Postgres>, sqlx::Error> {
    let mut tx = pool.begin().await?;
    sqlx::query("select set_config('rivet.defer_embed_enqueue', 'on', true)")
        .execute(&mut *tx)
        .await?;
    Ok(tx)
}

async fn insert_message(
    pool: &PgPool,
    index: usize,
    content: &str,
    tool_result: Option<&str>,
) -> Result<Uuid, sqlx::Error> {
    let mut tx = begin_deferred(pool).await?;
    let conversation: Uuid = sqlx::query_scalar(
        "insert into rh_conversations (session_key, agent, owner_user_id) values ($1, 'rivet', 'owner') returning id",
    )
    .bind(format!("embed-store-{index}"))
    .fetch_one(&mut *tx)
    .await?;
    let id: Uuid = sqlx::query_scalar(
        "insert into rh_messages (conversation_id, agent, channel, role, content, tool_result, owner_user_id)
         values ($1, 'rivet', 'test', 'user', $2, $3, 'owner')
         returning id",
    )
    .bind(conversation)
    .bind(content)
    .bind(tool_result)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

async fn insert_summary(pool: &PgPool, content: &str) -> Result<Uuid, sqlx::Error> {
    let mut tx = begin_deferred(pool).await?;
    let id: Uuid = sqlx::query_scalar(
        "insert into rh_summaries (content, owner_user_id) values ($1, 'owner') returning id",
    )
    .bind(content)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(id)
}

async fn insert_chunk(pool: &PgPool, content: &str) -> Result<Uuid, sqlx::Error> {
    let message = insert_message(pool, 10_000, "chunk parent", None).await?;
    let id: Uuid = sqlx::query_scalar(
        "insert into rh_message_chunks
            (message_id, idx, char_start, char_end, content, embed_status, owner_user_id)
         values ($1, 0, 0, 1, $2, 'pending', 'owner')
         returning id",
    )
    .bind(message)
    .bind(content)
    .fetch_one(pool)
    .await?;
    Ok(id)
}

async fn insert_wiki(pool: &PgPool, slug: &str, search_text: &str) -> Result<(), sqlx::Error> {
    sqlx::query("insert into rh_wiki_topics (slug, title, search_text) values ($1, $2, $3)")
        .bind(slug)
        .bind(slug)
        .bind(search_text)
        .execute(pool)
        .await?;
    Ok(())
}

#[derive(Debug, sqlx::FromRow)]
struct MessageLedger {
    content_hash: Option<String>,
    embed_status: Option<String>,
    embed_error: Option<String>,
    embed_failures: Option<i32>,
    embedded: Option<bool>,
    cleared: Option<bool>,
}

async fn message_ledger(pool: &PgPool, id: Uuid) -> Result<MessageLedger, sqlx::Error> {
    sqlx::query_as(
        "select content_hash, embed_status, embed_error, embed_failures,
                (embedding = $2) as embedded,
                (embedding is null) as cleared
           from rh_messages where id = $1",
    )
    .bind(id)
    .bind(half_ones())
    .fetch_one(pool)
    .await
}

#[derive(Debug, sqlx::FromRow)]
struct StatusRow {
    embed_status: Option<String>,
    embed_error: Option<String>,
    embed_failures: Option<i32>,
    embedded: Option<bool>,
    cleared: Option<bool>,
}

async fn status_uuid(pool: &PgPool, sql: &'static str, id: Uuid) -> Result<StatusRow, sqlx::Error> {
    sqlx::query_as(sql)
        .bind(id)
        .bind(half_ones())
        .fetch_one(pool)
        .await
}

async fn status_slug(pool: &PgPool, slug: &str) -> Result<StatusRow, sqlx::Error> {
    sqlx::query_as(WIKI_STATUS)
        .bind(slug)
        .bind(half_ones())
        .fetch_one(pool)
        .await
}

#[derive(Debug, PartialEq, Eq, sqlx::FromRow)]
struct ChunkText {
    idx: i32,
    content: String,
}

async fn chunk_texts(pool: &PgPool, message: Uuid) -> Result<Vec<ChunkText>, sqlx::Error> {
    sqlx::query_as("select idx, content from rh_message_chunks where message_id = $1 order by idx")
        .bind(message)
        .fetch_all(pool)
        .await
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn message_jobs_round_trip(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    for (index, (content, tool_result)) in [("plain text", None), ("", Some("tool output"))]
        .into_iter()
        .enumerate()
    {
        let id = insert_message(&pool, index, content, tool_result).await?;
        let target = message_target(&id);
        let loaded = EmbeddingStore::load(&memory, &target)
            .await?
            .expect("stored row");
        assert_eq!(loaded.content, content);
        assert_eq!(loaded.tool_result.as_deref(), tool_result);
        EmbeddingStore::write(&memory, stored(target, "hash", Vec::new())).await?;
        let ledger = message_ledger(&pool, id).await?;
        assert_eq!(ledger.content_hash.as_deref(), Some("hash"));
        assert_eq!(ledger.embed_status, None);
        assert_eq!(ledger.embed_error, None);
        assert_eq!(ledger.embed_failures, Some(0));
        assert_eq!(ledger.embedded, Some(true));
        assert_eq!(ledger.cleared, Some(false));
    }
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn summary_chunk_and_wiki_round_trip(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    check_summary(&pool, &memory).await?;
    check_chunk(&pool, &memory).await?;
    check_wiki(&pool, &memory).await?;
    Ok(())
}

async fn check_summary(pool: &PgPool, memory: &PostgresMemory) -> TestResult {
    let id = insert_summary(pool, "summary body").await?;
    let target = EmbedTarget {
        table: EmbedTable::Summaries,
        id: id.to_string(),
    };
    let loaded = EmbeddingStore::load(memory, &target)
        .await?
        .expect("summary");
    assert_eq!(loaded.content, "summary body");
    assert_eq!(loaded.tool_result, None);
    EmbeddingStore::write(memory, stored(target.clone(), "unused", Vec::new())).await?;
    settle(
        memory,
        &target,
        status_uuid(pool, SUMMARY_STATUS, id).await?,
        || status_uuid(pool, SUMMARY_STATUS, id),
    )
    .await?;
    Ok(())
}

async fn check_chunk(pool: &PgPool, memory: &PostgresMemory) -> TestResult {
    let id = insert_chunk(pool, "chunk body").await?;
    let target = EmbedTarget {
        table: EmbedTable::MessageChunks,
        id: id.to_string(),
    };
    let loaded = EmbeddingStore::load(memory, &target).await?.expect("chunk");
    assert_eq!(loaded.content, "chunk body");
    assert_eq!(loaded.tool_result, None);
    EmbeddingStore::write(memory, stored(target.clone(), "unused", Vec::new())).await?;
    settle(
        memory,
        &target,
        status_uuid(pool, CHUNK_STATUS, id).await?,
        || status_uuid(pool, CHUNK_STATUS, id),
    )
    .await?;
    Ok(())
}

async fn check_wiki(pool: &PgPool, memory: &PostgresMemory) -> TestResult {
    let slug = "embed-topic";
    insert_wiki(pool, slug, "searchable body").await?;
    let target = EmbedTarget {
        table: EmbedTable::WikiTopics,
        id: slug.to_owned(),
    };
    let loaded = EmbeddingStore::load(memory, &target).await?.expect("wiki");
    assert_eq!(loaded.content, "searchable body");
    assert_eq!(loaded.tool_result, None);
    let absent = EmbedTarget {
        table: EmbedTable::WikiTopics,
        id: "missing-topic".to_owned(),
    };
    assert!(EmbeddingStore::load(memory, &absent).await?.is_none());
    EmbeddingStore::write(memory, stored(target.clone(), "unused", Vec::new())).await?;
    settle(memory, &target, status_slug(pool, slug).await?, || {
        status_slug(pool, slug)
    })
    .await?;
    Ok(())
}

fn assert_clean(row: &StatusRow) {
    assert_eq!(row.embedded, Some(true));
    assert_eq!(row.cleared, Some(false));
    assert_eq!(row.embed_status, None);
    assert_eq!(row.embed_error, None);
    assert_eq!(row.embed_failures, Some(0));
}

fn assert_null_row(row: &StatusRow) {
    assert_eq!(row.embed_failures, Some(1));
    assert_eq!(row.embed_error.as_deref(), Some("Embedding returned null"));
    assert_eq!(row.embed_status, None);
    assert_eq!(row.embedded, Some(true));
    assert_eq!(row.cleared, Some(false));
}

fn assert_permanent(row: &StatusRow) {
    assert_eq!(row.embed_status.as_deref(), Some("failed"));
    assert_eq!(
        row.embed_error.as_deref(),
        Some("Embedding returned null (permanent)")
    );
    assert_eq!(row.cleared, Some(true));
}

async fn settle<F, Fut>(
    memory: &PostgresMemory,
    target: &EmbedTarget,
    written: StatusRow,
    mut read: F,
) -> TestResult
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<StatusRow, sqlx::Error>>,
{
    assert_clean(&written);
    let count = EmbeddingStore::record_null(memory, target, "Embedding returned null").await?;
    assert_eq!(count, 1);
    assert_null_row(&read().await?);
    EmbeddingStore::mark_permanent(memory, target, "Embedding returned null (permanent)").await?;
    assert_permanent(&read().await?);
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn null_then_permanent_keeps_the_ledger(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let id = insert_message(&pool, 1, "hello ledger", None).await?;
    let target = message_target(&id);
    EmbeddingStore::write(&memory, stored(target.clone(), "first", Vec::new())).await?;
    let count = EmbeddingStore::record_null(&memory, &target, "Embedding returned null").await?;
    assert_eq!(count, 1);
    let ledger = message_ledger(&pool, id).await?;
    assert_eq!(ledger.embed_failures, Some(1));
    assert_eq!(
        ledger.embed_error.as_deref(),
        Some("Embedding returned null")
    );
    assert_eq!(ledger.embed_status, None);
    assert_eq!(ledger.embedded, Some(true));
    EmbeddingStore::mark_permanent(&memory, &target, "Embedding returned null (permanent)").await?;
    let ledger = message_ledger(&pool, id).await?;
    assert_eq!(ledger.embed_status.as_deref(), Some("failed"));
    assert_eq!(
        ledger.embed_error.as_deref(),
        Some("Embedding returned null (permanent)")
    );
    assert_eq!(ledger.cleared, Some(true));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn message_chunks_replace_on_rewrite(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let id = insert_message(&pool, 2, "chunked", None).await?;
    let target = message_target(&id);
    EmbeddingStore::write(
        &memory,
        stored(
            target.clone(),
            "first",
            vec![chunk(0, "one"), chunk(1, "two")],
        ),
    )
    .await?;
    assert_eq!(
        chunk_texts(&pool, id).await?,
        vec![
            ChunkText {
                idx: 0,
                content: "one".to_owned(),
            },
            ChunkText {
                idx: 1,
                content: "two".to_owned(),
            },
        ]
    );
    EmbeddingStore::write(&memory, stored(target, "second", Vec::new())).await?;
    assert_eq!(chunk_texts(&pool, id).await?, Vec::<ChunkText>::new());
    let ledger = message_ledger(&pool, id).await?;
    assert_eq!(ledger.content_hash.as_deref(), Some("second"));
    assert_eq!(ledger.embedded, Some(true));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn unembeddable_clears_the_vector(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let id = insert_message(&pool, 3, "media", None).await?;
    let target = message_target(&id);
    EmbeddingStore::write(&memory, stored(target.clone(), "hash", Vec::new())).await?;
    EmbeddingStore::mark_unembeddable(&memory, &target, Unembeddable::MediaMarker).await?;
    let ledger = message_ledger(&pool, id).await?;
    assert_eq!(ledger.embed_status.as_deref(), Some("unembeddable"));
    assert_eq!(
        ledger.embed_error.as_deref(),
        Some(Unembeddable::MediaMarker.error_text().as_str())
    );
    assert_eq!(ledger.cleared, Some(true));
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn refuses_a_bad_id_a_narrow_vector_and_stray_chunks(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let bogus = EmbedTarget {
        table: EmbedTable::Messages,
        id: "msg-1".to_owned(),
    };
    assert!(matches!(
        EmbeddingStore::load(&memory, &bogus).await,
        Err(EmbeddingStoreError::TargetId { .. })
    ));
    let missing = message_target(&Uuid::new_v4());
    assert!(EmbeddingStore::load(&memory, &missing).await?.is_none());
    assert!(matches!(
        EmbeddingStore::write(&memory, stored(missing, "hash", Vec::new())).await,
        Err(EmbeddingStoreError::Missing { .. })
    ));
    let id = insert_message(&pool, 4, "width", None).await?;
    let narrow = StoredEmbedding {
        target: message_target(&id),
        vector: vec![1.0; 8],
        content_hash: "hash".to_owned(),
        chunks: Vec::new(),
    };
    assert!(matches!(
        EmbeddingStore::write(&memory, narrow).await,
        Err(EmbeddingStoreError::Width {
            got: 8,
            expected: WIDTH
        })
    ));
    let summary = insert_summary(&pool, "a summary").await?;
    let stray = stored(
        EmbedTarget {
            table: EmbedTable::Summaries,
            id: summary.to_string(),
        },
        "hash",
        vec![chunk(0, "piece")],
    );
    assert!(matches!(
        EmbeddingStore::write(&memory, stray).await,
        Err(EmbeddingStoreError::Chunks {
            table: EmbedTable::Summaries
        })
    ));
    Ok(())
}
