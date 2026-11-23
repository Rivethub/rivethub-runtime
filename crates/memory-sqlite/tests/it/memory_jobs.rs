use std::sync::Arc;
use std::time::Duration;

use kernel::model::Embedding;
use memory_sqlite::QueryEmbed;
use memory_sqlite::SqliteStore;
use rusqlite::params;
use testkit::FakeProvider;
use testkit::Script;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::common::TestResult;
use crate::common::fresh_store;
use crate::common::seed;

#[tokio::test]
async fn a_phrase_inside_a_long_message_outranks_its_parent() -> TestResult {
    let fresh = fresh_store()?;
    let provider: Arc<dyn kernel::model::ModelProvider> =
        Arc::new(FakeProvider::new(vec![Script::Embed(vec![Embedding {
            vector: testkit::memory_jobs::query_vector(),
        }])]));
    let store = fresh.store.clone().with_query_embed(QueryEmbed::new(
        provider,
        kernel::memory_core::DEFAULT_EMBED_QUERY_INSTRUCTION.to_owned(),
        Duration::from_secs(5),
        CancellationToken::new(),
    ));
    testkit::memory_jobs::a_phrase_inside_a_long_message_outranks_its_parent(&store).await
}

#[tokio::test]
async fn the_nearest_chunk_pool_keeps_one_chunk_per_message() -> TestResult {
    let fresh = fresh_store()?;
    let provider: Arc<dyn kernel::model::ModelProvider> = Arc::new(FakeProvider::new(vec![
        Script::Embed(vec![Embedding {
            vector: testkit::memory_jobs::query_vector(),
        }]),
        Script::Embed(vec![Embedding {
            vector: testkit::memory_jobs::query_vector(),
        }]),
    ]));
    let store = fresh.store.clone().with_query_embed(QueryEmbed::new(
        provider,
        kernel::memory_core::DEFAULT_EMBED_QUERY_INSTRUCTION.to_owned(),
        Duration::from_secs(5),
        CancellationToken::new(),
    ));
    testkit::memory_jobs::the_nearest_chunk_pool_keeps_one_chunk_per_message(&store).await
}

#[tokio::test]
async fn a_chunk_write_rejects_a_vector_wider_than_the_message() -> TestResult {
    let fresh = fresh_store()?;
    testkit::memory_jobs::a_chunk_write_rejects_a_vector_wider_than_the_message(&fresh.store).await
}

#[tokio::test]
async fn a_chunk_embedding_write_rejects_a_vector_wider_than_the_message() -> TestResult {
    let fresh = fresh_store()?;
    let message = Uuid::new_v4().to_string();
    let chunk = Uuid::new_v4().to_string();
    let parent = testkit::memory_jobs::query_vector();
    fresh
        .store
        .with_conn({
            let message = message.clone();
            let chunk = chunk.clone();
            let parent = parent.clone();
            move |conn| plant_parent_vector(conn, &message, &chunk, &parent)
        })
        .await?;
    let refused = kernel::memory::EmbeddingStore::write(
        &fresh.store,
        kernel::memory::StoredEmbedding {
            target: kernel::memory::EmbedTarget {
                table: kernel::memory::EmbedTable::MessageChunks,
                id: chunk.clone(),
            },
            vector: vec![0.5, 0.0, 0.25],
            content_hash: "test".to_owned(),
            chunks: Vec::new(),
        },
    )
    .await;
    match refused {
        Err(kernel::memory::EmbeddingStoreError::Width { got, expected }) => {
            assert_eq!((got, expected), (3, parent.len()));
        }
        other => return Err(format!("chunk embedding write returned {other:?}").into()),
    }
    let stored = fresh
        .store
        .with_conn(move |conn| {
            let held: i64 = conn
                .query_row(
                    "SELECT count(*) FROM rh_message_chunks WHERE id = ?1 AND embedding IS NOT NULL",
                    [chunk],
                    |row| row.get(0),
                )
                .map_err(seed)?;
            Ok(held)
        })
        .await?;
    assert_eq!(stored, 0);
    Ok(())
}

fn plant_parent_vector(
    conn: &rusqlite::Connection,
    message: &str,
    chunk: &str,
    parent: &[f32],
) -> Result<(), memory_sqlite::SqliteError> {
    let mut bytes = Vec::new();
    for value in parent {
        bytes.extend(value.to_le_bytes());
    }
    conn.execute(
        "INSERT INTO ros_conversations (id, session_key, agent, channel, created_at, updated_at) \
         VALUES ('conv-width', 'session-width', 'agent', 'test', ?1, ?1)",
        ["2026-01-01T00:00:00.000Z"],
    )
    .map_err(seed)?;
    conn.execute(
        "INSERT INTO ros_messages \
            (id, conversation_id, agent, channel, role, content, created_at, embedding) \
         VALUES (?1, 'conv-width', 'agent', 'test', 'user', 'width body', ?2, ?3)",
        params![message, "2026-01-01T00:00:00.000Z", bytes],
    )
    .map_err(seed)?;
    conn.execute(
        "INSERT INTO rh_message_chunks \
            (id, message_id, idx, char_start, char_end, content, created_at) \
         VALUES (?1, ?2, 0, 0, 1, 'x', ?3)",
        params![chunk, message, "2026-01-01T00:00:00.000Z"],
    )
    .map_err(seed)?;
    Ok(())
}

