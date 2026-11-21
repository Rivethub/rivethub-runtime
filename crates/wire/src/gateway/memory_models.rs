use serde::{Deserialize, Serialize};

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    strum::Display,
    strum::EnumIter,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum ModelRole {
    Default,
    Summarizer,
    Categorizer,
    WikiWriter,
    EntityBuilder,
    Embedder,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ModelHealth {
    Healthy,
    Slow,
    Failing,
    Unconfigured,
    #[serde(rename = "not-running")]
    NotRunning,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ModelJob {
    pub role: ModelRole,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    pub inherits_default: bool,
    pub health: ModelHealth,
    pub detail: String,
    pub queued: u64,
    #[serde(default)]
    pub outage: Option<String>,
    #[serde(default)]
    pub reported_by: Option<String>,
}
