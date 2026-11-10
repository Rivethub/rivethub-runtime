use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use uuid::Uuid;

use super::MemoryStoreError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDigest {
    pub key: String,
    pub conversation_id: Uuid,
    pub title: Option<String>,
    pub agent: String,
    pub first: DateTime<Utc>,
    pub last: DateTime<Utc>,
    pub messages: u64,
    pub imported_from: Option<String>,
}

#[async_trait]
pub trait SessionArchive: Send + Sync {
    type Session: Send + 'static;

    async fn digests(&self, keys: &[String]) -> Result<Vec<SessionDigest>, MemoryStoreError>;

    async fn export(&self, ids: &[Uuid]) -> Result<Vec<Self::Session>, MemoryStoreError>;
}
