use crate::events::ContextSource;
use crate::harness::{HarnessId, HarnessSessionSummary};
use crate::json::JsonObject;
use crate::session_id::SessionId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptBlock {
    Thinking,
    Text,
    ToolUse,
    ToolResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum TranscriptToolStatus {
    Running,
    Done,
    Error,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessTranscriptTool {
    pub name: String,
    pub status: TranscriptToolStatus,
    pub args: Option<JsonObject>,
    pub id: Option<String>,
    pub input: Option<JsonObject>,
    pub result_text: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptAttachment {
    pub media_type: String,
    pub name: Option<String>,
    pub uri: Option<String>,
    pub upload_id: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessTranscriptTurn {
    pub role: SessionMessageRole,
    pub text: String,
    pub thinking: Option<String>,
    pub tools: Option<Vec<HarnessTranscriptTool>>,
    pub usage: Option<MessageUsage>,
    pub model: Option<String>,
    pub stop_reason: Option<String>,
    pub last_block: Option<TranscriptBlock>,
    pub attachments: Option<Vec<TranscriptAttachment>>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub complete: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub compact: bool,
    pub started_at_ms: Option<i64>,
    pub ended_at_ms: Option<i64>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessSessionTranscriptResponse {
    pub session_id: SessionId,
    pub harness_id: HarnessId,
    pub turns: Vec<HarnessTranscriptTurn>,
    pub context_window: u64,
    pub compact_at: u64,
    pub context_source: ContextSource,
    pub redirected_to: Option<SessionId>,
    pub earlier: Option<u64>,
    pub context_used: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessSessionTitleRequest {
    pub title: String,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessSessionResponse {
    #[serde(flatten)]
    pub summary: HarnessSessionSummary,
    pub redirected_to: Option<SessionId>,
    pub switches: Option<SessionSwitches>,
    pub running_elsewhere: Option<bool>,
}

#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "camelCase")]
pub struct SessionSwitches {
    pub model: bool,
    pub effort: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessToolOutput {
    pub text: String,
    pub is_error: bool,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MessageUsage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionMessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionPreviewKind {
    Running,
    Asking,
    Approval,
    Said,
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionPreview {
    pub session_id: SessionId,
    pub kind: SessionPreviewKind,
    pub text: String,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionPreviewList {
    pub previews: Vec<SessionPreview>,
}
