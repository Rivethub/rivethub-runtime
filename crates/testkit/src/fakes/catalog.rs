use std::sync::Arc;

use async_trait::async_trait;
use kernel::memory::MemoryStoreError;
use kernel::memory::SessionCatalog;
use kernel::memory::SessionPage;
use kernel::memory::SessionQuery;
use kernel::memory::SessionRecord;
use kernel::memory::SessionView;
use wire::session_id::SessionId;

use super::FakeSessionDeletion;

#[derive(Debug)]
pub struct FakeSessionCatalog {
    records: Vec<SessionRecord>,
    marks: Arc<FakeSessionDeletion>,
}

impl FakeSessionCatalog {
    #[must_use]
    pub fn new(marks: Arc<FakeSessionDeletion>) -> Self {
        Self {
            records: Vec::new(),
            marks,
        }
    }

    #[must_use]
    pub fn with_record(mut self, record: SessionRecord) -> Self {
        self.records.push(record);
        self
    }

    fn marked(&self, record: &SessionRecord) -> SessionRecord {
        SessionRecord {
            kept: self.marks.is_kept(&record.id),
            trashed: self.marks.is_trashed(&record.id),
            ..record.clone()
        }
    }
}

#[async_trait]
impl SessionCatalog for FakeSessionCatalog {
    async fn list(&self, query: &SessionQuery) -> Result<SessionPage, MemoryStoreError> {
        let mut items: Vec<SessionRecord> = self
            .records
            .iter()
            .map(|record| self.marked(record))
            .filter(|record| match query.view {
                SessionView::All => !record.trashed,
                SessionView::Unfiled => !record.trashed && record.topic.is_none(),
                SessionView::Kept => !record.trashed && record.kept,
                SessionView::Trash => record.trashed,
            })
            .filter(|record| {
                query
                    .harness
                    .is_none_or(|harness| record.id.harness() == harness)
            })
            .filter(|record| !query.exclude.contains(&record.id.to_string()))
            .collect();
        items.sort_by(|left, right| {
            right
                .started_at
                .cmp(&left.started_at)
                .then_with(|| right.id.cmp(&left.id))
        });
        Ok(SessionPage { items, next: None })
    }

    async fn records(
        &self,
        sessions: &[SessionId],
    ) -> Result<Vec<SessionRecord>, MemoryStoreError> {
        Ok(self
            .records
            .iter()
            .filter(|record| sessions.contains(&record.id))
            .map(|record| self.marked(record))
            .collect())
    }
}
