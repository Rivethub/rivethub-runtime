use crate::JsonObject;
use crate::message::Role;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CaptureBatchRequest {
    pub session_key: String,
    pub agent: String,
    pub channel: Option<String>,
    pub title: Option<String>,
    pub settings: Option<JsonObject>,
    pub task_id: Option<String>,
    pub finalize: Option<bool>,
    #[serde(default)]
    #[serde_as(as = "Option<crate::time::Millis>")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    #[serde_as(as = "Option<crate::time::Millis>")]
    pub updated_at: Option<DateTime<Utc>>,
    pub messages: Vec<CaptureMessage>,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CaptureMessage {
    pub event_id: String,
    pub role: Role,
    pub content: String,
    pub tool_name: Option<String>,
    pub tool_args: Option<JsonObject>,
    pub tool_result: Option<String>,
    pub metadata: Option<JsonObject>,
    #[serde(default)]
    #[serde_as(as = "Option<crate::time::Millis>")]
    pub created_at: Option<DateTime<Utc>>,
}

impl CaptureBatchRequest {
    #[must_use]
    pub fn entries(&self) -> Vec<crate::memory::MemoryEntry> {
        self.messages
            .iter()
            .map(|message| crate::memory::MemoryEntry {
                id: Some(message.event_id.clone()),
                session_id: self.session_key.clone(),
                agent: self.agent.clone(),
                channel: self.channel.clone().unwrap_or_default(),
                role: message.role,
                content: message.content.clone(),
                tool_name: message.tool_name.clone(),
                tool_args: message.tool_args.clone(),
                tool_result: message.tool_result.clone(),
                metadata: message.metadata.clone(),
                created_at: message.created_at,
            })
            .collect()
    }
}

pub const SESSION_BUNDLE_VERSION: u32 = 2;

pub const UNKNOWN_ORIGIN: &str = "unknown";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionBundle {
    pub version: u32,
    pub manifest: BundleManifest,
    pub sessions: Vec<BundledSession>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BundleManifest {
    pub exported_at: DateTime<Utc>,
    pub exported_by: BundleAuthor,
    pub sanitization: Sanitization,
    pub session_count: usize,
    pub sessions_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BundleAuthor {
    pub user: String,
    pub node: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Sanitization {
    Raw,
    Sanitized(SanitizeStamp),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SanitizeStamp {
    pub at: DateTime<Utc>,
    pub policy_sha256: String,
    pub passes: Vec<String>,
    pub unresolved_flags: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BundledSession {
    pub origin: SessionOrigin,
    pub batch: CaptureBatchRequest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionOrigin {
    pub author: String,
    pub node: String,
    pub conversation_id: String,
    pub session_key: String,
    pub agent: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CaptureResult {
    pub ok: bool,
    pub conversation_id: String,
    pub inserted: u64,
    pub skipped: u64,
}
