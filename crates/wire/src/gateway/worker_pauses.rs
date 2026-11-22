use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

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
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum PausableRole {
    Categorizer,
    WikiBuilder,
    EntityBuilder,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkerPause {
    pub role: PausableRole,
    pub paused: bool,
    #[serde(default)]
    #[serde_as(as = "Option<crate::time::EpochMillis>")]
    pub paused_at: Option<Timestamp>,
    #[serde(default)]
    pub paused_by: Option<String>,
    pub queued: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WorkerPauses {
    pub roles: Vec<WorkerPause>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct WorkerPauseUpdate {
    pub paused: bool,
}
