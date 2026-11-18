use std::time::Duration;

use chrono::Utc;
use kernel::memory::summary_conversion::{ConversionOutcome, SummaryConversion};

use super::{SummarizeEnv, live};
use crate::WorkerError;

pub(crate) async fn run(env: &live::LiveEnv<'_>, store: &dyn SummaryConversion) {
    let cancel = env.summary.cancel;
    let mut ticks = tokio::time::interval(Duration::from_secs(1));
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            _ = ticks.tick() => {}
        }
        let result = tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            result = handle_conversion_tick(env, store) => result,
        };
        if let Err(error) = result {
            tracing::warn!("summary conversion pump failed: {error}");
        }
    }
}

#[doc(hidden)]
pub async fn handle_conversion_tick(
    env: &live::LiveEnv<'_>,
    store: &dyn SummaryConversion,
) -> Result<(), WorkerError> {
    let Some(lease) = store.claim(env.summary.settings.inactivity).await? else {
        return Ok(());
    };
    let summary = SummarizeEnv {
        now: Utc::now(),
        ..*env.summary
    };
    let env = live::LiveEnv {
        summary: &summary,
        ..*env
    };
    let outcome = match live::convert(&env, store, lease).await {
        Ok(()) => ConversionOutcome::Complete,
        Err(
            WorkerError::ConversionYield
            | WorkerError::Cancelled
            | WorkerError::SummarySuperseded
            | WorkerError::SummaryReplan,
        ) => ConversionOutcome::Yield,
        Err(error) => ConversionOutcome::Fail(failure_reason(&error)),
    };
    store.finish(lease, outcome).await?;
    Ok(())
}

fn failure_reason(error: &WorkerError) -> String {
    match error {
        WorkerError::SummarySettings { message } => return message.clone(),
        WorkerError::Llm { .. } | WorkerError::ProviderOutage { .. } => "model request failed",
        WorkerError::Embed(_) | WorkerError::EmbedTimeout | WorkerError::NullEmbedding => {
            "summary embedding failed"
        }
        WorkerError::EmptySummary => "the model returned an empty summary",
        WorkerError::Summary(_) => "summary store operation failed",
        _ => "summary conversion failed",
    }
    .to_owned()
}
