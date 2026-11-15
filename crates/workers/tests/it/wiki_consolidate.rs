use async_trait::async_trait;
use chrono::{DateTime, Utc};
use kernel::queue::{ClaimSelector, FixedJob, JobName, JobQueue};
use kernel::wiki::builder::WikiStoreError;
use kernel::wiki::consolidate::{ConsolidateWiki, WikiConsolidateStore, WikiFold, WikiTopicStats};
use testkit::fakes::FakeJobQueue;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use wire::JsonObject;
use workers::WorkerError;
use workers::wiki_builder::handle_consolidate_wiki;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Topics {
    topics: Vec<WikiTopicStats>,
    folds: Mutex<Vec<WikiFold>>,
}

impl Topics {
    fn new() -> Self {
        Self {
            topics: [
                "acmekit-deploys",
                "acmekit-deploys-friday",
                "staging-database",
            ]
            .iter()
            .map(|slug| WikiTopicStats {
                slug: (*slug).to_owned(),
                title: (*slug).to_owned(),
                citations: 0,
                history: 0,
                created_at: DateTime::<Utc>::UNIX_EPOCH,
                bound: false,
            })
            .collect(),
            folds: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl WikiConsolidateStore for Topics {
    async fn wiki_topic_stats(&self) -> Result<Vec<WikiTopicStats>, WikiStoreError> {
        Ok(self.topics.clone())
    }

    async fn fold_wiki(
        &self,
        fold: &WikiFold,
        _now: DateTime<Utc>,
    ) -> Result<Vec<String>, WikiStoreError> {
        self.folds.lock().await.push(fold.clone());
        Ok(fold.children.clone())
    }
}

fn payload(options: &ConsolidateWiki) -> Result<JsonObject, serde_json::Error> {
    JsonObject::from_serialize(options)
}

async fn embed_queued(queue: &FakeJobQueue) -> Result<bool, kernel::queue::JobQueueError> {
    let selector = ClaimSelector::matching(vec![JobName::Fixed(FixedJob::EmbedTarget)]);
    Ok(queue.claim(&selector).await?.is_some())
}

#[tokio::test]
async fn consolidation_folds_each_planned_cluster_and_reembeds_the_parent() -> TestResult {
    let store = Topics::new();
    let queue = FakeJobQueue::default();
    let removed = handle_consolidate_wiki(
        &store,
        &queue,
        &payload(&defaults()?)?,
        Utc::now(),
        &CancellationToken::new(),
    )
    .await?;
    assert_eq!(removed, 1);
    assert_eq!(
        *store.folds.lock().await,
        vec![WikiFold {
            cluster: "acmekit-deploys".to_owned(),
            parent: "acmekit-deploys".to_owned(),
            children: vec!["acmekit-deploys-friday".to_owned()],
            mark: kernel::wiki::page::FoldMark::Merged,
        }]
    );
    assert!(embed_queued(&queue).await?);
    Ok(())
}

#[tokio::test]
async fn a_dry_run_folds_nothing() -> TestResult {
    let store = Topics::new();
    let queue = FakeJobQueue::default();
    let removed = handle_consolidate_wiki(
        &store,
        &queue,
        &payload(&ConsolidateWiki {
            dry_run: true,
            ..defaults()?
        })?,
        Utc::now(),
        &CancellationToken::new(),
    )
    .await?;
    assert_eq!(removed, 0);
    assert_eq!(*store.folds.lock().await, Vec::<WikiFold>::new());
    assert!(!embed_queued(&queue).await?);
    Ok(())
}

#[tokio::test]
async fn an_unknown_option_is_refused() -> TestResult {
    let store = Topics::new();
    let queue = FakeJobQueue::default();
    let options: JsonObject = serde_json::from_str(r#"{"limit":3}"#)?;
    let refused = handle_consolidate_wiki(
        &store,
        &queue,
        &options,
        Utc::now(),
        &CancellationToken::new(),
    )
    .await;
    assert!(matches!(refused, Err(WorkerError::WikiJson(_))));
    assert_eq!(*store.folds.lock().await, Vec::<WikiFold>::new());
    Ok(())
}

fn defaults() -> Result<ConsolidateWiki, serde_json::Error> {
    serde_json::from_str("{}")
}
