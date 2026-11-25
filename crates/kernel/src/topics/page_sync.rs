use jiff::Timestamp;
use serde::Deserialize;
use serde::Serialize;
use values::JsonObject;

use super::DecisionStore;
use super::Topic;
use super::TopicId;
use super::TopicKind;
use super::TopicPageError;
use super::TopicPageStore;
use super::TopicStore;
use super::page::PageSync;
use super::page::SweepOwners;
use crate::queue::FixedJob;
use crate::queue::Job;
use crate::queue::JobKeyMode;
use crate::queue::JobName;
use crate::queue::MAINTENANCE_PRIORITY;
use crate::wiki::builder::WikiStoreError;
use crate::wiki::page::FoldMark;
use crate::wiki::page::WikiSlug;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SyncTopicPage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub topic_id: Option<TopicId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fold: Vec<WikiSlug>,
}

impl SyncTopicPage {
    #[must_use]
    pub fn topic(id: TopicId) -> Self {
        Self {
            topic_id: Some(id),
            fold: Vec::new(),
        }
    }

    #[must_use]
    pub fn every_topic() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn after_create(topic: &Topic) -> Vec<Self> {
        vec![Self::topic(topic.id)]
    }

    #[must_use]
    pub fn after_update(before: &Topic, after: &Topic) -> Vec<Self> {
        let mut syncs = vec![Self::topic(after.id)];
        if let Some(left) = before.parent.filter(|left| after.parent != Some(*left)) {
            syncs.push(Self::topic(left));
        }
        syncs
    }

    #[must_use]
    pub fn after_merge(source: &Topic, into: &Topic, page: Option<WikiSlug>) -> Vec<Self> {
        let mut syncs = vec![Self {
            topic_id: Some(into.id),
            fold: page.into_iter().collect(),
        }];
        if let Some(left) = source
            .parent
            .filter(|left| *left != into.id && into.parent != Some(*left))
        {
            syncs.push(Self::topic(left));
        }
        syncs
    }

    #[must_use]
    pub fn after_delete(topic: &Topic) -> Vec<Self> {
        topic.parent.map(Self::topic).into_iter().collect()
    }

    pub fn job(&self) -> Result<Job, TopicPageError> {
        let key = match (self.topic_id, self.fold.is_empty()) {
            (Some(id), true) => Some(format!("sync-topic-page:{id}")),
            (Some(_), false) => None,
            (None, _) => Some("sync-topic-page:all".to_owned()),
        };
        Ok(Job {
            name: JobName::Fixed(FixedJob::SyncTopicPage),
            payload: JsonObject::from_serialize(self)
                .map_err(|source| TopicPageError::Payload(Box::new(source)))?,
            key,
            key_mode: JobKeyMode::Replace,
            priority: MAINTENANCE_PRIORITY,
            run_at: None,
            max_attempts: None,
        })
    }

    pub fn from_payload(payload: &JsonObject) -> Result<Self, TopicPageError> {
        payload
            .deserialize()
            .map_err(|source| TopicPageError::Payload(Box::new(source)))
    }
}

#[derive(Debug)]
pub struct SweepFailure {
    pub topic: TopicId,
    pub error: TopicPageError,
}

pub async fn sync_topic_page(
    topics: &dyn TopicStore,
    pages: &dyn TopicPageStore,
    decisions: &dyn DecisionStore,
    job: &SyncTopicPage,
    owners: SweepOwners<'_>,
    now: Timestamp,
) -> Result<Vec<SweepFailure>, TopicPageError> {
    let sync = PageSync { topics, pages, now };
    if let Some(id) = job.topic_id {
        sync.topic(id, &job.fold, decisions).await?;
        return Ok(Vec::new());
    }
    let mut failures = Vec::new();
    for listing in topics.list().await? {
        let topic = listing.topic;
        if !owners.includes(&topic) {
            continue;
        }
        if let Err(error) = swept(&sync, &topic, decisions).await {
            failures.push(SweepFailure {
                topic: topic.id,
                error,
            });
        }
    }
    Ok(failures)
}

async fn swept(
    sync: &PageSync<'_>,
    topic: &Topic,
    decisions: &dyn DecisionStore,
) -> Result<(), TopicPageError> {
    let owns_page = match topic.kind {
        TopicKind::Type => false,
        TopicKind::Entity => true,
        TopicKind::Work => {
            topic.parent.is_none() || sync.topics.page_slug(topic.id).await?.is_some()
        }
    };
    if owns_page {
        sync.topic(topic.id, &[], decisions).await?;
    }
    Ok(())
}

pub async fn fold_section_steps<P: TopicPageStore + ?Sized>(
    pages: &P,
    root: &Topic,
    topic: &Topic,
    child: &WikiSlug,
    into: &WikiSlug,
    now: Timestamp,
) -> Result<(), WikiStoreError> {
    let Some(before) = pages.topic_page(child).await? else {
        return Ok(());
    };
    if !before.text().is_empty() {
        let parent = pages
            .topic_page(into)
            .await?
            .ok_or_else(|| WikiStoreError::TopicMissing(into.as_ref().to_owned()))?;
        let mut after = parent.clone();
        let mut emptied = before.clone();
        after.take_text(&mut emptied, topic.id, topic.name.as_str());
        if after != parent {
            pages.save_topic_page(root.id, &after, now).await?;
        }
        pages.save_topic_page(topic.id, &emptied, now).await?;
    }
    pages.fold_page(child, into, FoldMark::Section, now).await
}
