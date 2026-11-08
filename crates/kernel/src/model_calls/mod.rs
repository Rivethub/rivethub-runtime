mod headers;
mod totals;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use values::{CallOutcome, CallUsage, SessionId};

use crate::memory::MemoryStoreError;
use crate::owner::Owner;

pub use headers::{Correlated, CredentialHeader, ProxyHeaders, TurnMetadata, correlate};
pub use totals::CallTotals;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelCall {
    pub call_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub provider: String,
    pub model_requested: Option<String>,
    pub model_served: Option<String>,
    pub account: Option<String>,
    pub status_code: Option<u16>,
    pub outcome: CallOutcome,
    pub retry_after_secs: Option<u64>,
    pub usage: Option<CallUsage>,
    pub session: Option<SessionId>,
    pub turn: Option<TurnMetadata>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RecordedCalls {
    pub inserted: u64,
    pub duplicate: u64,
}

#[async_trait]
pub trait ModelCallStore: Send + Sync {
    async fn record(
        &self,
        owner: &Owner,
        calls: &[ModelCall],
    ) -> Result<RecordedCalls, MemoryStoreError>;

    async fn session_calls(&self, session: &SessionId) -> Result<Vec<ModelCall>, MemoryStoreError>;
}