#[tokio::test]
async fn unchunked_counts_trimmed_characters_and_caps_tool_output() -> TestResult {
    let fresh = fresh_store()?;
    testkit::memory_jobs::unchunked_counts_trimmed_characters_and_caps_tool_output(&fresh.store)
        .await
}

#[tokio::test]
async fn chunks_are_written_once_and_an_unknown_message_is_skipped() -> TestResult {
    let fresh = fresh_store()?;
    testkit::memory_jobs::chunks_are_written_once_and_an_unknown_message_is_skipped(&fresh.store)
        .await
}

#[tokio::test]
async fn a_tool_sentence_replaces_only_matching_content() -> TestResult {
    let fresh = fresh_store()?;
    testkit::memory_jobs::a_tool_sentence_replaces_only_matching_content(&fresh.store).await
}

#[tokio::test]
async fn encoded_tool_arguments_round_trip_and_reject_bad_json() -> TestResult {
    let fresh = fresh_store()?;
    let id = testkit::memory_jobs::remember_tool_call(&fresh.store).await?;
    plant(&fresh.store, id, r#"{"path":"notes.txt"}"#).await?;
    testkit::memory_jobs::tool_arguments_match(&fresh.store, id, "notes.txt").await?;
    plant(&fresh.store, id, "not-json").await?;
    testkit::memory_jobs::tool_arguments_are_rejected(&fresh.store, id).await
}

#[tokio::test]
async fn an_unembedded_chunk_is_not_healed_and_clears_once_written() -> TestResult {
    let fresh = fresh_store()?;
    testkit::memory_jobs::an_unembedded_chunk_is_not_healed_and_clears_once_written(&fresh.store)
        .await
}

#[tokio::test]
async fn a_non_heartbeat_leaf_is_the_only_missing_wiki() -> TestResult {
    let fresh = fresh_store()?;
    testkit::memory_jobs::a_non_heartbeat_leaf_is_the_only_missing_wiki(&fresh.store, &fresh.store)
        .await
}

#[tokio::test]
async fn a_fresh_dead_wiki_job_is_not_revived() -> TestResult {
    let fresh = fresh_store()?;
    testkit::memory_jobs::a_fresh_dead_wiki_job_is_not_revived(&fresh.store, &fresh.store).await
}

#[tokio::test]
async fn an_empty_assistant_tool_call_is_backfilled_once() -> TestResult {
    let fresh = fresh_store()?;
    let jobs = fresh.store.clone();
    testkit::memory_jobs::an_empty_assistant_tool_call_is_backfilled_once(
        &fresh.store,
        &fresh.store,
        async move |limit, dry| {
            jobs.backfill_tool_synth(limit, dry)
                .await
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
        },
    )
    .await
}

#[tokio::test]
async fn an_exhausted_wiki_extraction_stays_suppressed() -> TestResult {
    let fresh = fresh_store()?;
    testkit::memory_jobs::an_exhausted_wiki_extraction_stays_suppressed(&fresh.store, &fresh.store)
        .await
}

#[tokio::test]
async fn an_exhausted_tool_synthesis_is_backfilled_again() -> TestResult {
    let fresh = fresh_store()?;
    let jobs = fresh.store.clone();
    testkit::memory_jobs::an_exhausted_tool_synthesis_is_backfilled_again(
        &fresh.store,
        &fresh.store,
        async move |limit, dry| {
            jobs.backfill_tool_synth(limit, dry)
                .await
                .map_err(|error| Box::new(error) as Box<dyn std::error::Error + Send + Sync>)
        },
    )
    .await
}

#[tokio::test]
async fn a_chunk_write_enqueues_its_embedding() -> TestResult {
    let fresh = fresh_store()?;
    testkit::memory_jobs::a_chunk_write_enqueues_its_embedding(&fresh.store, &fresh.store).await
}

async fn plant(store: &SqliteStore, id: Uuid, args: &str) -> TestResult {
    let id = id.to_string();
    let args = serde_json::to_string(args)?;
    store
        .with_conn(move |conn| {
            conn.execute(
                "UPDATE ros_messages SET tool_args = ?2 WHERE id = ?1",
                params![id, args],
            )
            .map(|_| ())
            .map_err(seed)
        })
        .await?;
    Ok(())
}
