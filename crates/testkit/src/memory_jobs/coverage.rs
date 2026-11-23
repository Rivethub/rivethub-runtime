use std::collections::BTreeMap;

use jiff::Timestamp;
use kernel::memory::MemoryStore;
use kernel::memory::NewSummary;
use kernel::memory::SummaryKind;
use kernel::memory::SummaryStore;
use kernel::memory::jobs::MemoryJobStore;
use kernel::queue::ClaimSelector;
use kernel::queue::FixedJob;
use kernel::queue::Job;
use kernel::queue::JobKeyMode;
use kernel::queue::JobName;
use kernel::queue::JobQueue;
use kernel::wiki::builder::ExtractWiki;
use uuid::Uuid;
use wire::JsonObject;

use super::user;
use crate::topics::Outcome;

#[derive(Debug)]
pub struct AgedDead {
    pub key: String,
    pub idle_hours: i64,
    pub locked_hours: Option<i64>,
}

pub async fn a_recent_leaf_is_queued_ahead_of_an_old_one<S, Q, F>(
    store: &S,
    queue: &Q,
    mut sweep: F,
) -> Outcome
where
    S: MemoryStore + MemoryJobStore + SummaryStore,
    Q: JobQueue,
    F: AsyncFnMut(bool, u32) -> Result<(u64, u64), Box<dyn std::error::Error + Send + Sync>>,
{
    let notes = super::capture(store, user("claude-code:notes", "notes")).await?;
    let pulse = super::capture(store, user("heartbeat:pulse", "pulse")).await?;
    let conversation = notes.conversation;
    let recent = summary(store, conversation, SummaryKind::Leaf, Timestamp::now()).await?;
    let old = summary(
        store,
        conversation,
        SummaryKind::Leaf,
        at("2020-01-01T00:00:00Z")?,
    )
    .await?;
    let queued = summary(
        store,
        conversation,
        SummaryKind::Leaf,
        at("2020-06-01T00:00:00Z")?,
    )
    .await?;
    let flagged = summary(
        store,
        conversation,
        SummaryKind::Leaf,
        at("2020-03-01T00:00:00Z")?,
    )
    .await?;
    summary(
        store,
        pulse.conversation,
        SummaryKind::Leaf,
        at("2020-02-01T00:00:00Z")?,
    )
    .await?;
    summary(
        store,
        conversation,
        SummaryKind::Branch,
        at("2020-04-01T00:00:00Z")?,
    )
    .await?;
    assert_eq!(sweep(false, 20).await.map_err(widen)?, (0, 0));
    assert_eq!(sweep(true, 0).await.map_err(widen)?, (0, 0));
    assert_eq!(
        sorted(wiki_ids(store).await?),
        sorted(vec![recent, old, queued, flagged])
    );
    queue
        .enqueue(wiki_job(flagged, Some(conversation), 1)?)
        .await?;
    exhaust(queue).await?;
    queue
        .enqueue(wiki_job(queued, Some(conversation), 2)?)
        .await?;
    assert_eq!(sorted(wiki_ids(store).await?), sorted(vec![recent, old]));
    assert_eq!(sweep(true, 20).await.map_err(widen)?, (0, 2));
    let mut want = BTreeMap::new();
    want.insert(recent, -10);
    want.insert(old, 10);
    want.insert(queued, 0);
    assert_eq!(claimed(queue).await?, want);
    assert_eq!(sweep(true, 20).await.map_err(widen)?, (0, 0));
    assert_eq!(wiki_ids(store).await?, Vec::<Uuid>::new());
    Ok(())
}

