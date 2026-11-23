use std::sync::Arc;
use std::time::Duration;

use kernel::model::Embedding;
use memory_sqlite::QueryEmbed;
use memory_sqlite::SqliteStore;
use testkit::FakeProvider;
use testkit::Script;
use tokio_util::sync::CancellationToken;

use crate::common::TestResult;
use crate::common::fresh_store;

fn searching(store: SqliteStore, scripts: usize) -> SqliteStore {
    let provider: Arc<dyn kernel::model::ModelProvider> = Arc::new(FakeProvider::new(
        (0..scripts)
            .map(|_| {
                Script::Embed(vec![Embedding {
                    vector: testkit::summary_passages::vector(1.0, 0.0),
                }])
            })
            .collect(),
    ));
    store.with_query_embed(QueryEmbed::new(
        provider,
        kernel::memory_core::DEFAULT_EMBED_QUERY_INSTRUCTION.to_owned(),
        Duration::from_secs(5),
        CancellationToken::new(),
    ))
}

#[tokio::test]
async fn a_trashed_session_loses_its_chunk() -> TestResult {
    let fresh = fresh_store()?;
    testkit::chunk_search::a_trashed_session_loses_its_chunk(&searching(fresh.store, 1)).await
}

#[tokio::test]
async fn a_role_filter_drops_the_other_parents_chunk() -> TestResult {
    let fresh = fresh_store()?;
    testkit::chunk_search::a_role_filter_drops_the_other_parents_chunk(&searching(fresh.store, 1))
        .await
}

#[tokio::test]
async fn a_time_window_drops_an_older_chunk() -> TestResult {
    let fresh = fresh_store()?;
    testkit::chunk_search::a_time_window_drops_an_older_chunk(&searching(fresh.store, 1)).await
}

#[tokio::test]
async fn said_only_drops_a_tool_parents_chunk() -> TestResult {
    let fresh = fresh_store()?;
    testkit::chunk_search::said_only_drops_a_tool_parents_chunk(&searching(fresh.store, 1)).await
}

#[tokio::test]
async fn a_hybrid_tag_keeps_the_tagged_conversations_chunk() -> TestResult {
    let fresh = fresh_store()?;
    testkit::chunk_search::a_hybrid_tag_keeps_the_tagged_conversations_chunk(&searching(
        fresh.store,
        1,
    ))
    .await
}

#[tokio::test]
async fn clearing_chunk_vectors_drops_the_hit() -> TestResult {
    let fresh = fresh_store()?;
    let store = searching(fresh.store.clone(), 2);
    testkit::chunk_search::clearing_chunk_vectors_drops_the_hit(&store, async || {
        fresh.store.reembed(true).await?;
        Ok(())
    })
    .await
}
