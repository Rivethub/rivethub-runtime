use jiff::Timestamp;
use kernel::queue::JobQueue;
use kernel::wiki::consolidate::{ConsolidateWiki, WikiConsolidateStore, consolidation_plan};
use tokio_util::sync::CancellationToken;
use wire::JsonObject;

use super::embed_job;
use crate::WorkerError;

pub async fn handle_consolidate_wiki(
    store: &dyn WikiConsolidateStore,
    queue: &dyn JobQueue,
    payload: &JsonObject,
    now: Timestamp,
    cancel: &CancellationToken,
) -> Result<usize, WorkerError> {
    let options: ConsolidateWiki =
        serde_json::from_value(serde_json::to_value(payload).map_err(WorkerError::WikiJson)?)
            .map_err(WorkerError::WikiJson)?;
    let topics = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(WorkerError::Cancelled),
        result = store.wiki_topic_stats() => result?,
    };
    let mut removed = 0;
    for fold in consolidation_plan(&topics, &options) {
        if options.dry_run {
            tracing::info!(
                "wiki consolidate {} -> {} <- {} (dry run)",
                fold.cluster,
                fold.parent,
                fold.children.join(", ")
            );
            continue;
        }
        let folded = tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(WorkerError::Cancelled),
            result = store.fold_wiki(&fold, now) => result?,
        };
        if folded.is_empty() {
            continue;
        }
        tracing::info!(
            "wiki consolidate {} -> {} (+{})",
            fold.cluster,
            fold.parent,
            folded.len()
        );
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Err(WorkerError::Cancelled),
            result = queue.enqueue(embed_job(&fold.parent)?) => {
                result.map(|_| ()).map_err(WorkerError::Queue)?;
            }
        }
        removed += folded.len();
    }
    Ok(removed)
}
