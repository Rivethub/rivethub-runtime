use jiff::Timestamp;
use std::collections::HashMap;

use kernel::wiki::builder::{
    DeferredWiki, ExtractWiki, WikiBuilderStore, WikiRouting, WikiStoreError, WikiWork,
};
use kernel::wiki::page::WikiPatch;
use kernel::wiki::page::WikiSlug;
use tokio::sync::Mutex;
use uuid::Uuid;

#[derive(Default)]
pub(crate) struct WikiStore {
    work: Mutex<HashMap<Uuid, WikiWork>>,
}

#[async_trait::async_trait]
impl WikiBuilderStore for WikiStore {
    async fn wiki_work(&self, job: &ExtractWiki) -> Result<WikiWork, WikiStoreError> {
        self.work
            .lock()
            .await
            .get(&job.summary_id)
            .cloned()
            .ok_or(WikiStoreError::Missing(job.summary_id))
    }

    async fn wiki_backfill(
        &self,
        budget: kernel::wiki::builder::BackfillBudget,
    ) -> Result<Vec<ExtractWiki>, WikiStoreError> {
        Ok(self
            .work
            .lock()
            .await
            .values()
            .filter_map(|work| match work {
                WikiWork::Pending(summary) => Some(ExtractWiki {
                    summary_id: summary.id,
                    conversation_id: summary.conversation_id,
                }),
                WikiWork::Complete { .. } => None,
            })
            .take(usize::try_from(budget.idle).expect("test limit"))
            .collect())
    }

    async fn apply_wiki(
        &self,
        job: &ExtractWiki,
        patches: Vec<WikiPatch>,
        _routing: &WikiRouting,
        _now: Timestamp,
    ) -> Result<Vec<String>, WikiStoreError> {
        let topics: Vec<String> = patches
            .into_iter()
            .map(|patch| String::from(patch.slug))
            .collect();
        self.work.lock().await.insert(
            job.summary_id,
            WikiWork::Complete {
                topics: topics.clone(),
            },
        );
        Ok(topics)
    }

    async fn fail_wiki(
        &self,
        _summary_id: Uuid,
        _error: &str,
        _now: Timestamp,
    ) -> Result<(), WikiStoreError> {
        Ok(())
    }

    async fn defer_wiki(&self, _deferred: DeferredWiki) -> Result<(), WikiStoreError> {
        Ok(())
    }

    async fn unsettled_notes(&self, _limit: u32) -> Result<Vec<DeferredWiki>, WikiStoreError> {
        Ok(Vec::new())
    }

    async fn cite_wiki(
        &self,
        _summary_id: Uuid,
        _slug: &WikiSlug,
        _now: Timestamp,
    ) -> Result<(), WikiStoreError> {
        Ok(())
    }

    async fn uncite_wiki(&self, _summary_id: Uuid, _slug: &WikiSlug) -> Result<(), WikiStoreError> {
        Ok(())
    }
}
