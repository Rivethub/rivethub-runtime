use super::*;

impl MemStore {
    pub(super) fn new() -> Self {
        Self {
            openings: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl SummaryStore for MemStore {
    async fn conversation(&self, _: Uuid) -> Result<Option<ConversationWindow>, SummaryStoreError> {
        Ok(None)
    }
    async fn pending(&self) -> Result<Vec<PendingConversation>, SummaryStoreError> {
        Ok(Vec::new())
    }
    async fn write_summary(&self, _: NewSummary) -> Result<SummaryWrite, SummaryStoreError> {
        Err(SummaryStoreError::Closed)
    }

    async fn opening_title(&self, id: Uuid) -> Result<Option<OpeningTitle>, SummaryStoreError> {
        Ok(self
            .openings
            .lock()
            .await
            .iter()
            .find(|opening| opening.head.id == id)
            .cloned())
    }

    async fn finish_opening_title(
        &self,
        snapshot: &OpeningTitle,
        title: Option<&str>,
    ) -> Result<bool, SummaryStoreError> {
        let mut openings = self.openings.lock().await;
        let Some(opening) = openings.iter_mut().find(|opening| **opening == *snapshot) else {
            return Ok(false);
        };
        opening.armed = false;
        if let Some(title) = title {
            opening.head.title = Some(title.to_owned());
        }
        drop(openings);
        Ok(title.is_some())
    }

    async fn opening_filing(&self, _: Uuid) -> Result<Option<bool>, SummaryStoreError> {
        Ok(None)
    }

    async fn finish_opening_filing(&self, _: Uuid) -> Result<(), SummaryStoreError> {
        Ok(())
    }

    async fn undelivered_openings(&self) -> Result<Vec<Job>, SummaryStoreError> {
        Ok(Vec::new())
    }

    async fn retitle(&self, _: Uuid, _: &str) -> Result<bool, SummaryStoreError> {
        Ok(false)
    }
}
