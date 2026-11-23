use crate::common;

use common::TestResult;
use common::fresh_store;
use kernel::memory::EmbedModelMismatch;
use kernel::memory::EmbedTable;
use kernel::memory::EmbedTarget;
use kernel::memory::EmbeddingStore;
use kernel::memory::EmbeddingStoreError;
use kernel::memory::LoadedTarget;
use kernel::memory::MemoryStore;
use kernel::memory::StoredChunk;
use kernel::memory::StoredEmbedding;
use kernel::memory::Unembeddable;
use rusqlite::OptionalExtension as _;
use rusqlite::params;

const CONVERSATION: &str = "00000000-0000-4000-8000-000000000002";
const DONE: &str = "00000000-0000-4000-8000-000000000101";
const ZERO: &str = "00000000-0000-4000-8000-000000000102";
const MEDIA: &str = "00000000-0000-4000-8000-000000000103";
const FAILED: &str = "00000000-0000-4000-8000-000000000104";
const WIDE: &str = "00000000-0000-4000-8000-000000000105";

#[tokio::test]
async fn embeddings_follow_the_recorded_row_effects() -> TestResult {
    let fresh = fresh_store()?;
    seed(&fresh.store).await?;
    let vector = sample();
    write_done(&fresh.store, &vector).await?;
    record_zero(&fresh.store).await?;
    mark_media_and_failed(&fresh.store).await?;
    keep_written_bytes(&fresh.store, &vector).await
}

async fn write_done(store: &memory_sqlite::SqliteStore, vector: &[f32]) -> TestResult {
    store
        .write(stored(DONE, vector.to_vec(), Vec::new()))
        .await?;
    expect_row(store, DONE, Some("done"), Some(32), None, 0).await?;
    assert_eq!(
        store.load(&target(EmbedTable::Messages, DONE)).await?,
        Some(kernel::memory::LoadedTarget {
            content: "deploy notes".to_owned(),
            tool_result: None,
        })
    );
    Ok(())
}

async fn record_zero(store: &memory_sqlite::SqliteStore) -> TestResult {
    let failures = store
        .record_null(
            &target(EmbedTable::Messages, ZERO),
            "embedding is a zero vector",
        )
        .await?;
    assert_eq!(failures, 1);
    expect_row(
        store,
        ZERO,
        None,
        None,
        Some("embedding is a zero vector"),
        1,
    )
    .await
}

async fn mark_media_and_failed(store: &memory_sqlite::SqliteStore) -> TestResult {
    store
        .mark_unembeddable(
            &target(EmbedTable::Messages, MEDIA),
            Unembeddable::Base64DataUrl,
        )
        .await?;
    expect_row(
        store,
        MEDIA,
        Some("unembeddable"),
        None,
        Some("unembeddable: base64-data-url"),
        0,
    )
    .await?;
    store
        .mark_permanent(&target(EmbedTable::Messages, FAILED), "embed failed")
        .await?;
    expect_row(store, FAILED, Some("failed"), None, Some("embed failed"), 0).await
}

async fn keep_written_bytes(store: &memory_sqlite::SqliteStore, vector: &[f32]) -> TestResult {
    let kept = store
        .record_null(
            &target(EmbedTable::Messages, DONE),
            "embedding is a zero vector",
        )
        .await?;
    assert_eq!(kept, 1);
    expect_row(
        store,
        DONE,
        Some("done"),
        Some(32),
        Some("embedding is a zero vector"),
        1,
    )
    .await?;
    let bytes = embedding_bytes(store, DONE).await?;
    assert_eq!(bytes.as_deref(), Some(pack(vector).as_slice()));
    Ok(())
}

