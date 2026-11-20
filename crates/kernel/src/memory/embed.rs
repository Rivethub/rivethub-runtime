use async_trait::async_trait;
use thiserror::Error;

use crate::BoxError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::Display, strum::EnumString)]
pub enum EmbedTable {
    #[strum(serialize = "ros_messages")]
    Messages,
    #[strum(serialize = "ros_summaries")]
    Summaries,
    #[strum(serialize = "ros_wiki_topics")]
    WikiTopics,
    #[strum(serialize = "ros_message_chunks")]
    MessageChunks,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedTarget {
    pub table: EmbedTable,
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadedTarget {
    pub content: String,
    pub tool_result: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredChunk {
    pub index: u32,
    pub char_start: u32,
    pub char_end: u32,
    pub content: String,
    pub vector: Option<Vec<f32>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StoredEmbedding {
    pub target: EmbedTarget,
    pub vector: Vec<f32>,
    pub content_hash: String,
    pub chunks: Vec<StoredChunk>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbedModelMismatch {
    pub stored: String,
    pub configured: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, strum::Display)]
#[strum(serialize_all = "kebab-case")]
pub enum Unembeddable {
    MediaMarker,
    MediaPrefix,
    Base64DataUrl,
    Base64Png,
    Base64Jpeg,
    Base64Blob,
}

impl Unembeddable {
    #[must_use]
    pub fn error_text(self) -> String {
        format!("unembeddable: {self}")
    }
}

#[derive(Debug, Error)]
pub enum EmbeddingStoreError {
    #[error(transparent)]
    SummaryReference(#[from] super::summary_reference::SummaryReferenceError),
    #[error("embedding store query failed: {message}")]
    Query {
        message: String,
        #[source]
        source: BoxError,
    },
    #[error("embed target {id} is missing")]
    Missing { id: String },
    #[error("embed target id is not a uuid: {id}")]
    TargetId {
        id: String,
        #[source]
        source: uuid::Error,
    },
    #[error("embedding width is {got}, the column is {expected}")]
    Width { got: usize, expected: usize },
    #[error(
        "stored vectors were written by embedding model {stored}, the configured model is {configured}; run rh memory reembed"
    )]
    Model { stored: String, configured: String },
    #[error("embed chunks belong on a message, not {table}")]
    Chunks { table: EmbedTable },
    #[error("embed chunk index does not fit the table")]
    ChunkIndex,
    #[error("embed failure count does not fit")]
    FailureCount,
    #[error("the embedding store is closed")]
    Closed,
}

#[async_trait]
pub trait EmbeddingStore: Send + Sync {
    async fn load(&self, target: &EmbedTarget)
    -> Result<Option<LoadedTarget>, EmbeddingStoreError>;

    async fn write(&self, stored: StoredEmbedding) -> Result<(), EmbeddingStoreError>;

    async fn mark_unembeddable(
        &self,
        target: &EmbedTarget,
        reason: Unembeddable,
    ) -> Result<(), EmbeddingStoreError>;

    async fn record_null(
        &self,
        target: &EmbedTarget,
        error: &str,
    ) -> Result<u32, EmbeddingStoreError>;

    async fn mark_permanent(
        &self,
        target: &EmbedTarget,
        error: &str,
    ) -> Result<(), EmbeddingStoreError>;
}
