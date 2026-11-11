use std::collections::BTreeMap;

use values::CallUsage;

use super::ModelCall;

const UNKNOWN_MODEL: &str = "unknown";

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CallTotals {
    pub calls: u64,
    pub errors: u64,
    pub usage: CallUsage,
    pub models: BTreeMap<String, u64>,
}

impl CallTotals {
    #[must_use]
    pub fn of(calls: &[ModelCall]) -> Self {
        let mut totals = Self::default();
        for call in calls {
            totals.calls += 1;
            if call.outcome.is_error() {
                totals.errors += 1;
            }
            if let Some(usage) = call.usage {
                totals.usage.input = totals.usage.input.saturating_add(usage.input);
                totals.usage.output = totals.usage.output.saturating_add(usage.output);
                totals.usage.cache_read = totals.usage.cache_read.saturating_add(usage.cache_read);
                totals.usage.cache_write =
                    totals.usage.cache_write.saturating_add(usage.cache_write);
                totals.usage.reasoning = totals.usage.reasoning.saturating_add(usage.reasoning);
            }
            let model = call
                .model_served
                .as_deref()
                .or(call.model_requested.as_deref())
                .unwrap_or(UNKNOWN_MODEL);
            *totals.models.entry(model.to_owned()).or_default() += 1;
        }
        totals
    }
}
