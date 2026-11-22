use std::pin::pin;
use std::time::Duration;

use kernel::wiki::entity_pass::PublishOutcome;
use tokio::time::timeout;

use super::{EntityScenarioStore, at, commit, conversation, leaf_at, rewrite};
use crate::topics::Outcome;

const BLOCKED_WINDOW: Duration = Duration::from_millis(150);
const RELEASE_WINDOW: Duration = Duration::from_secs(5);

pub async fn rewrite_waits_for_page(
    holder: &impl EntityScenarioStore,
    waiter: &impl EntityScenarioStore,
) -> Outcome {
    let conversation = conversation(holder).await?;
    let leaf = leaf_at(holder, conversation, "rewrite", at("2026-11-01T00:00:00Z")?).await?;
    let entity = commit(holder, &leaf, "host-a", "Hosts", &[], None).await?;
    let mut first = holder.lease_entity(entity.id).await?;
    assert_eq!(first.page().await?.summary, "");
    let mut waiting = pin!(waiter.lease_entity(entity.id));
    assert!(timeout(BLOCKED_WINDOW, waiting.as_mut()).await.is_err());
    let body = rewrite::body(&leaf, "First rewrite.", &[], "Observed the host.");
    let written = first.apply(&body, leaf.start).await?;
    assert!(matches!(written, PublishOutcome::Stored { .. }));
    first.release().await?;
    let mut second = timeout(RELEASE_WINDOW, waiting).await??;
    assert_eq!(second.page().await?.summary, "First rewrite.");
    second.release().await?;
    Ok(())
}

pub async fn merge_waits_for_both_entities(
    holder: &impl EntityScenarioStore,
    waiter: &impl EntityScenarioStore,
) -> Outcome {
    let conversation = conversation(holder).await?;
    let older = leaf_at(holder, conversation, "older", at("2026-10-01T00:00:00Z")?).await?;
    let newer = leaf_at(holder, conversation, "newer", at("2026-11-01T00:00:00Z")?).await?;
    let keep = commit(holder, &older, "host-a", "Hosts", &[], None).await?;
    let absorb = commit(holder, &newer, "host-b", "Hosts", &[], None).await?;
    let mut first = holder.lease_entity(keep.id.min(absorb.id)).await?;
    let mut waiting = pin!(waiter.merge_entities(keep.id, absorb.id, newer.start));
    assert!(timeout(BLOCKED_WINDOW, waiting.as_mut()).await.is_err());
    let mut second = timeout(RELEASE_WINDOW, holder.lease_entity(keep.id.max(absorb.id))).await??;
    first.release().await?;
    assert!(timeout(BLOCKED_WINDOW, waiting.as_mut()).await.is_err());
    second.page().await?;
    second.release().await?;
    let merged = timeout(RELEASE_WINDOW, waiting)
        .await??
        .ok_or("merge missing")?;
    assert!(merged.performed);
    assert_eq!((merged.survivor, merged.absorbed), (keep.id, absorb.id));
    Ok(())
}
