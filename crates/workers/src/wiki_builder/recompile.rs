use jiff::Timestamp;
use kernel::model::ModelProvider;
use kernel::queue::JobQueue;
use kernel::wiki::recompile::{RecompileWiki, WikiRecompileStore, recompile};
use tokio_util::sync::CancellationToken;
use wire::JsonObject;

use super::{ModelMerger, embed_job};
use crate::WorkerError;

#[expect(missing_debug_implementations)]
pub struct RecompileEnv<'a> {
    pub store: &'a dyn WikiRecompileStore,
    pub provider: &'a dyn ModelProvider,
    pub queue: &'a dyn JobQueue,
    pub model: &'a str,
    pub now: Timestamp,
    pub cancel: &'a CancellationToken,
}

pub async fn handle_recompile_wiki(
    env: &RecompileEnv<'_>,
    payload: &JsonObject,
) -> Result<(), WorkerError> {
    let job: RecompileWiki =
        serde_json::from_value(serde_json::to_value(payload).map_err(WorkerError::WikiJson)?)
            .map_err(WorkerError::WikiJson)?;
    let merger = ModelMerger {
        provider: env.provider,
        model: env.model,
        cancel: env.cancel,
    };
    let report = tokio::select! {
        biased;
        () = env.cancel.cancelled() => return Err(WorkerError::Cancelled),
        result = recompile(env.store, &merger, &job, env.now) => match result {
            Err(error) if error.obsolete_input() => return Ok(()),
            result => result?,
        },
    };
    let Some(report) = report else {
        tracing::info!("recompile-wiki {}: no page file", job.slug.as_ref());
        return Ok(());
    };
    tracing::info!(
        "recompile-wiki {}: restored {} history entries, {} merge(s), {} merge(s) fell back",
        job.slug.as_ref(),
        report.restored,
        report.merges,
        report.merge_failures
    );
    env.queue
        .enqueue(embed_job(job.slug.as_ref())?)
        .await
        .map_err(WorkerError::Queue)?;
    Ok(())
}