#[tokio::test]
async fn width_mismatch_is_refused_and_chunks_are_stored() -> TestResult {
    let fresh = fresh_store()?;
    seed(&fresh.store).await?;
    fresh
        .store
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO ros_meta (key, value) VALUES ('embed_dims', '8')",
                [],
            )
            .map_err(common::seed)?;
            Ok(())
        })
        .await?;
    fresh
        .store
        .write(stored(WIDE, sample(), Vec::new()))
        .await?;
    let wide = fresh
        .store
        .write(stored(WIDE, vec![0.1; 10], Vec::new()))
        .await;
    match wide {
        Err(EmbeddingStoreError::Width {
            got: 10,
            expected: 8,
        }) => {}
        other => return Err(format!("width mismatch returned {other:?}").into()),
    }
    fresh
        .store
        .write(stored(DONE, sample(), vec![sample_chunk()]))
        .await?;
    let chunk_id = fresh
        .store
        .with_conn(|conn| {
            conn.query_row(
                "SELECT id FROM rh_message_chunks WHERE content = 'note'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map_err(common::seed)
        })
        .await?;
    assert_eq!(
        fresh
            .store
            .load(&target(EmbedTable::MessageChunks, &chunk_id))
            .await?,
        Some(LoadedTarget {
            content: "note".to_owned(),
            tool_result: None,
        })
    );
    let nested = fresh
        .store
        .write(StoredEmbedding {
            target: target(EmbedTable::MessageChunks, &chunk_id),
            vector: sample(),
            content_hash: "ignored".to_owned(),
            chunks: vec![sample_chunk()],
        })
        .await;
    match nested {
        Err(EmbeddingStoreError::Chunks {
            table: EmbedTable::MessageChunks,
        }) => {}
        other => return Err(format!("nested chunks returned {other:?}").into()),
    }
    assert_schema(&fresh.store).await
}

#[tokio::test]
async fn the_first_write_records_the_model_and_another_model_is_refused() -> TestResult {
    let fresh = fresh_store()?;
    seed(&fresh.store).await?;
    let first = fresh.store.clone().with_embed_model("embed-a");
    first.write(stored(DONE, sample(), Vec::new())).await?;
    assert_eq!(
        meta(&fresh.store, "embed_model").await?.as_deref(),
        Some("embed-a")
    );
    assert_eq!(
        meta(&fresh.store, "embed_dims").await?.as_deref(),
        Some("8")
    );
    let second = fresh.store.clone().with_embed_model("embed-b");
    match second.write(stored(WIDE, sample(), Vec::new())).await {
        Err(EmbeddingStoreError::Model { stored, configured })
            if stored == "embed-a" && configured == "embed-b" => {}
        other => return Err(format!("a second model returned {other:?}").into()),
    }
    expect_row(&fresh.store, WIDE, None, None, None, 0).await?;
    assert_eq!(
        second.embed_model_mismatch().await?,
        Some(EmbedModelMismatch {
            stored: "embed-a".to_owned(),
            configured: "embed-b".to_owned(),
        })
    );
    assert_eq!(first.embed_model_mismatch().await?, None);
    assert_eq!(fresh.store.embed_model_mismatch().await?, None);
    first.write(stored(WIDE, sample(), Vec::new())).await?;
    expect_row(&fresh.store, WIDE, Some("done"), Some(32), None, 0).await
}

async fn meta(store: &memory_sqlite::SqliteStore, key: &str) -> TestResult<Option<String>> {
    let key = key.to_owned();
    let value = store
        .with_conn(move |conn| {
            conn.query_row(
                "SELECT value FROM ros_meta WHERE key = ?1",
                params![key],
                |row| row.get(0),
            )
            .optional()
            .map_err(common::seed)
        })
        .await?;
    Ok(value)
}

