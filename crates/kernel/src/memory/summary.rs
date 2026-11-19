use async_trait::async_trait;
use jiff::Timestamp;
use thiserror::Error;
use uuid::Uuid;
use values::Role;

use super::summary_opening::OpeningTitle;
use crate::BoxError;
use crate::queue::Job;

pub const HEARTBEAT_SESSION_PREFIX: &str = "heartbeat:";

use super::Provenance;
use super::SummaryKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationHead {
    pub id: Uuid,
    pub session_key: String,
    pub agent: String,
    pub channel: String,
    pub title: Option<String>,
    pub trashed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowMessage {
    pub id: Uuid,
    pub agent: String,
    pub role: Role,
    pub content: String,
    pub tool: Option<ToolUse>,
    pub created_at: Timestamp,
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
    pub earliest_at: Timestamp,
    pub latest_at: Timestamp,
    pub depth: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationWindow {
    pub conversation: ConversationHead,
    pub provenance: Option<Provenance>,
    pub messages: Vec<WindowMessage>,
    pub leaves: Vec<SummaryNode>,
    pub branches: Vec<SummaryNode>,
    pub latest_message_at: Option<Timestamp>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingConversation {
    pub id: Uuid,
    pub unsummarized: u32,
    pub latest_message_at: Option<Timestamp>,
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
    pub earliest_at: Timestamp,
    pub latest_at: Timestamp,
    pub model: String,
    pub pipeline_version: i32,
    pub created_at: Timestamp,
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
    #[error("unknown or unsupported summary kind {kind}")]
    Kind { kind: String },
    #[error("a summary count does not fit")]
    Count,
    #[error("summary checkpoint run {id} no longer owns revision {revision}")]
    StaleRun { id: Uuid, revision: i64 },
    #[error("the summary store is closed")]
    Closed,
}

#[async_trait]
pub trait SummaryStore: Send + Sync {
    fn summary_conversion(&self) -> Option<&dyn super::summary_conversion::SummaryConversion> {
        None
    }

    async fn record_summary_call(
        &self,
        _conversation: Uuid,
        _call: &crate::model_calls::ModelCall,
    ) -> Result<(), super::MemoryStoreError> {
        Ok(())
    }

    async fn summary_state(
        &self,
        _id: Uuid,
    ) -> Result<Option<super::settlement::SummaryState>, SummaryStoreError> {
        Ok(None)
    }

    async fn summary_eligible(&self, _id: Uuid, _revision: i64) -> Result<bool, SummaryStoreError> {
        Ok(false)
    }

    async fn expire_summary_page(
        &self,
        _after: Option<Uuid>,
        _cutoff: Timestamp,
        _now: Timestamp,
        _limit: u32,
    ) -> Result<super::settlement::SettlementPage, SummaryStoreError> {
        Ok(super::settlement::SettlementPage {
            next: None,
            settled: 0,
        })
    }

    #[doc(hidden)]
    async fn live_candidates(
        &self,
        _after: Uuid,
        _limit: i64,
    ) -> Result<Vec<(Uuid, i64)>, SummaryStoreError> {
        Ok(Vec::new())
    }

    #[doc(hidden)]
    async fn live_input(
        &self,
        _id: Uuid,
    ) -> Result<Option<super::summary_backend::SummaryInput>, SummaryStoreError> {
        Ok(None)
    }

    #[doc(hidden)]
    async fn live_run(
        &self,
        _id: Uuid,
    ) -> Result<Option<super::summary_backend::SummaryRun>, SummaryStoreError> {
        Err(SummaryStoreError::Closed)
    }

    #[doc(hidden)]
    async fn freeze_live_run(
        &self,
        _id: Uuid,
        _revision: i64,
        _root: Option<Uuid>,
        _manifest: super::summary_backend::SummaryArtifact,
    ) -> Result<Uuid, SummaryStoreError> {
        Err(SummaryStoreError::Closed)
    }

    #[doc(hidden)]
    async fn live_checkpoint(
        &self,
        _run: Uuid,
        _key: [u8; 32],
    ) -> Result<Option<super::summary_backend::SummaryArtifact>, SummaryStoreError> {
        Err(SummaryStoreError::Closed)
    }

    #[doc(hidden)]
    async fn save_live_checkpoint(
        &self,
        _run: Uuid,
        _revision: i64,
        _key: [u8; 32],
        _artifact: super::summary_backend::SummaryArtifact,
    ) -> Result<(), SummaryStoreError> {
        Err(SummaryStoreError::Closed)
    }

    #[doc(hidden)]
    async fn try_summary_permit(
        &self,
        _limit: u32,
    ) -> Result<Option<Box<dyn super::summary_backend::SummaryPermit>>, SummaryStoreError> {
        Ok(Some(Box::new(())))
    }

    #[doc(hidden)]
    async fn publish_live(
        &self,
        _tree: super::summary_reference::PreparedSummaryTree,
        _indexes: super::summary_backend::SummaryIndexes,
    ) -> Result<bool, SummaryStoreError> {
        Err(SummaryStoreError::Closed)
    }

    #[doc(hidden)]
    async fn fail_live(&self, _id: Uuid, _revision: i64) -> Result<(), SummaryStoreError> {
        Err(SummaryStoreError::Closed)
    }

    #[doc(hidden)]
    fn live_index_key(&self) -> Option<[u8; 32]> {
        None
    }

    #[doc(hidden)]
    async fn repair_candidates(&self, _limit: u32) -> Result<Vec<Uuid>, SummaryStoreError> {
        Ok(Vec::new())
    }

    #[doc(hidden)]
    async fn repair_input(
        &self,
        _id: Uuid,
    ) -> Result<Option<super::summary_backend::SummaryRepair>, SummaryStoreError> {
        Ok(None)
    }

    #[doc(hidden)]
    async fn repair_index(
        &self,
        _expected: super::summary_backend::SummaryRepair,
        _index: super::summary_backend::SummaryIndex,
    ) -> Result<(), SummaryStoreError> {
        Err(SummaryStoreError::Closed)
    }

    async fn conversation(&self, id: Uuid)
    -> Result<Option<ConversationWindow>, SummaryStoreError>;

    async fn pending(&self) -> Result<Vec<PendingConversation>, SummaryStoreError>;

    async fn write_summary(&self, summary: NewSummary) -> Result<SummaryWrite, SummaryStoreError>;

    async fn retitle(&self, id: Uuid, title: &str) -> Result<bool, SummaryStoreError>;

    async fn opening_title(&self, id: Uuid) -> Result<Option<OpeningTitle>, SummaryStoreError>;

    async fn finish_opening_title(
        &self,
        opening: &OpeningTitle,
        title: Option<&str>,
    ) -> Result<bool, SummaryStoreError>;

    async fn opening_filing(&self, id: Uuid) -> Result<Option<bool>, SummaryStoreError>;

    async fn finish_opening_filing(&self, id: Uuid) -> Result<(), SummaryStoreError>;

    async fn undelivered_openings(&self) -> Result<Vec<Job>, SummaryStoreError>;
}
