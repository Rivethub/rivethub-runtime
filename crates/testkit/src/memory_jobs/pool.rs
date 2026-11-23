use kernel::memory::EmbeddingStore;
use kernel::memory::MemoryScope;
use kernel::memory::MemoryStore;
use kernel::memory::SearchMode;
use kernel::memory::SearchQuery;
use kernel::memory::StoredChunk;
use kernel::memory::jobs::MemoryJobStore;

use super::chunks::unit_span_vector;
use super::message;
use super::pair;
use super::user;
use crate::topics::Outcome;

const NEAR: &str = "short-a";
const FAR: &str = "short-b";
const CROWD: &str = "short-c";

pub async fn the_nearest_chunk_pool_keeps_one_chunk_per_message<S>(store: &S) -> Outcome
where
    S: MemoryStore + MemoryJobStore + EmbeddingStore,
{
    let near = message(store, user("pool", NEAR)).await?;
    let far = message(store, user("pool", FAR)).await?;
    store
        .write_chunks(near, vec![unit_span_vector(0, "c", &pair(1.0, 0.5))])
        .await?;
    store
        .write_chunks(far, vec![unit_span_vector(0, "c", &pair(1.0, 1.0))])
        .await?;
    assert_ordered_pair(&store.search(hybrid_query()).await?)?;
    let crowd = message(store, user("pool", CROWD)).await?;
    store.write_chunks(crowd, crowd_spans()?).await?;
    assert_only_crowd(&store.search(hybrid_query()).await?);
    Ok(())
}

fn assert_ordered_pair(outcome: &kernel::memory::SearchOutcome) -> Outcome {
    assert_eq!(outcome.degraded, None);
    let contents: Vec<&str> = outcome
        .results
        .iter()
        .map(|hit| hit.content.as_str())
        .collect();
    assert_eq!(contents, vec![NEAR, FAR]);
    let marked: Vec<bool> = outcome
        .results
        .iter()
        .map(|hit| hit.chunk.is_some())
        .collect();
    assert_eq!(marked, vec![true, true]);
    super::chunks::decreasing_scores(&outcome.results)?;
    Ok(())
}

fn assert_only_crowd(outcome: &kernel::memory::SearchOutcome) {
    assert_eq!(outcome.degraded, None);
    let contents: Vec<&str> = outcome
        .results
        .iter()
        .map(|hit| hit.content.as_str())
        .collect();
    assert_eq!(contents, vec![CROWD]);
}

fn crowd_spans() -> Result<Vec<StoredChunk>, Box<dyn std::error::Error>> {
    let count = usize::try_from(kernel::memory_core::chunk_pool(2) + 1)?;
    (0..count)
        .map(|index| {
            let index = u32::try_from(index)?;
            Ok(unit_span_vector(index, "c", &pair(1.0, 0.0)))
        })
        .collect()
}

fn hybrid_query() -> SearchQuery {
    SearchQuery {
        text: "zzzzqqqx".to_owned(),
        mode: SearchMode::Hybrid,
        scope: MemoryScope::Messages,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: kernel::memory::Limit::clamped(Some(2), 10, 1..=50),
        since: None,
        before: None,
        window: None,
        expand: None,
        synthesize: None,
    }
}
