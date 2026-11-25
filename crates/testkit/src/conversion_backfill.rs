use std::collections::BTreeSet;

use async_trait::async_trait;
use jiff::Timestamp;
use kernel::memory::{MemoryStore, NewSummary, SummaryKind, SummaryStore};
use kernel::wiki::builder::{BackfillBudget, WikiBuilderStore};
use kernel::wiki::entity_pass::EntityStore;
use uuid::Uuid;

use crate::topics::Outcome;

type SeedResult = Result<(), Box<dyn std::error::Error>>;

#[async_trait]
pub trait ConversionSeed: Send + Sync {
    async fn convert(&self, conversation: Uuid, state: &str) -> SeedResult;

    async fn release(&self, conversation: Uuid, anchor: Uuid) -> SeedResult;
}

pub trait BackfillStore: EntityStore + WikiBuilderStore + MemoryStore + SummaryStore {}

impl<T: EntityStore + WikiBuilderStore + MemoryStore + SummaryStore> BackfillStore for T {}

pub async fn legacy_leaves_wait_for_their_conversion(
    store: &impl BackfillStore,
    seed: &impl ConversionSeed,
) -> Outcome {
    let converting = conversation(store).await?;
    let legacy = leaf(store, converting, "legacy").await?;
    let anchor = leaf(store, converting, "released").await?;
    let plain = leaf(store, conversation(store).await?, "plain").await?;
    seed.convert(converting, "pending").await?;
    seed.release(converting, anchor).await?;

    let waiting = BTreeSet::from([anchor, plain]);
    assert_eq!(entity_backfill(store).await?, waiting, "entity, converting");
    assert_eq!(wiki_backfill(store).await?, waiting, "wiki, converting");

    seed.convert(converting, "complete").await?;
    let all = BTreeSet::from([legacy, anchor, plain]);
    assert_eq!(entity_backfill(store).await?, all, "entity, converted");
    assert_eq!(wiki_backfill(store).await?, all, "wiki, converted");
    Ok(())
}

async fn entity_backfill(
    store: &impl BackfillStore,
) -> Result<BTreeSet<Uuid>, Box<dyn std::error::Error>> {
    let jobs = store.entity_backfill(BackfillBudget::flat(100)).await?;
    Ok(jobs.into_iter().map(|job| job.summary_id).collect())
}

async fn wiki_backfill(
    store: &impl BackfillStore,
) -> Result<BTreeSet<Uuid>, Box<dyn std::error::Error>> {
    let jobs = store.wiki_backfill(BackfillBudget::flat(100)).await?;
    Ok(jobs.into_iter().map(|job| job.summary_id).collect())
}

async fn conversation(store: &impl BackfillStore) -> Result<Uuid, Box<dyn std::error::Error>> {
    let session = format!("claude-code:{}", Uuid::new_v4());
    crate::filing::capture(store, &session, "claude-code", "Conversion backfill", None).await
}

async fn leaf(
    store: &impl BackfillStore,
    conversation: Uuid,
    content: &str,
) -> Result<Uuid, Box<dyn std::error::Error>> {
    let id = Uuid::new_v4();
    let at: Timestamp = "2026-11-01T00:00:00Z".parse()?;
    store
        .write_summary(NewSummary {
            id,
            conversation_id: conversation,
            kind: SummaryKind::Leaf,
            depth: 0,
            content: content.to_owned(),
            message_count: 0,
            earliest_at: at,
            latest_at: at,
            model: "fixture".to_owned(),
            pipeline_version: 6,
            created_at: at,
            sources: Vec::new(),
            child_ids: Vec::new(),
        })
        .await?;
    Ok(id)
}
