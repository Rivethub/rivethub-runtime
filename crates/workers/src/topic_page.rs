use std::sync::Arc;

use chrono::Utc;
use kernel::topics::DecisionStore;
use kernel::topics::SyncTopicPage;
use kernel::topics::TopicPageStore;
use kernel::topics::TopicStore;
use kernel::topics::sync_topic_page;
use tokio_util::sync::CancellationToken;
use wire::JsonObject;

use crate::WorkerError;

#[derive(Clone)]
pub(crate) struct TopicPages {
    pub(crate) topics: Arc<dyn TopicStore>,
    pub(crate) pages: Arc<dyn TopicPageStore>,
    pub(crate) decisions: Arc<dyn DecisionStore>,
}

pub(crate) async fn handle_sync_topic_page(
    deps: &TopicPages,
    payload: &JsonObject,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    let job = SyncTopicPage::from_payload(payload)?;
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(WorkerError::Cancelled),
        result = sync_topic_page(
            deps.topics.as_ref(),
            deps.pages.as_ref(),
            deps.decisions.as_ref(),
            &job,
            Utc::now(),
        ) => {
            result.map_err(WorkerError::from)
        }
    }
}
