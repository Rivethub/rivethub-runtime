use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use super::SummaryKind;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    strum::EnumString,
)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "camelCase")]
pub enum VectorState {
    Embedded,
    Pending,
    Failed,
    Skipped,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemorySummaryNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub level: u32,
    pub kind: SummaryKind,
    pub content: String,
    pub message_count: u32,
    pub earliest_at: Option<DateTime<Utc>>,
    pub latest_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub source_message_ids: Vec<String>,
    pub vector: VectorState,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(default)]
    pub children: Option<u64>,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct MemorySummaryCounts {
    pub leaves: u64,
    pub branches: u64,
    pub roots: u64,
    pub embedded: u64,
    pub top: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemorySummaryChildren {
    pub summaries: Vec<MemorySummaryNode>,
    pub total: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemoryMessageTally {
    pub total: u32,
    pub user: u32,
    pub assistant: u32,
    pub system: u32,
    pub tool: u32,
    pub embedded: u32,
    pub pending: u32,
    pub failed: u32,
    pub skipped: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemorySummaryDagResponse {
    pub session_id: String,
    pub summaries: Vec<MemorySummaryNode>,
    pub messages: MemoryMessageTally,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<MemorySummaryCounts>,
}
