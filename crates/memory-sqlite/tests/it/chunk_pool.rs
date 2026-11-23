use std::sync::Arc;
use std::time::Instant;

use kernel::memory::Limit;
use kernel::memory::MemoryHitSource;
use kernel::memory::MemoryScope;
use kernel::memory::MemoryStore;
use kernel::memory::SearchMode;
use kernel::memory::SearchQuery;
use kernel::model::Embedding;
use memory_sqlite::QueryEmbed;
use memory_sqlite::SqliteError;
use rusqlite::Connection;
use testkit::FakeProvider;
use testkit::Script;
use tokio_util::sync::CancellationToken;

use crate::common::TestResult;
use crate::common::fresh_store;
use crate::common::seed;

const CHUNKED: u32 = 20_000;
const CLOSE: u32 = 17_000;
const SEARCH_BOUND_MS: u128 = 1_200;
const CLOSER_CHUNKS: u32 = 200;

#[tokio::test]
async fn twenty_thousand_chunked_messages_search_inside_the_bound() -> TestResult {
    let fresh = fresh_store()?;
    fresh.store.with_conn(plant).await?;
    let store = searching(fresh.store);
    let started = Instant::now();
    let outcome = store.search(vector_query()).await?;
    let elapsed = started.elapsed();
    assert!(
        elapsed.as_millis() <= SEARCH_BOUND_MS,
        "search took {elapsed:?}"
    );
    let top = outcome.results.first().cloned();
    let id = top.as_ref().map(|row| row.id.clone());
    let index = top.and_then(|row| row.chunk.map(|chunk| chunk.index));
    assert_eq!(id.as_deref(), Some(message_id(CLOSE).as_str()));
    assert_eq!(index, Some(0));
    Ok(())
}

#[tokio::test]
async fn a_tagged_chunk_survives_closer_untagged_chunks() -> TestResult {
    let fresh = fresh_store()?;
    fresh.store.with_conn(plant_tagged).await?;
    let store = searching(fresh.store);
    let mut query = vector_query();
    query.mode = SearchMode::Hybrid;
    query.tag = Some(kernel::memory::TagLiteral {
        key: "project".to_owned(),
        value: "orchard".to_owned(),
    });
    let outcome = store.search(query).await?;
    let ids: Vec<String> = outcome.results.into_iter().map(|hit| hit.id).collect();
    assert_eq!(ids, vec!["msg-tagged".to_owned()]);
    Ok(())
}

fn plant_tagged(conn: &mut Connection) -> Result<(), SqliteError> {
    conn.execute_batch("BEGIN IMMEDIATE").map_err(seed)?;
    insert_pair(conn, "conv-tagged", "session-tagged", "msg-tagged")?;
    insert_pair(conn, "conv-plain", "session-plain", "msg-plain")?;
    conn.execute(
        "INSERT INTO ros_tags \
            (id, entity_type, entity_id, key, value, source, state, created_at, updated_at) \
         VALUES ('tag-orchard', 'conversation', 'conv-tagged', 'project', 'orchard', \
                 'user', 'accepted', ?1, ?1)",
        ["2026-01-01T00:00:00.000Z"],
    )
    .map_err(seed)?;
    insert_span(conn, "chunk-tagged", "msg-tagged", 0, false)?;
    for index in 0..CLOSER_CHUNKS {
        insert_span(conn, &chunk_id(index), "msg-plain", index, true)?;
    }
    conn.execute_batch("COMMIT").map_err(seed)?;
    Ok(())
}

fn insert_pair(
    conn: &Connection,
    conversation: &str,
    session: &str,
    message: &str,
) -> Result<(), SqliteError> {
    conn.execute(
        "INSERT INTO ros_conversations (id, session_key, agent, channel, created_at, updated_at) \
         VALUES (?1, ?2, 'agent-pool', 'test', ?3, ?3)",
        rusqlite::params![conversation, session, "2026-01-01T00:00:00.000Z"],
    )
    .map_err(seed)?;
    conn.execute(
        "INSERT INTO ros_messages \
            (id, conversation_id, agent, channel, role, content, created_at) \
         VALUES (?1, ?2, 'agent-pool', 'test', 'user', ?3, ?4)",
        rusqlite::params![
            message,
            conversation,
            "the tagged note is long enough to be a real sentence here",
            "2026-01-01T00:00:00.000Z",
        ],
    )
    .map_err(seed)?;
    Ok(())
}

fn insert_span(
    conn: &Connection,
    id: &str,
    message: &str,
    index: u32,
    close: bool,
) -> Result<(), SqliteError> {
    conn.execute(
        "INSERT INTO rh_message_chunks \
            (id, message_id, idx, char_start, char_end, content, embedding, created_at) \
         VALUES (?1, ?2, ?3, 0, 4, 'note', ?4, ?5)",
        rusqlite::params![id, message, index, blob(close), "2026-01-01T00:00:00.000Z"],
    )
    .map_err(seed)?;
    Ok(())
}

