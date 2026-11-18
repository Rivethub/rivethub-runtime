use chrono::TimeDelta;

use super::top_down::{
    prompt::session_ceiling,
    settings::{CHUNK_TOKENS, FALLBACK_CONTEXT_TOKENS, Limits, request_max_tokens},
};
use crate::WorkerError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummarySettings {
    pub model: String,
    pub inactivity: TimeDelta,
    pub context_tokens: u32,
    pub model_concurrency: u32,
    pub model_profile: String,
}

impl SummarySettings {
    pub fn standard(model: impl Into<String>) -> Result<Self, WorkerError> {
        let model = model.into();
        if model.is_empty() {
            return Err(invalid("summary model is empty"));
        }
        Ok(Self {
            model,
            inactivity: TimeDelta::minutes(i64::from(
                kernel::memory::settlement::DEFAULT_INACTIVITY_MINUTES,
            )),
            context_tokens: FALLBACK_CONTEXT_TOKENS,
            model_concurrency: 3,
            model_profile: String::new(),
        })
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
