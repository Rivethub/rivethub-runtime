use jiff::Timestamp;
use std::sync::Arc;

use kernel::flagged::FlaggedJobs;
use kernel::owner::Owner;
use kernel::tenancy::Scope;
use kernel::topics::DecisionStore;
use kernel::topics::SweepFailure;
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
    pub(crate) flagged: Arc<dyn FlaggedJobs>,
}

pub(crate) async fn handle_sync_topic_page(
    deps: &TopicPages,
    scopes: Option<&OwnerScopes>,
    payload: &JsonObject,
    cancel: &CancellationToken,
) -> Result<(), WorkerError> {
    let job = SyncTopicPage::from_payload(payload)?;
    let Some(scopes) = scopes else {
        let failures = sync(deps, &job, SweepOwners::All, cancel).await?;
        return retry_alone(deps.flagged.as_ref(), failures).await;
    };
    if let Some(id) = job.topic_id {
        let found = Scope::Maintenance
            .run(deps.topics.find(&TopicKey::Id(id)))
            .await?;
        let owner = found.and_then(|topic| topic.owner);
        let scope = scopes.of_topic_owner(owner.as_ref()).await?;
        return scope
            .run(sync(deps, &job, SweepOwners::All, cancel))
            .await
            .map(drop);
    }
    let mut stalled = None;
    for owner in topic_owners(deps.topics.as_ref()).await? {
        let swept = async {
            let scope = scopes.of_topic_owner(owner.as_ref()).await?;
            scope
                .run(sync(deps, &job, SweepOwners::Only(owner.as_ref()), cancel))
                .await
        }
        .await;
        match swept {
            Ok(failures) => retry_alone(deps.flagged.as_ref(), failures).await?,
            Err(WorkerError::Cancelled) => return Err(WorkerError::Cancelled),
            Err(error) => {
                tracing::warn!(
                    owner = owner.as_ref().map_or("", Owner::as_str),
                    %error,
                    "topic page sweep skipped an owner"
                );
                stalled.get_or_insert(error);
            }
        }
    }
    stalled.map_or(Ok(()), Err)
}

async fn retry_alone(
    flagged: &dyn FlaggedJobs,
    failures: Vec<SweepFailure>,
) -> Result<(), WorkerError> {
    for SweepFailure { topic, error } in failures {
        let job = SyncTopicPage::topic(topic).job()?;
        if Scope::Maintenance
            .run(flagged.queue_unless_flagged(&job))
            .await?
        {
            tracing::warn!(%topic, %error, "topic page sweep queued this topic's own sync");
        } else {
            tracing::warn!(%topic, %error, "topic page sweep left this flagged topic for review");
        }
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
) -> Result<Vec<SweepFailure>, WorkerError> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(WorkerError::Cancelled),
        result = sync_topic_page(
            deps.topics.as_ref(),
            deps.pages.as_ref(),
            deps.decisions.as_ref(),
            job,
            owners,
            Timestamp::now(),
        ) => {
            result.map_err(WorkerError::from)
        }
    }
}
