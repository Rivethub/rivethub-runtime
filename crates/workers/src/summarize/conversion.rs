use jiff::Timestamp;
use std::time::Duration;

use kernel::memory::summary_conversion::{ConversionLease, ConversionOutcome, SummaryConversion};

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
        if let Err(error) = handle_conversion_tick(env, store).await {
            tracing::warn!("summary conversion pump failed: {error}");
        }
    }
}

#[doc(hidden)]
pub async fn handle_conversion_tick(
    env: &live::LiveEnv<'_>,
    store: &dyn SummaryConversion,
) -> Result<(), WorkerError> {
    let settings = env.summary.settings;
    settings.validate_timeouts()?;
    settings.validate_conversion()?;
    let Some(lease) = store
        .claim_with_expiry(settings.inactivity, settings.conversion_lease)
        .await?
    else {
        return Ok(());
    };
    let summary = SummarizeEnv {
        now: Timestamp::now(),
        ..*env.summary
    };
    let env = live::LiveEnv {
        summary: &summary,
        ..*env
    };
    let outcome = match convert_with_heartbeat(&env, store, lease).await {
        Ok(()) => ConversionOutcome::Complete,
        Err(
            WorkerError::ConversionYield
            | WorkerError::Cancelled
            | WorkerError::SummarySuperseded
            | WorkerError::SummaryReplan,
        ) => ConversionOutcome::Yield,
        Err(WorkerError::ModelTimeout { .. } | WorkerError::EmptySummary)
            if lease.timeout_retries < env.summary.settings.conversion_timeout_retries =>
        {
            ConversionOutcome::RetryTransient
        }
        Err(WorkerError::ModelTimeout { .. } | WorkerError::EmptySummary) if !lease.second_pass => {
            ConversionOutcome::Requeue {
                delay: settings.conversion_second_pass_delay,
            }
        }
        Err(error) => ConversionOutcome::Fail(failure_reason(&error)),
    };
    store.finish(lease, outcome).await?;
    Ok(())
}

async fn convert_with_heartbeat(
    env: &live::LiveEnv<'_>,
    store: &dyn SummaryConversion,
    lease: ConversionLease,
) -> Result<(), WorkerError> {
    tokio::select! {
        biased;
        result = live::convert(env, store, lease) => result,
        result = heartbeat(env.summary.settings, store, lease) => {
            if let Err(error) = result {
                tracing::warn!(conversation = %lease.conversation, %error, "summary conversion lease renewal failed");
            }
            Err(WorkerError::ConversionYield)
        }
    }
}

async fn heartbeat(
    settings: &super::SummarySettings,
    store: &dyn SummaryConversion,
    lease: ConversionLease,
) -> Result<(), kernel::memory::SummaryStoreError> {
    loop {
        tokio::time::sleep(settings.conversion_heartbeat).await;
        if !store.renew(lease, settings.conversion_lease).await? {
            return Ok(());
        }
    }
}

fn failure_reason(error: &WorkerError) -> String {
    match error {
        WorkerError::SummarySettings { message } => return message.clone(),
        WorkerError::ModelTimeout { .. } => "model request timed out",
        WorkerError::Llm { .. } | WorkerError::ProviderOutage { .. } => "model request failed",
        WorkerError::Embed(_) | WorkerError::EmbedTimeout | WorkerError::NullEmbedding => {
            "summary embedding failed"
        }
        WorkerError::EmptySummary => "the model returned an empty summary",
        WorkerError::Summary(_) => "summary store operation failed",
        WorkerError::Tenancy(..)
        | WorkerError::JobSetting { .. }
        | WorkerError::EmptySynth
        | WorkerError::SynthPayload { .. }
        | WorkerError::MemoryJob(..)
        | WorkerError::MissingMemoryJobs
        | WorkerError::WikiStore(..)
        | WorkerError::EntityStore(..)
        | WorkerError::WikiJson(..)
        | WorkerError::WikiModel(..)
        | WorkerError::MissingDependencies { .. }
        | WorkerError::Concurrency
        | WorkerError::EmbedWidth { .. }
        | WorkerError::Width
        | WorkerError::ChunkWindow
        | WorkerError::Payload { .. }
        | WorkerError::Queue(..)
        | WorkerError::Target(..)
        | WorkerError::Unhandled { .. }
        | WorkerError::TextTooLong
        | WorkerError::Cancelled
        | WorkerError::TaskEnded(..)
        | WorkerError::Closed
        | WorkerError::SummaryPayload { .. }
        | WorkerError::ConversationMissing { .. }
        | WorkerError::SummarySuperseded
        | WorkerError::ConversionYield
        | WorkerError::SummaryReplan
        | WorkerError::Truncated
        | WorkerError::RouteBusy
        | WorkerError::EmptyTitle
        | WorkerError::TitleJson(..)
        | WorkerError::Count
        | WorkerError::Clock
        | WorkerError::FilePayload { .. }
        | WorkerError::Filing { .. }
        | WorkerError::Cleanup { .. }
        | WorkerError::Topics(..)
        | WorkerError::Lineage(..)
        | WorkerError::Memory(..)
        | WorkerError::TopicPage(..)
        | WorkerError::Flagged(..)
        | WorkerError::Tidy(..)
        | WorkerError::TidyAnswer { .. }
        | WorkerError::Changes(..)
        | WorkerError::EmptyDigest => "summary conversion failed",
    }
    .to_owned()
}
