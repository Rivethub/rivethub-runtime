pub mod candidates;
pub mod known;
pub mod merge;
pub mod rewrite;
pub mod transcript;

use jiff::Timestamp;
use kernel::facts::FactStore;
use kernel::memory::{MemoryStore, NewSummary, SummaryKind, SummarySource, SummaryStore};
use kernel::wiki::entity_pass::{
    EntityLeaf, EntityStore, EntityView, EntityWork, ExtractEntities, PreparedEntity,
    PreparedHistory,
};
use uuid::Uuid;
use wire::time::TimestampFormat as _;

use crate::topics::Outcome;

pub trait EntityScenarioStore: EntityStore + FactStore + MemoryStore + SummaryStore {}

impl<T: EntityStore + FactStore + MemoryStore + SummaryStore> EntityScenarioStore for T {}

type ScenarioResult<T> = Result<T, Box<dyn std::error::Error>>;

fn at(text: &str) -> Result<Timestamp, jiff::Error> {
    text.parse()
}

async fn conversation(store: &impl EntityScenarioStore) -> ScenarioResult<Uuid> {
    let session = format!("claude-code:{}", Uuid::new_v4());
    crate::filing::capture(store, &session, "claude-code", "Entity scenario", None).await
}

async fn leaf_at(
    store: &impl EntityScenarioStore,
    conversation: Uuid,
    content: &str,
    time: Timestamp,
) -> ScenarioResult<EntityLeaf> {
    leaf_with_sources(store, conversation, content, time, time, Vec::new()).await
}

async fn leaf_with_sources(
    store: &impl EntityScenarioStore,
    conversation: Uuid,
    content: &str,
    start: Timestamp,
    end: Timestamp,
    sources: Vec<SummarySource>,
) -> ScenarioResult<EntityLeaf> {
    let id = Uuid::new_v4();
    store
        .write_summary(NewSummary {
            id,
            conversation_id: conversation,
            kind: SummaryKind::Leaf,
            depth: 0,
            content: content.to_owned(),
            message_count: u32::try_from(sources.len())?,
            earliest_at: start,
            latest_at: end,
            model: "fixture".to_owned(),
            pipeline_version: 6,
            created_at: start,
            sources,
            child_ids: Vec::new(),
        })
        .await?;
    match store.entity_work(&job(id, conversation)).await? {
        EntityWork::Pending(leaf) => Ok(leaf),
        EntityWork::Complete { entities } => {
            Err(format!("new summary was already extracted: {entities:?}").into())
        }
    }
}

fn job(summary_id: Uuid, conversation: Uuid) -> ExtractEntities {
    ExtractEntities {
        summary_id,
        conversation_id: Some(conversation),
        session_at: None,
    }
}

fn prepared(name: &str, kind: &str, aliases: &[&str], entity_ref: Option<Uuid>) -> PreparedEntity {
    PreparedEntity {
        entity_ref,
        name: name.to_owned(),
        type_name: kind.to_owned(),
        aliases: aliases.iter().map(|alias| (*alias).to_owned()).collect(),
        facts: Vec::new(),
        history: Vec::new(),
    }
}

async fn commit(
    store: &impl EntityScenarioStore,
    leaf: &EntityLeaf,
    name: &str,
    kind: &str,
    aliases: &[&str],
    entity_ref: Option<Uuid>,
) -> ScenarioResult<EntityView> {
    Ok(store
        .commit(
            std::slice::from_ref(leaf),
            &prepared(name, kind, aliases, entity_ref),
            leaf.end,
        )
        .await?
        .ok_or("entity commit was skipped")?)
}

fn history(leaf: &EntityLeaf, sentence: &str) -> PreparedHistory {
    PreparedHistory {
        summary_id: leaf.id,
        conversation_id: leaf.conversation_id,
        session_on: leaf.start.utc_date(),
        sentence: sentence.to_owned(),
    }
}

pub async fn backfill_and_completion(store: &impl EntityScenarioStore) -> Outcome {
    use kernel::wiki::builder::BackfillBudget;
    let conversation = conversation(store).await?;
    let later = leaf_at(store, conversation, "later", at("2026-11-07T00:00:00Z")?).await?;
    let older = leaf_at(store, conversation, "older", at("2026-11-01T00:00:00Z")?).await?;
    let middle = leaf_at(store, conversation, "middle", at("2026-11-03T00:00:00Z")?).await?;
    store.fail_entity(middle.id, "retry", middle.start).await?;
    let jobs = store.entity_backfill(BackfillBudget::flat(100)).await?;
    let sequence = jobs
        .into_iter()
        .map(|job| (job.summary_id, job.session_at))
        .collect::<Vec<_>>();
    assert_eq!(
        sequence,
        [
            (older.id, Some(older.start)),
            (middle.id, Some(middle.start)),
            (later.id, Some(later.start))
        ]
    );
    store
        .finish_entities(&[middle.id], &["host".to_owned()], None, middle.start)
        .await?;
    store
        .fail_entity(middle.id, "late failure", later.start)
        .await?;
    assert_eq!(
        store.entity_work(&job(middle.id, conversation)).await?,
        EntityWork::Complete {
            entities: vec!["host".to_owned()]
        }
    );
    let following = store.following(&older, 5).await?;
    assert_eq!(
        following
            .into_iter()
            .map(|next| (next.leaf.id, next.done))
            .collect::<Vec<_>>(),
        [(middle.id, true), (later.id, false)]
    );
    Ok(())
}

pub async fn stale_refresh(store: &impl EntityScenarioStore) -> Outcome {
    let conversation = conversation(store).await?;
    let now = Timestamp::now();
    let fresh = leaf_at(store, conversation, "fresh", now).await?;
    store.fail_entity(fresh.id, "retry", now).await?;
    let jobs = store.stale_entities(10).await?;
    assert_eq!(
        jobs.iter().map(|job| job.summary_id).collect::<Vec<_>>(),
        [fresh.id]
    );
    store.finish_entities(&[fresh.id], &[], None, now).await?;
    assert_eq!(store.stale_entities(10).await?, Vec::new());
    Ok(())
}
