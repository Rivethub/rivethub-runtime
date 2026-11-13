use std::time::Duration;

use kernel::model::ModelCallFailure;
use kernel::model::ModelProviderError;
use tokio_util::sync::CancellationToken;

use crate::WorkerError;
use crate::outage::OutageCause;

const RETRY_MIN: Duration = Duration::from_secs(2);
const RETRY_SPAN_MS: u64 = 3_000;

const LAST_CHANCE_MIN: Duration = Duration::from_secs(10);

pub(super) async fn retry_after<T>(
    cancel: &CancellationToken,
    first: Result<T, WorkerError>,
) -> Result<Option<T>, WorkerError> {
    match first {
        Ok(value) => Ok(Some(value)),
        Err(error) => {
            let Some(kind) = transient_kind(&error) else {
                return Err(error);
            };
            tracing::warn!(%error, "entity-retry {kind}: entity model call failed; retrying once");
            if !pause(cancel).await {
                return Err(WorkerError::Cancelled);
            }
            Ok(None)
        }
    }
}

pub(super) async fn last_chance<T>(
    cancel: &CancellationToken,
    second: Result<T, WorkerError>,
) -> Result<Option<T>, WorkerError> {
    match second {
        Ok(value) => Ok(Some(value)),
        Err(error) => {
            let server_side = matches!(
                transient_kind(&error),
                Some(
                    "malformed-stream"
                        | "stream-ended"
                        | "connection-reset"
                        | "http-5xx"
                        | "timeout"
                )
            );
            if !server_side {
                return Err(error);
            }
            tracing::warn!(%error, "entity-retry last chance: server-side failure twice; retrying after a longer pause");
            let wait = LAST_CHANCE_MIN + retry_delay();
            let waited = tokio::select! {
                biased;
                () = cancel.cancelled() => false,
                () = tokio::time::sleep(wait) => true,
            };
            if !waited {
                return Err(WorkerError::Cancelled);
            }
            Ok(None)
        }
    }
}

pub(super) fn transient_kind(error: &WorkerError) -> Option<&'static str> {
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "only the listed model failures are retried; every other worker error fails the job"
    )]
    match error {
        WorkerError::EmptySummary => Some("empty"),
        WorkerError::ModelTimeout { .. } => Some("timeout"),
        WorkerError::ProviderOutage {
            cause: OutageCause::ServerError,
            ..
        } => Some("http-5xx"),
        WorkerError::Llm { message } if message.contains("dropped every current fact") => {
            Some("dropped-facts")
        }
        WorkerError::WikiModel(source) => provider_kind(source),
        _ => None,
    }
}

fn provider_kind(error: &ModelProviderError) -> Option<&'static str> {
    if mentions(error, "malformed stream event") {
        return Some("malformed-stream");
    }
    if matches!(error, ModelProviderError::Stream { .. }) || mentions(error, "stream ended early") {
        return Some("stream-ended");
    }
    if mentions(error, "connection reset") {
        return Some("connection-reset");
    }
    if mentions(error, "timed out") || mentions(error, "timeout") {
        return Some("timeout");
    }
    match error.failure() {
        Some(ModelCallFailure::Http { status, .. }) if (500..600).contains(status) => {
            Some("http-5xx")
        }
        Some(_) | None => None,
    }
}

fn mentions(error: &dyn std::error::Error, needle: &str) -> bool {
    let mut current = Some(error);
    while let Some(item) = current {
        if item.to_string().to_ascii_lowercase().contains(needle) {
            return true;
        }
        current = item.source();
    }
    false
}

async fn pause(cancel: &CancellationToken) -> bool {
    tokio::select! {
        biased;
        () = cancel.cancelled() => false,
        () = tokio::time::sleep(retry_delay()) => true,
    }
}

fn retry_delay() -> Duration {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a clock before the epoch uses the minimum retry delay"
        )
    )]
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.subsec_nanos());
    let extra = u64::from(nanos) % (RETRY_SPAN_MS + 1);
    RETRY_MIN + Duration::from_millis(extra)
}

#[cfg(test)]
#[path = "tests/entity_retry_tests.rs"]
mod tests;
