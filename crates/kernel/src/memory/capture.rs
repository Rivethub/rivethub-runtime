use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use values::{JsonObject, Role};

use super::MemoryEntry;

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureBatchRequest {
    pub session_key: String,
    pub agent: String,
    pub channel: Option<String>,
    pub title: Option<String>,
    pub settings: Option<JsonObject>,
    pub task_id: Option<String>,
    pub finalize: Option<bool>,
    #[serde(default)]
    #[serde_as(as = "Option<values::Millis>")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    #[serde_as(as = "Option<values::Millis>")]
    pub updated_at: Option<DateTime<Utc>>,
    pub messages: Vec<CaptureMessage>,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureMessage {
    pub event_id: String,
    pub role: Role,
    pub content: String,
    pub tool_name: Option<String>,
    pub tool_args: Option<JsonObject>,
    pub tool_result: Option<String>,
    pub metadata: Option<JsonObject>,
    #[serde(default)]
    #[serde_as(as = "Option<values::Millis>")]
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureResult {
    pub ok: bool,
    pub conversation_id: String,
    pub inserted: u64,
    pub skipped: u64,
}

impl CaptureBatchRequest {
    #[must_use]
    pub fn entries(&self) -> Vec<MemoryEntry> {
        self.messages
            .iter()
            .map(|message| MemoryEntry {
                provenance: None,
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
