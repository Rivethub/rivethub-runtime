use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::JsonObject;

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlaggedJob {
    pub id: i64,
    pub task: String,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default)]
    pub subject: Option<String>,
    pub payload: JsonObject,
    pub error: String,
    pub failures: u32,
    #[serde(default)]
    #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
    pub first_failed_at: Option<DateTime<Utc>>,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub last_failed_at: DateTime<Utc>,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub flagged_at: DateTime<Utc>,
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlaggedGroup {
    pub task: String,
    pub cause: String,
    pub count: u64,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub last_failed_at: DateTime<Utc>,
    pub sample: FlaggedJob,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FlaggedJobs {
    pub groups: Vec<FlaggedGroup>,
}
