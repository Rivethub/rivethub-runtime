use std::sync::Arc;

use async_trait::async_trait;
use kernel::memory::MemoryStoreError;
use kernel::memory::SessionCatalog;
use kernel::memory::SessionPage;
use kernel::memory::SessionQuery;
use kernel::memory::SessionRecord;
use parking_lot::Mutex;
use wire::session_id::SessionId;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Calls {
    pub(super) lists: usize,
    pub(super) records: Vec<Vec<SessionId>>,
}

pub(super) struct Lookups {
    inner: Arc<dyn SessionCatalog>,
    calls: Mutex<Calls>,
}

impl Lookups {
    pub(super) fn new(inner: Arc<dyn SessionCatalog>) -> Self {
        Self {
            inner,
            calls: Mutex::new(Calls::default()),
        }
    }

    pub(super) fn take(&self) -> Calls {
        std::mem::take(&mut *self.calls.lock())
    }
}

#[async_trait]
impl SessionCatalog for Lookups {
    async fn list(&self, query: &SessionQuery) -> Result<SessionPage, MemoryStoreError> {
        self.calls.lock().lists += 1;
        self.inner.list(query).await
    }

    async fn records(
        &self,
        sessions: &[SessionId],
    ) -> Result<Vec<SessionRecord>, MemoryStoreError> {
        self.calls.lock().records.push(sessions.to_vec());
        self.inner.records(sessions).await
    }
}
