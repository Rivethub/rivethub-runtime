use async_trait::async_trait;
use jiff::Timestamp;
use tokio_util::sync::CancellationToken;

use super::MemoryStoreError;

#[async_trait]
pub trait MemoryExport: Send + Sync {
    async fn export(
        &self,
        since: Option<Timestamp>,
        cancellation: CancellationToken,
    ) -> Result<Vec<u8>, MemoryStoreError>;
}
