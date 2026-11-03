use crate::events::ContextSource;
use crate::harness::{HarnessId, HarnessSessionSummary};
use crate::json::JsonObject;
use crate::message::Role;
use crate::session_id::SessionId;
use crate::stream::StreamEvent;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptTurn {
    pub role: Role,
    pub text: String,
    #[serde(default)]
    #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
    pub ts: Option<DateTime<Utc>>,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptResponse {
    pub id: String,
    pub command: String,
    pub turns: Vec<TranscriptTurn>,
    pub context_window: Option<u64>,
    pub compact_at: Option<u64>,
    pub context_source: Option<ContextSource>,
}

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

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessTurnAccepted {
    pub ok: bool,
    pub session_id: SessionId,
    pub redirected_to: Option<SessionId>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dismissed_dialog: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessSessionTitleRequest {
    pub title: String,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessSessionForkRequest {
    pub prompts: u32,
    pub from: Option<u64>,
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

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub last_active: DateTime<Utc>,
    pub messages: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionList {
    pub sessions: Vec<SessionSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MessageList {
    pub messages: Vec<SessionMessage>,
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

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionMessage {
    pub id: String,
    pub session_id: String,
    pub role: SessionMessageRole,
    pub text: String,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub ts: DateTime<Utc>,
    pub usage: Option<MessageUsage>,
    pub model: Option<String>,
    pub duration_ms: Option<u64>,
    pub thinking: Option<bool>,
    pub tools: Option<Vec<String>>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionPostRequest {
    pub exclude_tools: Option<Vec<String>>,
    pub text: String,
    pub user_id: Option<String>,
    pub agent: Option<String>,
    pub thinking: Option<crate::stream::ThinkingLevel>,
    pub system_prompt: Option<String>,
    pub attachments: Option<Vec<super::agents::UserTurnAttachment>>,
    pub harness: Option<crate::harness::HarnessId>,
    pub model: Option<String>,
    pub working_dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionPostAccepted {
    pub accepted: bool,
    pub session: String,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionSteerRequest {
    pub text: String,
    pub attachments: Option<Vec<super::agents::UserTurnAttachment>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionSteerResponse {
    pub outcome: crate::harness::SteerOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionPostReply {
    pub message: SessionMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionWsClientMessage {
    pub r#type: SessionClientKind,
    pub session: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SessionClientKind {
    Watch,
    Unwatch,
    Sync,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SessionWsFrame {
    Message {
        #[serde(flatten)]
        message: SessionMessage,
    },
    Stream {
        session: String,
        event: StreamEvent,
    },
    Transcript(TranscriptWsFrame),
    #[serde(rename = "sessions-dirty")]
    SessionsDirty,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptWsFrame {
    pub session: String,
    pub rev: u64,
    pub from: u64,
    pub turns: Vec<HarnessTranscriptTurn>,
    pub total: u64,
    pub command: String,
    pub truncated_before: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HarnessSocketRefusalCode {
    InvalidSessionId,
    Forbidden,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum HarnessSocketRefusal {
    Error {
        code: HarnessSocketRefusalCode,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum HarnessSocketMessage {
    Sync,
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
