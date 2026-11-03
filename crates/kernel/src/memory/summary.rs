use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use thiserror::Error;
use uuid::Uuid;
use values::Role;

use crate::BoxError;

pub const HEARTBEAT_SESSION_PREFIX: &str = "heartbeat:";

use super::SummaryKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationHead {
    pub id: Uuid,
    pub session_key: String,
    pub agent: String,
    pub channel: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowMessage {
    pub id: Uuid,
    pub agent: String,
    pub role: Role,
    pub content: String,
    pub tool: Option<ToolUse>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolUse {
    pub input: String,
    pub result: String,
}

impl ToolUse {
    pub const INPUT_BYTES: usize = 600;
    pub const RESULT_BYTES: usize = 300;

    #[must_use]
    pub fn from_prefixes(input: &[u8], result: &[u8]) -> Option<Self> {
        let tool = Self {
            input: clipped(input, Self::INPUT_BYTES),
            result: clipped(result, Self::RESULT_BYTES),
        };
        (!tool.input.is_empty() || !tool.result.is_empty()).then_some(tool)
    }
}

fn clipped(prefix: &[u8], budget: usize) -> String {
    let head = prefix.get(..budget).unwrap_or(prefix);
    let valid: String = head.utf8_chunks().map(|chunk| chunk.valid()).collect();
    let text = valid.trim();
    if prefix.len() > budget && !text.is_empty() {
        format!("{text} …")
    } else {
        text.to_owned()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryNode {
    pub id: Uuid,
    pub kind: SummaryKind,
    pub content: String,
    pub message_count: u32,
    pub earliest_at: DateTime<Utc>,
    pub latest_at: DateTime<Utc>,
    pub depth: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationWindow {
    pub conversation: ConversationHead,
    pub messages: Vec<WindowMessage>,
    pub leaves: Vec<SummaryNode>,
    pub branches: Vec<SummaryNode>,
    pub latest_message_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingConversation {
    pub id: Uuid,
    pub unsummarized: u32,
    pub latest_message_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummarySource {
    pub message_id: Uuid,
    pub ordinal: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSummary {
    pub id: Uuid,
    pub conversation_id: Uuid,
    pub kind: SummaryKind,
    pub depth: i32,
    pub content: String,
    pub message_count: u32,
    pub earliest_at: DateTime<Utc>,
    pub latest_at: DateTime<Utc>,
    pub model: String,
    pub pipeline_version: i32,
    pub created_at: DateTime<Utc>,
    pub sources: Vec<SummarySource>,
    pub child_ids: Vec<Uuid>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrittenSummary {
    pub id: Uuid,
    pub session_key: String,
    pub kind: SummaryKind,
    pub depth: i32,
}

pub trait SummaryWatch: Send + Sync {
    fn written(&self) -> tokio::sync::broadcast::Receiver<WrittenSummary>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryWrite {
    FirstLeaf,
    Added,
}

#[derive(Debug, Error)]
pub enum SummaryStoreError {
    #[error("summary store query failed: {message}")]
    Query {
        message: String,
        #[source]
        source: BoxError,
    },
    #[error("summary {id} is missing")]
    Missing { id: String },
    #[error("stored summary kind {kind} is not leaf, branch, or root")]
    Kind { kind: String },
    #[error("a summary count does not fit")]
    Count,
    #[error("the summary store is closed")]
    Closed,
}

#[async_trait]
pub trait SummaryStore: Send + Sync {
    async fn conversation(&self, id: Uuid)
    -> Result<Option<ConversationWindow>, SummaryStoreError>;

    async fn pending(&self) -> Result<Vec<PendingConversation>, SummaryStoreError>;

    async fn write_summary(&self, summary: NewSummary) -> Result<SummaryWrite, SummaryStoreError>;

    async fn retitle(&self, id: Uuid, title: &str) -> Result<bool, SummaryStoreError>;
}
