use crate::JsonObject;
use crate::gateway::memory::MemoryHitSource;
use crate::message::Role;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEntry {
    pub id: Option<String>,
    pub session_id: String,
    pub agent: String,
    pub channel: String,
    pub role: Role,
    pub content: String,
    pub tool_name: Option<String>,
    pub tool_args: Option<JsonObject>,
    pub tool_result: Option<String>,
    pub metadata: Option<JsonObject>,
    pub created_at: Option<DateTime<Utc>>,
    pub provenance: Option<Provenance>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemorySearchResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk: Option<MemoryChunkMatch>,
    pub id: String,
    pub source: MemoryHitSource,
    pub content: String,
    pub role: Option<Role>,
    pub agent: Option<String>,
    pub relevance_score: f64,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemoryChunkMatch {
    pub index: u32,
    pub total: u32,
    pub char_start: u32,
    pub char_end: u32,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    Verified,
    Unsigned,
    UnknownKey,
    Unreceived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReceiptChecks {
    pub rules_sha256: String,
    pub findings_by_family: BTreeMap<String, u32>,
    pub decision: String,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Provenance {
    pub author: Option<String>,
    pub origin_date: Option<DateTime<Utc>>,
    pub verification: Verification,
    pub checks: Option<ReceiptChecks>,
}