#[tokio::test]
async fn a_summary_with_the_message_id_keeps_its_own_hit() -> TestResult {
    let fresh = fresh_store()?;
    fresh.store.with_conn(plant_shared_id).await?;
    let store = searching(fresh.store);
    let mut query = vector_query();
    query.scope = MemoryScope::Both;
    let outcome = store.search(query).await?;
    let kinds: Vec<(MemoryHitSource, bool)> = outcome
        .results
        .iter()
        .map(|hit| (hit.source, hit.chunk.is_some()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (MemoryHitSource::Message, true),
            (MemoryHitSource::Summary, false),
        ]
    );
    Ok(())
}

fn plant_shared_id(conn: &mut Connection) -> Result<(), SqliteError> {
    conn.execute_batch("BEGIN IMMEDIATE").map_err(seed)?;
    insert_pair(conn, "conv-shared", "session-shared", "same-id")?;
    conn.execute(
        "INSERT INTO ros_summaries (id, conversation_id, content, kind, created_at, embedding) \
         VALUES ('same-id', 'conv-shared', 'the summary stands beside the message', 'leaf', ?1, ?2)",
        rusqlite::params!["2026-01-01T00:00:00.000Z", blob(false)],
    )
    .map_err(seed)?;
    insert_span(conn, "chunk-shared", "same-id", 0, true)?;
    conn.execute_batch("COMMIT").map_err(seed)?;
    Ok(())
}

fn searching(store: memory_sqlite::SqliteStore) -> memory_sqlite::SqliteStore {
    let provider: Arc<dyn kernel::model::ModelProvider> =
        Arc::new(FakeProvider::new(vec![Script::Embed(vec![Embedding {
            vector: vec![1.0, 0.0],
        }])]));
    store.with_query_embed(QueryEmbed::new(
        provider,
        kernel::memory_core::DEFAULT_EMBED_QUERY_INSTRUCTION.to_owned(),
        std::time::Duration::from_secs(5),
        CancellationToken::new(),
    ))
}

fn vector_query() -> SearchQuery {
    SearchQuery {
        text: "pool".to_owned(),
        mode: SearchMode::Vector,
        scope: MemoryScope::Messages,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: Limit::clamped(Some(2), 10, 1..=50),
        since: None,
        before: None,
        window: None,
        expand: None,
        synthesize: None,
    }
}

fn plant(conn: &mut Connection) -> Result<(), SqliteError> {
    conn.execute_batch("BEGIN IMMEDIATE").map_err(seed)?;
    insert_conversation(conn)?;
    insert_chunked(conn)?;
    conn.execute_batch("COMMIT").map_err(seed)?;
    Ok(())
}

fn insert_conversation(conn: &Connection) -> Result<(), SqliteError> {
    conn.execute(
        "INSERT INTO ros_conversations (id, session_key, agent, channel, created_at, updated_at) \
         VALUES ('conv-pool', 'session-pool', 'agent-pool', 'test', ?1, ?1)",
        ["2026-01-01T00:00:00.000Z"],
    )
    .map_err(seed)?;
    Ok(())
}

fn insert_chunked(conn: &Connection) -> Result<(), SqliteError> {
    let mut message = conn
        .prepare(
            "INSERT INTO ros_messages \
                (id, conversation_id, agent, channel, role, content, created_at) \
             VALUES (?1, 'conv-pool', 'agent-pool', 'test', 'user', 'note', ?2)",
        )
        .map_err(seed)?;
    let mut chunk = conn
        .prepare(
            "INSERT INTO rh_message_chunks \
                (id, message_id, idx, char_start, char_end, content, embedding, created_at) \
             VALUES (?1, ?2, 0, 0, 4, 'note', ?3, ?4)",
        )
        .map_err(seed)?;
    for index in 0..CHUNKED {
        insert_one(&mut message, &mut chunk, index)?;
    }
    Ok(())
}

fn insert_one(
    message: &mut rusqlite::Statement<'_>,
    chunk: &mut rusqlite::Statement<'_>,
    index: u32,
) -> Result<(), SqliteError> {
    let id = message_id(index);
    message
        .execute(rusqlite::params![id, stamp(index)])
        .map_err(seed)?;
    chunk
        .execute(rusqlite::params![
            chunk_id(index),
            id,
            blob(index == CLOSE),
            stamp(index)
        ])
        .map_err(seed)?;
    Ok(())
}

fn message_id(index: u32) -> String {
    format!("{index:08x}-0000-4000-8000-000000000001")
}

fn chunk_id(index: u32) -> String {
    format!("{index:08x}-0000-4000-8000-000000000002")
}

fn stamp(index: u32) -> String {
    format!(
        "2026-01-01T{:02}:{:02}:{:02}.000Z",
        index / 3_600,
        (index / 60) % 60,
        index % 60
    )
}

fn blob(close: bool) -> Vec<u8> {
    let (left, right) = if close {
        (1.0_f32, 0.0_f32)
    } else {
        (0.0_f32, 1.0_f32)
    };
    let mut bytes = Vec::with_capacity(8);
    bytes.extend(left.to_le_bytes());
    bytes.extend(right.to_le_bytes());
    bytes
}
