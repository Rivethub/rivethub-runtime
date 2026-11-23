use kernel::flagged::FlaggedJobs;
use kernel::memory::MemoryStore;
use kernel::memory::StoredChunk;
use kernel::memory::SummaryKind;
use kernel::memory::SummaryStore;
use kernel::memory::jobs::ChunkWrite;
use kernel::memory::jobs::MemoryJobStore;
use kernel::memory::jobs::SynthesizeToolCall;
use kernel::queue::ClaimSelector;
use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueue;
use kernel::wiki::builder::ExtractWiki;
use uuid::Uuid;
use wire::JsonObject;
use wire::message::Role;

use super::message;
use super::noted;
use super::user;
use crate::topics::Outcome;

pub async fn an_exhausted_wiki_extraction_stays_suppressed<S, Q>(store: &S, queue: &Q) -> Outcome
where
    S: MemoryStore + MemoryJobStore + SummaryStore + FlaggedJobs,
    Q: JobQueue,
{
    let notes = super::capture(store, user("claude-code:notes", "notes")).await?;
    let leaf = leaf(store, notes.conversation, "2026-06-01T00:00:00Z").await?;
    assert_eq!(wiki_ids(store, 10).await?, vec![leaf]);
    queue
        .enqueue(wiki_job(leaf, Some(notes.conversation), 1)?)
        .await?;
    assert_eq!(wiki_ids(store, 10).await?, Vec::<Uuid>::new());
    let selector = ClaimSelector::matching(vec![JobName::Fixed(FixedJob::ExtractWiki)]);
    let claimed = queue
        .claim(&selector)
        .await?
        .ok_or("the wiki job was not claimed")?;
    queue.fail(claimed.id, "boom").await?;
    assert_eq!(store.revive_dead_wiki(10).await?, 0);
    assert_eq!(queue.claim(&selector).await?, None);
    assert_eq!(wiki_ids(store, 10).await?, Vec::<Uuid>::new());
    let mut open = store.flagged().await?;
    let job = open.pop().ok_or("the exhausted wiki job was not flagged")?;
    assert_eq!(open, Vec::new());
    assert_eq!(job.key, Some(format!("wiki-ext-{leaf}")));
    let at = wire::time::parse_rfc3339("2026-06-02T00:00:00Z")?;
    store.dismiss(job.id, at).await?;
    assert_eq!(store.flagged().await?, Vec::new());
    assert_eq!(wiki_ids(store, 10).await?, Vec::<Uuid>::new());
    Ok(())
}

pub async fn an_exhausted_tool_synthesis_is_backfilled_again<S, Q, F>(
    store: &S,
    queue: &Q,
    mut backfill: F,
) -> Outcome
where
    S: MemoryStore,
    Q: JobQueue,
    F: AsyncFnMut(Option<u32>, bool) -> Result<u64, Box<dyn std::error::Error + Send + Sync>>,
{
    let id = message(
        store,
        noted("synth", Role::Assistant, "   ").tool("Bash", None, None),
    )
    .await?;
    message(
        store,
        noted("synth", Role::User, "   ").tool("Bash", None, None),
    )
    .await?;
    message(
        store,
        noted("synth", Role::Assistant, "hello").tool("Bash", None, None),
    )
    .await?;
    queue.enqueue(tool_job(id, 1)?).await?;
    let selector = ClaimSelector::matching(vec![JobName::Fixed(FixedJob::SynthesizeToolCall)]);
    let claimed = queue
        .claim(&selector)
        .await?
        .ok_or("the synthesize job was not claimed")?;
    queue.fail(claimed.id, "boom").await?;
    assert_eq!(queue.claim(&selector).await?, None);
    assert_eq!(backfill(None, false).await.map_err(widen)?, 1);
    let again = queue
        .claim(&selector)
        .await?
        .ok_or("the synthesize job was not requeued")?;
    let body: SynthesizeToolCall = serde_json::from_str(&serde_json::to_string(&again.payload)?)?;
    assert_eq!(body.message_id, id);
    Ok(())
}

pub async fn a_chunk_write_enqueues_its_embedding<S, Q>(store: &S, queue: &Q) -> Outcome
where
    S: MemoryStore + MemoryJobStore,
    Q: JobQueue,
{
    let id = message(store, user("chunks", "parent body")).await?;
    assert_eq!(
        store
            .write_chunks(id, vec![span(0, "seed chunk"), span(1, "   ")])
            .await?,
        ChunkWrite::Written
    );
    let selector = ClaimSelector::matching(vec![JobName::Fixed(FixedJob::EmbedTarget)]);
    let mut chunks = 0_u32;
    for _ in 0..8 {
        let Some(claimed) = queue.claim(&selector).await? else {
            break;
        };
        let body: EmbedBody = serde_json::from_str(&serde_json::to_string(&claimed.payload)?)?;
        if body.target_table == kernel::memory::EmbedTable::MessageChunks.to_string() {
            chunks += 1;
        }
    }
    assert_eq!(chunks, 1);
    Ok(())
}

#[derive(serde::Deserialize)]
struct EmbedBody {
    #[serde(rename = "targetTable")]
    target_table: String,
}

async fn wiki_ids<S: MemoryJobStore>(
    store: &S,
    limit: u32,
) -> Result<Vec<Uuid>, Box<dyn std::error::Error>> {
    let mut ids: Vec<Uuid> = store
        .missing_wiki(limit)
        .await?
        .into_iter()
        .map(|row| row.job.summary_id)
        .collect();
    ids.sort();
    Ok(ids)
}

async fn leaf<S: SummaryStore>(
    store: &S,
    conversation: Uuid,
    at: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let id = Uuid::new_v4();
    let at = wire::time::parse_rfc3339(at)?;
    store
        .write_summary(kernel::memory::NewSummary {
            id,
            conversation_id: conversation,
            kind: SummaryKind::Leaf,
            depth: SummaryKind::Leaf.depth(),
            content: "the notes leaf".to_owned(),
            message_count: 1,
            earliest_at: at,
            latest_at: at,
            model: "test".to_owned(),
            pipeline_version: 1,
            created_at: at,
            sources: Vec::new(),
            child_ids: Vec::<Uuid>::new(),
        })
        .await?;
    Ok(id)
}

fn wiki_job(
    summary_id: Uuid,
    conversation_id: Option<Uuid>,
    max_attempts: i32,
) -> Result<Job, serde_json::Error> {
    Ok(Job {
        name: JobName::Fixed(FixedJob::ExtractWiki),
        payload: JsonObject::from_serialize(&ExtractWiki {
            summary_id,
            conversation_id,
        })?,
        key: Some(format!("wiki-ext-{summary_id}")),
        key_mode: JobKeyMode::KeepRunAt,
        priority: 0,
        run_at: None,
        max_attempts: Some(max_attempts),
    })
}

fn tool_job(message_id: Uuid, max_attempts: i32) -> Result<Job, serde_json::Error> {
    Ok(Job {
        name: JobName::Fixed(FixedJob::SynthesizeToolCall),
        payload: JsonObject::from_serialize(&SynthesizeToolCall { message_id })?,
        key: Some(format!("tool-synth-{message_id}")),
        key_mode: JobKeyMode::KeepRunAt,
        priority: 0,
        run_at: None,
        max_attempts: Some(max_attempts),
    })
}

fn span(index: u32, content: &str) -> StoredChunk {
    StoredChunk {
        index,
        char_start: 0,
        char_end: 1,
        content: content.to_owned(),
        vector: None,
    }
}

fn widen(error: Box<dyn std::error::Error + Send + Sync>) -> Box<dyn std::error::Error> {
    error
}
