use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
pub use values::{CallOutcome, CallUsage};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelCallBatchRequest {
    pub calls: Vec<CapturedModelCall>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CapturedModelCall {
    pub call_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub provider: String,
    pub model_requested: Option<String>,
    pub model_served: Option<String>,
    pub account: Option<String>,
    pub status_code: Option<u16>,
    pub outcome: CallOutcome,
    pub retry_after_secs: Option<u64>,
    pub usage: Option<CallUsage>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ModelCallBatchResult {
    pub ok: bool,
    pub inserted: u64,
    pub duplicate: u64,
    pub matched: u64,
    pub unmatched: u64,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct StoredModelCall {
    pub call_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub provider: String,
    pub model_requested: Option<String>,
    pub model_served: Option<String>,
    pub account: Option<String>,
    pub status_code: Option<u16>,
    pub outcome: CallOutcome,
    pub retry_after_secs: Option<u64>,
    pub usage: Option<CallUsage>,
    pub turn_id: Option<String>,
    pub request_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CallTotals {
    pub calls: u64,
    pub errors: u64,
    pub usage: CallUsage,
    pub models: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionModelCalls {
    pub totals: CallTotals,
    pub calls: Vec<StoredModelCall>,
}
