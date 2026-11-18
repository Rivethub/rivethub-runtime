use async_trait::async_trait;
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;
use values::JsonObject;

use super::{EmbedTable, EmbedTarget, StoredChunk};
use crate::BoxError;
use crate::wiki::builder::ExtractWiki;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthesizeToolCall {
    pub message_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallMessage {
    pub id: Uuid,
    pub tool_name: Option<String>,
    pub tool_args: JsonObject,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnchunkedMessage {
    pub id: Uuid,
    pub content: String,
    pub tool_result: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingWiki {
    pub job: ExtractWiki,
    pub summarized_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkWrite {
    Written,
    Skipped,
}

#[derive(Debug, Error)]
pub enum MemoryJobError {
    #[error("memory job store failed: {0}")]
    Query(#[source] BoxError),
    #[error("invalid tool arguments: {0}")]
    Arguments(#[source] BoxError),
    #[error("optional embedding table {table} is missing: {source}")]
    MissingTable {
        table: EmbedTable,
        #[source]
        source: BoxError,
    },
}

#[async_trait]
pub trait MemoryJobStore: Send + Sync {
    async fn tool_call(&self, id: Uuid) -> Result<Option<ToolCallMessage>, MemoryJobError>;

    async fn write_tool_sentence(
        &self,
        id: Uuid,
        previous_content: &str,
        content: &str,
    ) -> Result<(), MemoryJobError>;

    async fn unchunked(
        &self,
        limit: u32,
        chars_per_chunk: u32,
        tool_result_cap: u32,
    ) -> Result<Vec<UnchunkedMessage>, MemoryJobError>;

    async fn write_chunks(
        &self,
        message_id: Uuid,
        chunks: Vec<StoredChunk>,
    ) -> Result<ChunkWrite, MemoryJobError>;

    async fn unembedded_chunks(&self, message_id: Uuid)
    -> Result<Vec<EmbedTarget>, MemoryJobError>;

    async fn revive_dead_wiki(&self, limit: u32) -> Result<u64, MemoryJobError>;

    async fn missing_wiki(&self, limit: u32) -> Result<Vec<MissingWiki>, MemoryJobError>;
}

#[async_trait]
pub trait UnembeddedStore: Send + Sync {
    async fn unembedded(
        &self,
        table: EmbedTable,
        limit: u32,
    ) -> Result<Vec<EmbedTarget>, MemoryJobError>;
}
