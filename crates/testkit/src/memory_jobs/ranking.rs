use kernel::memory::EmbedTable;
use kernel::memory::EmbedTarget;
use kernel::memory::EmbeddingStore;
use kernel::memory::Limit;
use kernel::memory::MemoryScope;
use kernel::memory::MemoryStore;
use kernel::memory::SearchMode;
use kernel::memory::SearchQuery;
use kernel::memory::StoredEmbedding;
use kernel::memory::jobs::MemoryJobStore;
use uuid::Uuid;
use wire::message::Role;

use super::capture;
use super::chunks::unit_span_vector;
use super::query_vector;
use crate::topics::Outcome;

const OUTSIDE: &str = "outside-cap";
const CHUNK: &str = "outside chunk";
const FILLERS: usize = 100;

pub async fn a_parent_outside_the_cap_uses_the_chunk_score<S>(store: &S) -> Outcome
where
    S: MemoryStore + MemoryJobStore + EmbeddingStore,
{
    let saved = plant(store).await?;
    let outside = saved
        .iter()
        .find(|(content, _)| content == OUTSIDE)
        .map(|(_, id)| *id)
        .ok_or("the outside parent was not stored")?;
    store
        .write_chunks(outside, vec![unit_span_vector(0, CHUNK, &tilted(4.0))])
        .await?;
    let outcome = store.search(vector_query()).await?;
    assert_eq!(outcome.degraded, None);
    let outside_hit = outcome
        .results
        .iter()
        .find(|hit| hit.content == OUTSIDE)
        .ok_or("the parent outside the cap is missing")?;
    assert_eq!(
        outside_hit
            .chunk
            .as_ref()
            .map(|chunk| chunk.content.as_str()),
        Some(CHUNK)
    );
    let filler = outcome
        .results
        .iter()
        .find(|hit| hit.content.starts_with("old-"))
        .ok_or("a capped parent is missing")?;
    let gap = filler.relevance_score - outside_hit.relevance_score;
    if gap <= 0.3 {
        return Err(format!("the outside parent kept too much of its own score: {gap}").into());
    }
    Ok(())
}

async fn plant<S>(store: &S) -> Result<Vec<(String, Uuid)>, Box<dyn std::error::Error>>
where
    S: MemoryStore + EmbeddingStore,
{
    let mut notes = Vec::new();
    for index in 0..FILLERS {
        notes.push(format!("old-{index:03}"));
    }
    notes.push(OUTSIDE.to_owned());
    let mut saved = Vec::new();
    for content in &notes {
        let id = capture_one(store, content).await?;
        let vector = if content == OUTSIDE {
            tilted(1.0)
        } else {
            query_vector()
        };
        write_vector(store, id, vector).await?;
        saved.push((content.clone(), id));
    }
    Ok(saved)
}

async fn capture_one<S: MemoryStore>(
    store: &S,
    content: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let saved = capture(
        store,
        super::Note {
            session: content,
            role: Role::User,
            content,
            tool_name: None,
            tool_args: None,
            tool_result: None,
        },
    )
    .await?;
    Ok(saved.message)
}

async fn write_vector<S: EmbeddingStore>(
    store: &S,
    id: Uuid,
    vector: Vec<f32>,
) -> Result<(), Box<dyn std::error::Error>> {
    store
        .write(StoredEmbedding {
            target: EmbedTarget {
                table: EmbedTable::Messages,
                id: id.to_string(),
            },
            vector,
            content_hash: "test".to_owned(),
            chunks: Vec::new(),
        })
        .await?;
    Ok(())
}

fn tilted(second: f32) -> Vec<f32> {
    let mut vector = super::axis(0, 1.0);
    if let Some(slot) = vector.get_mut(1) {
        *slot = second;
    }
    vector
}

fn vector_query() -> SearchQuery {
    SearchQuery {
        text: "zzzzqqqx".to_owned(),
        mode: SearchMode::Vector,
        scope: MemoryScope::Messages,
        agent: None,
        role: None,
        exclude_roles: Vec::new(),
        tool_name: None,
        tag: None,
        limit: Limit::clamped(Some(120), 10, 1..=200),
        since: None,
        before: None,
        window: None,
        expand: None,
        synthesize: None,
    }
}
