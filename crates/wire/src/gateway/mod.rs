pub mod account;
pub mod auth;
pub mod entities;
pub mod files;
pub mod flagged;
pub mod memory;
pub mod memory_models;
pub mod review;
pub mod roster;
pub mod session_catalog;
pub mod session_delete;
pub mod sessions;
pub mod share;
pub mod tenancy;
pub mod tidy;
pub mod today;
pub mod topics;
pub mod users;
pub mod wiki;

use crate::harness::HarnessId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HealthzResponse {
    pub ok: bool,
    pub sessions: u64,
    pub name: String,
    pub node: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub degraded_providers: Vec<DegradedProvider>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<HubPolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HubPolicy {
    pub cloud: CloudPolicy,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub share_servers: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum CloudPolicy {
    Enabled,
    Disabled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DegradedProvider {
    pub id: String,
    pub recovery: ProviderRecovery,
    pub reason: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, strum::Display,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum ProviderRecovery {
    Retrying,
    Permanent,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GatewayError {
    pub error: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub hint: Option<String>,
    #[serde(default)]
    pub code: Option<String>,
    #[serde(default)]
    pub harness_id: Option<HarnessId>,
    #[serde(default)]
    pub node: Option<String>,
    #[serde(default)]
    pub retryable: Option<bool>,
    #[serde(default)]
    pub reason: Option<String>,
}

impl GatewayError {
    #[must_use]
    pub fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into(),
            detail: None,
            hint: None,
            code: None,
            harness_id: None,
            node: None,
            retryable: None,
            reason: None,
        }
    }

    #[must_use]
    pub fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    #[must_use]
    pub fn hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    #[must_use]
    pub fn code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(code.into());
        self
    }

    #[must_use]
    pub fn reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    #[must_use]
    pub fn retryable(mut self, retryable: bool) -> Self {
        self.retryable = Some(retryable);
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct OkReply {
    pub ok: bool,
}
