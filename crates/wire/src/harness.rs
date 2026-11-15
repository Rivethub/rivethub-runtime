use crate::Extra;
use crate::session_id::SessionId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use strum::Display;

pub use values::HarnessId;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema, Display,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum HarnessCode {
    InvalidSessionId,
    BadRequest,
    CapabilityUnsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum HarnessSessionStatus {
    Active,
    Idle,
    Ended,
    Error,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessSessionSummary {
    pub session_id: SessionId,
    pub harness_id: HarnessId,
    pub title: Option<String>,
    pub cwd: Option<String>,
    #[serde_as(as = "crate::time::Millis")]
    pub created_at: DateTime<Utc>,
    #[serde_as(as = "crate::time::Millis")]
    pub updated_at: DateTime<Utc>,
    pub status: HarnessSessionStatus,
    pub blocked: Option<bool>,
    pub supersedes: Option<SessionId>,
    pub model: Option<String>,
    pub parent_session_id: Option<SessionId>,
    pub agent_name: Option<String>,
    pub task_id: Option<String>,
    pub effort: Option<String>,
    pub transport: Option<Transport>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Transport {
    Protocol,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessSessionList {
    pub sessions: Vec<HarnessSessionSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StoreHarnessCapabilities {
    pub list_sessions: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StoreHarness {
    pub harness_id: HarnessId,
    pub capabilities: StoreHarnessCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StoreHarnessList {
    pub harnesses: Vec<StoreHarness>,
}

#[must_use]
pub fn model_id(value: &str) -> std::borrow::Cow<'_, str> {
    match serde_json::from_str::<Vec<String>>(value) {
        Ok(parts) if !parts.is_empty() && parts.iter().all(|part| !part.is_empty()) => {
            std::borrow::Cow::Owned(parts.join("/"))
        }
        _ => std::borrow::Cow::Borrowed(value),
    }
}
