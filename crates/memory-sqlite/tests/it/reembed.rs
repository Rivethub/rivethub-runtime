use crate::common;

use common::TestResult;
use common::fresh_store;
use kernel::memory::EmbedTable;
use kernel::memory::EmbedTarget;
use kernel::memory::EmbeddingStore;
use kernel::memory::MemoryStore;
use kernel::memory::StoredEmbedding;
use kernel::memory::Unembeddable;
use memory_sqlite::ReembedReport;
use memory_sqlite::SqliteStore;
use rusqlite::params;

const CONVERSATION: &str = "00000000-0000-4000-8000-000000000002";
const EMBEDDED: &str = "00000000-0000-4000-8000-000000000201";
const MEDIA: &str = "00000000-0000-4000-8000-000000000202";
const DEAD: &str = "00000000-0000-4000-8000-000000000203";

#[tokio::test]
async fn reembed_clears_vectors_requeues_rows_and_forgets_the_model() -> TestResult {
    let fresh = fresh_store()?;
    seed(&fresh.store).await?;
    let old = fresh.store.clone().with_embed_model("embed-a");
    old.write(stored(EMBEDDED)).await?;
    old.mark_unembeddable(&target(MEDIA), Unembeddable::Base64DataUrl)
        .await?;
    let expected = ReembedReport {
        cleared: 1,
        queued: 2,
    };
    assert_eq!(fresh.store.reembed(false).await?, expected);
    assert_eq!(state(&fresh.store).await?, before());
    assert_eq!(fresh.store.reembed(true).await?, expected);
    assert_eq!(state(&fresh.store).await?, after());
    let new = fresh.store.clone().with_embed_model("embed-b");
    new.write(stored(EMBEDDED)).await?;
    let models = scalar_text(
        &fresh.store,
        "SELECT value FROM ros_meta WHERE key = 'embed_model'",
    )
    .await?;
    assert_eq!(models, "embed-b");
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct State {
    vectors: i64,
    media: String,
    queued: Vec<String>,
    dead: i64,
    meta: i64,
}

fn before() -> State {
    State {
        vectors: 1,
        media: "unembeddable".to_owned(),
        queued: Vec::new(),
        dead: 1,
        meta: 2,
    }
}

fn after() -> State {
    State {
        vectors: 0,
        media: "unembeddable".to_owned(),
        queued: vec![
            format!("embed-ros_messages-{EMBEDDED}"),
            format!("embed-ros_messages-{DEAD}"),
        ],
        dead: 0,
        meta: 0,
    }
}

async fn state(store: &SqliteStore) -> TestResult<State> {
    let found = store
        .with_conn(|conn| {
            let count = |sql: &str| conn.query_row(sql, [], |row| row.get::<_, i64>(0));
            let mut keys = conn
                .prepare(
                    "SELECT job_key FROM ros_jobs WHERE task = 'embed-target' AND state = 'queued' \
                     ORDER BY job_key",
                )
                .map_err(common::seed)?;
            let mut queued = keys
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(common::seed)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(common::seed)?;
            queued.sort();
            Ok(State {
                vectors: count("SELECT count(*) FROM ros_messages WHERE embedding IS NOT NULL")
                    .map_err(common::seed)?,
                media: conn
                    .query_row(
                        "SELECT embed_status FROM ros_messages WHERE id = ?1",
                        params![MEDIA],
                        |row| row.get(0),
                    )
                    .map_err(common::seed)?,
                queued,
                dead: count("SELECT count(*) FROM ros_jobs WHERE state = 'dead'")
                    .map_err(common::seed)?,
                meta: count(
                    "SELECT count(*) FROM ros_meta WHERE key IN ('embed_model', 'embed_dims')",
                )
                .map_err(common::seed)?,
            })
        })
        .await?;
    Ok(found)
}

async fn scalar_text(store: &SqliteStore, sql: &str) -> TestResult<String> {
    let sql = sql.to_owned();
    let value = store
        .with_conn(move |conn| {
            conn.query_row(&sql, [], |row| row.get(0))
                .map_err(common::seed)
        })
        .await?;
    Ok(value)
}

async fn seed(store: &SqliteStore) -> TestResult {
    store
        .with_conn(move |conn| {
            conn.execute(
                "INSERT INTO ros_conversations \
                    (id, session_key, agent, channel, settings, active, created_at, updated_at) \
                 VALUES (?1, 'cli:reembed', 'rivet', 'cli', '{}', 1, ?2, ?2)",
                params![CONVERSATION, common::RECORDED_START],
            )
            .map_err(common::seed)?;
            for (id, content) in [
                (EMBEDDED, "deploy notes"),
                (MEDIA, "data:image/png;base64,aaaa"),
                (DEAD, "never embedded"),
            ] {
                conn.execute(
                    "INSERT INTO ros_messages \
                        (id, conversation_id, agent, channel, role, content, metadata, created_at) \
                     VALUES (?1, ?2, 'rivet', 'cli', 'user', ?3, '{}', ?4)",
                    params![id, CONVERSATION, content, common::RECORDED_START],
                )
                .map_err(common::seed)?;
            }
            conn.execute(
                "INSERT INTO ros_jobs \
                    (id, task, job_key, payload, run_at, attempts, max_attempts, state, created_at, updated_at) \
                 VALUES ('dead-job', 'embed-target', ?1, '{}', ?2, 5, 5, 'dead', ?2, ?2)",
                params![format!("embed-ros_messages-{DEAD}"), common::RECORDED_START],
            )
            .map_err(common::seed)?;
            Ok(())
        })
        .await?;
    Ok(())
}

#[tokio::test]
async fn reembed_drops_chunk_vectors_so_a_new_width_can_search() -> TestResult {
    let fresh = fresh_store()?;
    seed(&fresh.store).await?;
    let old = fresh.store.clone().with_embed_model("embed-a");
    old.write(chunked(EMBEDDED)).await?;
    fresh.store.reembed(true).await?;
    let chunk = chunk_id(&fresh.store).await?;
    assert_eq!(
        scalar_text(
            &fresh.store,
            "SELECT CAST(count(*) AS TEXT) FROM rh_message_chunks WHERE embedding IS NOT NULL",
        )
        .await?,
        "0"
    );
    let queued = scalar_text(
        &fresh.store,
        &format!(
            "SELECT CAST(count(*) AS TEXT) FROM ros_jobs WHERE job_key = 'embed-ros_message_chunks-{chunk}'"
        ),
    )
    .await?;
    assert_eq!(queued, "1");
    let provider: std::sync::Arc<dyn kernel::model::ModelProvider> =
        std::sync::Arc::new(testkit::FakeProvider::new(vec![testkit::Script::Embed(
            vec![kernel::model::Embedding {
                vector: testkit::memory_jobs::query_vector(),
            }],
        )]));
    let store = fresh
        .store
        .clone()
        .with_query_embed(memory_sqlite::QueryEmbed::new(
            provider,
            kernel::memory_core::DEFAULT_EMBED_QUERY_INSTRUCTION.to_owned(),
            std::time::Duration::from_secs(5),
            tokio_util::sync::CancellationToken::new(),
        ));
    let outcome = store
        .search(kernel::memory::SearchQuery {
            text: "zzzzqqqx".to_owned(),
            mode: kernel::memory::SearchMode::Vector,
            scope: kernel::memory::MemoryScope::Messages,
            agent: None,
            role: None,
            exclude_roles: Vec::new(),
            tool_name: None,
            tag: None,
            limit: kernel::memory::Limit::clamped(Some(10), 10, 1..=50),
            since: None,
            before: None,
            window: None,
            expand: None,
            synthesize: None,
        })
        .await?;
    assert_eq!(outcome.degraded, None);
    assert_eq!(outcome.results, Vec::new());
    Ok(())
}

async fn chunk_id(store: &SqliteStore) -> TestResult<String> {
    scalar_text(
        store,
        &format!("SELECT id FROM rh_message_chunks WHERE message_id = '{EMBEDDED}'"),
    )
    .await
}

#[tokio::test]
async fn reembed_clears_an_unembeddable_chunk_vector_without_requeueing_it() -> TestResult {
    let fresh = fresh_store()?;
    seed(&fresh.store).await?;
    let old = fresh.store.clone().with_embed_model("embed-a");
    old.write(chunked(EMBEDDED)).await?;
    let chunk = chunk_id(&fresh.store).await?;
    common::execute(
        &fresh.store,
        "UPDATE rh_message_chunks SET embed_status = 'unembeddable' WHERE embedding IS NOT NULL",
    )
    .await?;
    fresh.store.reembed(true).await?;
    assert_eq!(
        scalar_text(
            &fresh.store,
            "SELECT CAST(count(*) AS TEXT) FROM rh_message_chunks WHERE embedding IS NOT NULL",
        )
        .await?,
        "0"
    );
    assert_eq!(
        scalar_text(
            &fresh.store,
            &format!(
                "SELECT CAST(count(*) AS TEXT) FROM ros_jobs WHERE job_key = 'embed-ros_message_chunks-{chunk}'"
            ),
        )
        .await?,
        "0"
    );
    let provider: std::sync::Arc<dyn kernel::model::ModelProvider> =
        std::sync::Arc::new(testkit::FakeProvider::new(vec![testkit::Script::Embed(
            vec![kernel::model::Embedding {
                vector: vec![0.6, 0.8],
            }],
        )]));
    let store = fresh
        .store
        .clone()
        .with_query_embed(memory_sqlite::QueryEmbed::new(
            provider,
            kernel::memory_core::DEFAULT_EMBED_QUERY_INSTRUCTION.to_owned(),
            std::time::Duration::from_secs(5),
            tokio_util::sync::CancellationToken::new(),
        ));
    let outcome = store.search(vector_query()).await?;
    assert_eq!(outcome.results, Vec::new());
    Ok(())
}

fn vector_query() -> kernel::memory::SearchQuery {
    kernel::memory::SearchQuery {
        text: "zzzzqqqx".to_owned(),
        mode: kernel::memory::SearchMode::Vector,
        scope: kernel::memory::MemoryScope::Messages,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: kernel::memory::Limit::clamped(Some(10), 10, 1..=50),
        since: None,
        before: None,
        window: None,
        expand: None,
        synthesize: None,
    }
}

fn chunked(id: &str) -> StoredEmbedding {
    StoredEmbedding {
        target: target(id),
        vector: vec![0.6, 0.8],
        content_hash: "ignored".to_owned(),
        chunks: vec![kernel::memory::StoredChunk {
            index: 0,
            char_start: 0,
            char_end: 4,
            content: "note".to_owned(),
            vector: Some(vec![0.6, 0.8]),
        }],
    }
}

fn stored(id: &str) -> StoredEmbedding {
    StoredEmbedding {
        target: target(id),
        vector: vec![0.6, 0.8],
        content_hash: "ignored".to_owned(),
        chunks: Vec::new(),
    }
}

fn target(id: &str) -> EmbedTarget {
    EmbedTarget {
        table: EmbedTable::Messages,
        id: id.to_owned(),
    }
}
