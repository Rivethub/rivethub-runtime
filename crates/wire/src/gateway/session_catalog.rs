use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::harness::HarnessId;
use crate::session_id::{SessionId, SessionKey};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum SessionMemoryState {
    Capturing,
    Queued,
    Summarized,
    Flagged,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopicPath {
    pub id: String,
    pub path: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SharedWith {
    Private,
    Space { space: String },
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionOriginLabel {
    pub author: Option<String>,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub verification: Option<crate::capture::ReceiptVerification>,
    pub bundle_id: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummaryRow {
    pub owner: Option<String>,
    pub id: SessionKey,
    pub title: String,
    pub harness: Option<HarnessId>,
    pub origin: Option<SessionOriginLabel>,
    pub started_at: DateTime<Utc>,
    pub ended_at: Option<DateTime<Utc>>,
    pub message_count: u64,
    pub topic: Option<TopicPath>,
    pub visibility: SharedWith,
    pub kept: bool,
    pub trashed: bool,
    pub memory_state: SessionMemoryState,
    pub gist: Option<String>,
    pub parent_id: Option<SessionId>,
    pub child_count: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummaryPage {
    pub items: Vec<SessionSummaryRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionDetail {
    #[serde(flatten)]
    pub row: SessionSummaryRow,
    pub cwd: Option<String>,
    pub resume_command: Option<String>,
    pub changed: Vec<SessionChange>,
    pub entities: Vec<SessionEntity>,
    pub tags: Vec<SessionTag>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionChange {
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionEntity {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionTag {
    pub key: String,
    pub value: String,
    pub state: String,
}
