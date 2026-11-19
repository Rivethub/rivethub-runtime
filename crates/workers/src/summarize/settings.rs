use jiff::SignedDuration;
use std::time::Duration;

use super::top_down::{
    prompt::session_ceiling,
    settings::{CHUNK_TOKENS, FALLBACK_CONTEXT_TOKENS, Limits, request_max_tokens},
};
use crate::WorkerError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummarySettings {
    pub model: String,
    pub inactivity: SignedDuration,
    pub context_tokens: u32,
    pub model_concurrency: u32,
    pub model_profile: String,
    pub provider_id: String,
    pub first_token_timeout: Duration,
    pub inter_token_timeout: Duration,
    pub request_timeout: Duration,
    pub conversion_timeout_retries: i32,
    pub conversion_lease: SignedDuration,
    pub conversion_heartbeat: Duration,
}

impl SummarySettings {
    pub fn standard(model: impl Into<String>) -> Result<Self, WorkerError> {
        let model = model.into();
        if model.is_empty() {
            return Err(invalid("summary model is empty"));
        }
        Ok(Self {
            model,
            inactivity: SignedDuration::from_mins(i64::from(
                kernel::memory::settlement::DEFAULT_INACTIVITY_MINUTES,
            )),
            context_tokens: FALLBACK_CONTEXT_TOKENS,
            model_concurrency: 3,
            model_profile: String::new(),
            provider_id: "summarizer".to_owned(),
            first_token_timeout: Duration::from_secs(180),
            inter_token_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_mins(30),
            conversion_timeout_retries: 2,
            conversion_lease: kernel::memory::summary_conversion::LEASE,
            conversion_heartbeat: Duration::from_secs(20),
        })
    }

    pub(crate) fn validate_timeouts(&self) -> Result<(), WorkerError> {
        for timeout in [self.first_token_timeout, self.inter_token_timeout] {
            if timeout.is_zero() || timeout > self.request_timeout {
                return Err(invalid(
                    "summary token timeouts must be positive and at most the request timeout",
                ));
            }
        }
        if self.request_timeout > Duration::from_hours(24) {
            return Err(invalid("summary request timeout must be at most one day"));
        }
        if !(0..=10).contains(&self.conversion_timeout_retries) {
            return Err(invalid(
                "summary conversion timeout retries must be between 0 and 10",
            ));
        }
        Ok(())
    }

    pub(crate) fn validate_lease(&self) -> Result<(), WorkerError> {
        let expiry = Duration::try_from(self.conversion_lease)
            .map_err(|_| invalid("summary conversion lease must be positive"))?;
        if self.conversion_heartbeat.is_zero()
            || expiry < self.conversion_heartbeat.saturating_mul(2)
            || expiry > Duration::from_hours(24)
        {
            return Err(invalid(
                "summary conversion lease must be at most one day and at least twice the positive heartbeat interval",
            ));
        }
        Ok(())
    }

    pub(crate) fn live_limits(&self) -> Result<Limits, WorkerError> {
        if !(1..=64).contains(&self.model_concurrency) {
            return Err(invalid(
                "summary model concurrency must be between 1 and 64",
            ));
        }
        let mut budget = CHUNK_TOKENS.min(self.context_tokens.saturating_sub(18_000));
        while budget > 0 {
            let needed =
                budget + request_max_tokens(session_ceiling(budget as usize), false) + 2000;
            if needed <= self.context_tokens {
                break;
            }
            budget = budget.saturating_sub((needed - self.context_tokens).div_ceil(2));
        }
        Limits::new(self.context_tokens, budget, false).map_err(|error| invalid(error.to_string()))
    }
}

fn invalid(message: impl Into<String>) -> WorkerError {
    WorkerError::SummarySettings {
        message: message.into(),
    }
}
