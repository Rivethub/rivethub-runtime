use std::sync::Arc;

use chrono::Utc;
use kernel::owner::Owner;
use kernel::tenancy::Scope;
use kernel::topics::DecisionStore;
use kernel::topics::SweepOwners;
use kernel::topics::SyncTopicPage;
use kernel::topics::TopicKey;
use kernel::topics::TopicPageStore;
use kernel::topics::TopicStore;
use kernel::topics::sync_topic_page;
use std::collections::BTreeSet;

use crate::scopes::OwnerScopes;
use tokio_util::sync::CancellationToken;
use wire::JsonObject;

use crate::WorkerError;

#[derive(Clone)]
pub(crate) struct TopicPages {
    pub(crate) topics: Arc<dyn TopicStore>,
    pub(crate) pages: Arc<dyn TopicPageStore>,
    pub(crate) decisions: Arc<dyn DecisionStore>,
    pub(crate) facts: Arc<dyn kernel::facts::FactStore>,
}

pub(crate) async fn handle_sync_topic_page(
    deps: &TopicPages,
    scopes: Option<&OwnerScopes>,
    payload: &JsonObject,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    let job = SyncTopicPage::from_payload(payload)?;
    let Some(scopes) = scopes else {
        return sync(deps, &job, SweepOwners::All, cancel).await;
    };
    if let Some(id) = job.topic_id {
        let found = Scope::Maintenance
            .run(deps.topics.find(&TopicKey::Id(id)))
            .await?;
        let owner = found.and_then(|topic| topic.owner);
        let scope = scopes.of_topic_owner(owner.as_ref()).await?;
        return scope.run(sync(deps, &job, SweepOwners::All, cancel)).await;
    }
    for owner in topic_owners(deps.topics.as_ref()).await? {
        let scope = scopes.of_topic_owner(owner.as_ref()).await?;
        scope
            .run(sync(deps, &job, SweepOwners::Only(owner.as_ref()), cancel))
            .await?;
    }
    Ok(())
}

pub(crate) async fn topic_owners(
    topics: &dyn TopicStore,
) -> Result<BTreeSet<Option<Owner>>, WorkerError> {
    Ok(Scope::Maintenance
        .run(topics.list())
        .await?
        .into_iter()
        .map(|listing| listing.topic.owner)
        .collect())
}

async fn sync(
    deps: &TopicPages,
    job: &SyncTopicPage,
    owners: SweepOwners<'_>,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(WorkerError::Cancelled),
        result = sync_topic_page(
            deps.topics.as_ref(),
            deps.pages.as_ref(),
            deps.decisions.as_ref(),
            job,
            owners,
            Utc::now(),
        ) => {
            result.map_err(WorkerError::from)
        }
    }
}
