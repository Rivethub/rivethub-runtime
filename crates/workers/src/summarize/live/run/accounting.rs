use kernel::memory::summary_conversion::ConversionUsage;
use kernel::model::LlmResponse;
use kernel::model_calls::{ModelCall, TurnMetadata};
use wire::model_call::{CallOutcome, CallUsage};
use wire::stream::LlmUsage;

use super::{ConversionCharge, Run, Timestamp, Uuid, WorkerError};

impl Run<'_> {
    pub(super) async fn report_conversion(
        &self,
        charge: Option<ConversionCharge>,
        usage: Option<&LlmUsage>,
    ) -> Result<(), WorkerError> {
        if let (Some((conversion, _)), Some(charge), Some(usage)) =
            (self.env.conversion, charge, usage)
        {
            conversion
                .report(ConversionUsage {
                    estimate: charge.estimate,
                    input: i64::try_from(usage.prompt_tokens).map_err(|_| WorkerError::Count)?,
                    output: i64::try_from(usage.completion_tokens)
                        .map_err(|_| WorkerError::Count)?,
                })
                .await?;
        }
        Ok(())
    }

    pub(super) async fn record_model_call(
        &self,
        started: Timestamp,
        response: &Result<LlmResponse, WorkerError>,
        usage: Option<&LlmUsage>,
    ) {
        let settings = self.env.summary.settings;
        let kind = if self.env.conversion.is_some() {
            "summary-conversion"
        } else {
            "summary-live"
        };
        let conversation = self.request.conversation_id;
        let call = ModelCall {
            call_id: Uuid::new_v4().to_string(),
            started_at: started,
            finished_at: Some(Timestamp::now()),
            provider: settings.provider_id.clone(),
            model_requested: Some(settings.model.clone()),
            model_served: None,
            account: None,
            status_code: None,
            outcome: outcome(response),
            retry_after_secs: None,
            usage: usage.map(call_usage),
            session: None,
            turn: TurnMetadata::of(None, Some(kind.to_owned())),
        };
        if let Err(error) = self
            .env
            .summary
            .store
            .record_summary_call(conversation, &call)
            .await
        {
            tracing::warn!(%conversation, %error, "could not record summary model call");
        }
    }
}

fn outcome(response: &Result<LlmResponse, WorkerError>) -> CallOutcome {
    if response.is_ok() {
        CallOutcome::Completed
    } else if matches!(
        response,
        Err(WorkerError::ModelTimeout { .. }
            | WorkerError::ConversionYield
            | WorkerError::Cancelled)
    ) {
        CallOutcome::Aborted
    } else {
        CallOutcome::ServerError
    }
}

fn call_usage(usage: &LlmUsage) -> CallUsage {
    CallUsage {
        input: usage.prompt_tokens,
        output: usage.completion_tokens,
        cache_read: usage
            .cache_read_tokens
            .or(usage.cached_tokens)
            .unwrap_or_default(),
        cache_write: usage.cache_creation_tokens.unwrap_or_default(),
        reasoning: usage.reasoning_tokens.unwrap_or_default(),
    }
}