async fn seed(store: &memory_sqlite::SqliteStore) -> TestResult {
    let messages = [
        (DONE, "deploy notes"),
        (ZERO, "zero"),
        (MEDIA, "data:image/png;base64,aaaa"),
        (FAILED, "permanent"),
        (WIDE, "wide"),
    ];
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_conversations \
                    (id, session_key, agent, channel, settings, active, created_at, updated_at) \
                 VALUES (?1, 'cli:embed', 'rivet', 'cli', '{}', 1, ?2, ?2)",
                params![CONVERSATION, common::RECORDED_START],
            )
            .map_err(common::seed)?;
            for (id, content) in messages {
                conn.execute(
                    "INSERT INTO ros_messages \
                        (id, conversation_id, agent, channel, role, content, metadata, created_at) \
                     VALUES (?1, ?2, 'rivet', 'cli', 'user', ?3, '{}', ?4)",
                    params![id, CONVERSATION, content, common::RECORDED_START],
                )
                .map_err(common::seed)?;
            }
            Ok(())
        })
        .await?;
    Ok(())
}

async fn expect_row(
    store: &memory_sqlite::SqliteStore,
    id: &str,
    status: Option<&str>,
    bytes: Option<usize>,
    error: Option<&str>,
    failures: i64,
) -> TestResult {
    let id = id.to_owned();
    let row = store
        .with_conn(move |conn| {
            conn.query_row(
                "SELECT embed_status, embedding, embed_error, embed_failures \
                   FROM ros_messages WHERE id = ?1",
                params![id],
                |row| {
                    let blob: Option<Vec<u8>> = row.get(1)?;
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        blob.map(|bytes| bytes.len()),
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .map_err(common::seed)
        })
        .await?;
    assert_eq!(row.0.as_deref(), status);
    assert_eq!(row.1, bytes);
    assert_eq!(row.2.as_deref(), error);
    assert_eq!(row.3, failures);
    Ok(())
}

async fn embedding_bytes(
    store: &memory_sqlite::SqliteStore,
    id: &str,
) -> TestResult<Option<Vec<u8>>> {
    let id = id.to_owned();
    let bytes = store
        .with_conn(move |conn| {
            conn.query_row(
                "SELECT embedding FROM ros_messages WHERE id = ?1",
                params![id],
                |row| row.get::<_, Option<Vec<u8>>>(0),
            )
            .map_err(common::seed)
        })
        .await?;
    Ok(bytes)
}

async fn assert_schema(store: &memory_sqlite::SqliteStore) -> TestResult {
    let found = store
        .with_conn(|conn| {
            let mut info = conn
                .prepare("PRAGMA table_info(ros_messages)")
                .map_err(common::seed)?;
            let names = info
                .query_map([], |row| row.get::<_, String>(1))
                .map_err(common::seed)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(common::seed)?;
            let legacy: Option<String> = conn
                .query_row(
                    "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'ros_message_chunks'",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .map_err(common::seed)?;
            let chunks: Option<String> = conn
                .query_row(
                    "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'rh_message_chunks'",
                    [],
                    |row| row.get(0),
                )
                .optional()
                .map_err(common::seed)?;
            Ok((names, legacy, chunks))
        })
        .await?;
    let hash: Vec<&String> = found
        .0
        .iter()
        .filter(|name| name.as_str() == "content_hash")
        .collect();
    assert_eq!(hash, Vec::<&String>::new());
    assert_eq!(found.1, None);
    assert_eq!(found.2.as_deref(), Some("rh_message_chunks"));
    Ok(())
}

fn stored(id: &str, vector: Vec<f32>, chunks: Vec<StoredChunk>) -> StoredEmbedding {
    StoredEmbedding {
        target: target(EmbedTable::Messages, id),
        vector,
        content_hash: "ignored".to_owned(),
        chunks,
    }
}

fn target(table: EmbedTable, id: &str) -> EmbedTarget {
    EmbedTarget {
        table,
        id: id.to_owned(),
    }
}

fn sample() -> Vec<f32> {
    vec![0.25, 0.5, 0.75, 1.0, -0.25, -0.5, -0.75, -1.0]
}

fn sample_chunk() -> StoredChunk {
    StoredChunk {
        index: 0,
        char_start: 0,
        char_end: 4,
        content: "note".to_owned(),
        vector: Some(sample()),
    }
}

fn pack(values: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for value in values {
        bytes.extend(value.to_le_bytes());
    }
    bytes
}