pub async fn dead_wiki_jobs_outside_the_windows_stay_put<S, Q, P, F>(
    store: &S,
    queue: &Q,
    mut plant: P,
    mut sweep: F,
) -> Outcome
where
    S: MemoryStore + MemoryJobStore + SummaryStore,
    Q: JobQueue,
    P: AsyncFnMut(AgedDead) -> Result<(), Box<dyn std::error::Error + Send + Sync>>,
    F: AsyncFnMut(bool, u32) -> Result<(u64, u64), Box<dyn std::error::Error + Send + Sync>>,
{
    let notes = super::capture(store, user("claude-code:windows", "notes")).await?;
    let older = summary(
        store,
        notes.conversation,
        SummaryKind::Leaf,
        at("2020-01-01T00:00:00Z")?,
    )
    .await?;
    let newest = summary(
        store,
        notes.conversation,
        SummaryKind::Leaf,
        at("2020-01-02T00:00:00Z")?,
    )
    .await?;
    let idle = Uuid::new_v4();
    let unlocked = Uuid::new_v4();
    let locked = Uuid::new_v4();
    let fresh = Uuid::new_v4();
    for id in [idle, unlocked, locked, fresh] {
        queue.enqueue(wiki_job(id, None, 2)?).await?;
    }
    plant(aged(idle, 30, None)).await.map_err(widen)?;
    plant(aged(unlocked, 29, Some(5))).await.map_err(widen)?;
    plant(aged(locked, 28, Some(1))).await.map_err(widen)?;
    plant(aged(fresh, 2, None)).await.map_err(widen)?;
    assert_eq!(sweep(true, 3).await.map_err(widen)?, (2, 1));
    let mut want = BTreeMap::new();
    want.insert(idle, 10);
    want.insert(unlocked, 10);
    want.insert(newest, 10);
    assert_eq!(claimed(queue).await?, want);
    assert_eq!(wiki_ids(store).await?, vec![older]);
    Ok(())
}

fn aged(id: Uuid, idle_hours: i64, locked_hours: Option<i64>) -> AgedDead {
    AgedDead {
        key: format!("wiki-ext-{id}"),
        idle_hours,
        locked_hours,
    }
}

async fn claimed<Q: JobQueue>(
    queue: &Q,
) -> Result<BTreeMap<Uuid, i16>, Box<dyn std::error::Error>> {
    let selector = ClaimSelector::matching(vec![JobName::Fixed(FixedJob::ExtractWiki)]);
    let mut found = BTreeMap::new();
    for _ in 0..8 {
        let Some(claimed) = queue.claim(&selector).await? else {
            break;
        };
        let stored = queue
            .read(claimed.id)
            .await?
            .ok_or("the wiki job disappeared")?;
        let priority = stored.priority.ok_or("the wiki job has no priority")?;
        let body: ExtractWiki = serde_json::from_str(&serde_json::to_string(&stored.payload)?)?;
        found.insert(body.summary_id, priority);
    }
    Ok(found)
}

async fn exhaust<Q: JobQueue>(queue: &Q) -> Outcome {
    let selector = ClaimSelector::matching(vec![JobName::Fixed(FixedJob::ExtractWiki)]);
    let claimed = queue
        .claim(&selector)
        .await?
        .ok_or("the wiki job was not claimed")?;
    queue.fail(claimed.id, "boom").await?;
    assert_eq!(queue.claim(&selector).await?, None);
    Ok(())
}

async fn wiki_ids<S: MemoryJobStore>(store: &S) -> Result<Vec<Uuid>, Box<dyn std::error::Error>> {
    Ok(store
        .missing_wiki(10)
        .await?
        .into_iter()
        .map(|row| row.job.summary_id)
        .collect())
}

fn sorted(mut ids: Vec<Uuid>) -> Vec<Uuid> {
    ids.sort();
    ids
}

async fn summary<S: SummaryStore>(
    store: &S,
    conversation: Uuid,
    kind: SummaryKind,
    at: Timestamp,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let id = Uuid::new_v4();
    store
        .write_summary(NewSummary {
            id,
            conversation_id: conversation,
            kind,
            depth: kind.depth(),
            content: "a leaf".to_owned(),
            message_count: 1,
            earliest_at: at,
            latest_at: at,
            model: "test".to_owned(),
            pipeline_version: 1,
            created_at: at,
            sources: Vec::new(),
            child_ids: Vec::new(),
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

fn at(text: &str) -> Result<Timestamp, Box<dyn std::error::Error>> {
    Ok(wire::time::parse_rfc3339(text)?)
}

fn widen(error: Box<dyn std::error::Error + Send + Sync>) -> Box<dyn std::error::Error> {
    error
}
