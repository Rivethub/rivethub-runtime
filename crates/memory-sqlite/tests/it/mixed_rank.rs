use std::sync::Arc;
use std::time::Duration;

use kernel::model::Embedding;
use memory_sqlite::QueryEmbed;
use testkit::FakeProvider;
use testkit::Script;
use tokio_util::sync::CancellationToken;

use crate::common::TestResult;
use crate::common::fresh_store;

fn searching(store: memory_sqlite::SqliteStore) -> memory_sqlite::SqliteStore {
    let provider: Arc<dyn kernel::model::ModelProvider> =
        Arc::new(FakeProvider::new(vec![Script::Embed(vec![Embedding {
            vector: testkit::summary_passages::vector(1.0, 0.0),
        }])]));
    store.with_query_embed(QueryEmbed::new(
        provider,
        kernel::memory_core::DEFAULT_EMBED_QUERY_INSTRUCTION.to_owned(),
        Duration::from_secs(5),
        CancellationToken::new(),
    ))
}

#[tokio::test]
async fn a_message_a_chunk_and_a_passage_rank_together() -> TestResult {
    let fresh = fresh_store()?;
    testkit::mixed_rank::a_message_a_chunk_and_a_passage_rank_together(&searching(fresh.store))
        .await
}

#[tokio::test]
async fn a_near_tie_parent_outranks_the_passage() -> TestResult {
    let fresh = fresh_store()?;
    testkit::mixed_rank::a_near_tie_parent_outranks_the_passage(&searching(fresh.store)).await
}
