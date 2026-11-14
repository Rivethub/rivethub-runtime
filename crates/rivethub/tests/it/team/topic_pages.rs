use std::sync::Arc;
use std::time::Duration;

use kernel::owner::Owner;
use kernel::queue::JobQueue as _;
use kernel::tenancy::{Scope, TenancyStore as _};
use kernel::topics::{
    NewTopic, SyncTopicPage, Topic, TopicId, TopicKind, TopicName, TopicStore as _,
};
use memory_postgres::{PostgresMemory, PostgresTenancy};
use testkit::fakes::{FakeJobQueue, FakeProvider};
use tokio_util::sync::CancellationToken;
use workers::{EmbedSettings, OwnerScopes, ProcessRole, RoleModel, Runner, RunnerConfig};

use super::{ALEX, Hub, PHIL, TestResult};

struct Worker {
    store: Arc<PostgresMemory>,
    tenancy: Arc<PostgresTenancy>,
    _wiki: tempfile::TempDir,
}

impl Worker {
    async fn connect(hub: &Hub) -> Result<Self, Box<dyn std::error::Error>> {
        let wiki = tempfile::tempdir()?;
        let store = Arc::new(
            PostgresMemory::connect(&hub.url)
                .await?
                .with_wiki_directory(wiki.path().to_path_buf())
                .with_wiki_owner(Owner::new(PHIL.to_owned())?),
        );
        let tenancy = Arc::new(PostgresTenancy::new(store.pool().clone()));
        Ok(Self {
            store,
            tenancy,
            _wiki: wiki,
        })
    }

    async fn as_person(&self, person: &str) -> Result<Scope, Box<dyn std::error::Error>> {
        let owner = Owner::new(person.to_owned())?;
        Ok(Scope::Viewer(self.tenancy.viewer_of(&owner).await?))
    }

    async fn private_root(
        &self,
        person: &str,
        name: &str,
    ) -> Result<TopicId, Box<dyn std::error::Error>> {
        let topic = Topic::create(
            TopicId::random(),
            NewTopic {
                kind: TopicKind::Work,
                name: TopicName::new(name)?,
                aliases: Vec::new(),
                parent: None,
                directory: None,
                owner: Some(Owner::new(person.to_owned())?),
            },
            chrono::Utc::now(),
        )?;
        let made = self
            .as_person(person)
            .await?
            .run(self.store.create(topic))
            .await?;
        Ok(made.id)
    }

    async fn has_page(
        &self,
        person: &str,
        topic: TopicId,
    ) -> Result<bool, Box<dyn std::error::Error>> {
        let slug = self
            .as_person(person)
            .await?
            .run(self.store.page_slug(topic))
            .await?;
        Ok(slug.is_some())
    }

    async fn run(&self, jobs: &[SyncTopicPage]) -> TestResult {
        let queue = Arc::new(FakeJobQueue::default());
        let mut ids = Vec::new();
        for job in jobs {
            ids.push(queue.enqueue(job.job()?).await?);
        }
        let provider: Arc<dyn kernel::model::ModelProvider> =
            Arc::new(FakeProvider::new(Vec::new()));
        let runner = Runner::new(
            Arc::clone(&queue),
            RunnerConfig::new(ProcessRole::Summarizer, 1)?,
            workers::WorkerDeps {
                targets: self.store.clone(),
                summaries: self.store.clone(),
                summarizer: Arc::clone(&provider),
                categorizer: RoleModel {
                    provider: Arc::clone(&provider),
                    model: "unused".to_owned(),
                },
                embedder: provider,
                embed: EmbedSettings::standard(),
                summary: workers::SummarySettings::standard("unused")?,
                scopes: Some(OwnerScopes {
                    store: self.tenancy.clone(),
                    primary: Owner::new(PHIL.to_owned())?,
                }),
            },
        )
        .with_topic_pages(self.store.clone(), self.store.clone(), self.store.clone());
        let mut idle = runner.idle();
        let shutdown = CancellationToken::new();
        let cancel = shutdown.clone();
        let task = tokio::spawn(async move { runner.run(cancel).await });
        let settled = tokio::time::timeout(Duration::from_secs(20), async {
            idle.wait_for(|idle| *idle).await.map(|_| ())
        })
        .await;
        let mut left = Vec::new();
        for id in ids {
            left.extend(queue.read(id).await?);
        }
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(5), task).await???;
        assert!(left.is_empty(), "every page job completed: {left:?}");
        settled??;
        Ok(())
    }
}

#[tokio::test]
async fn a_sweep_makes_pages_for_every_owners_private_topics() -> TestResult {
    let hub = Hub::start().await?;
    let worker = Worker::connect(&hub).await?;
    let phils = worker.private_root(PHIL, "Phil garden plans").await?;
    let alexs = worker.private_root(ALEX, "Alex boat repairs").await?;
    worker.run(&[SyncTopicPage::every_topic()]).await?;
    assert!(
        worker.has_page(PHIL, phils).await?,
        "the primary person's page"
    );
    assert!(
        worker.has_page(ALEX, alexs).await?,
        "a second person's private topic gets its page too"
    );
    assert!(
        !worker.has_page(PHIL, alexs).await?,
        "the page stays private to its owner"
    );
    worker.store.pool().close().await;
    hub.stop().await
}

#[tokio::test]
async fn a_single_topic_sync_runs_as_the_topics_owner() -> TestResult {
    let hub = Hub::start().await?;
    let worker = Worker::connect(&hub).await?;
    let alexs = worker.private_root(ALEX, "Alex kitchen remodel").await?;
    worker.run(&[SyncTopicPage::topic(alexs)]).await?;
    assert!(
        worker.has_page(ALEX, alexs).await?,
        "a job naming another person's topic is not dropped as missing"
    );
    worker.store.pool().close().await;
    hub.stop().await
}

#[tokio::test]
async fn a_person_claims_only_their_own_wiki_scope() -> TestResult {
    let hub = Hub::start().await?;
    let pool = sqlx::PgPool::connect(&hub.url).await?;
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('rh.principal', $1, true)")
        .bind(ALEX)
        .execute(&mut *tx)
        .await?;
    let refused = sqlx::query(
        "INSERT INTO rh_wiki_scopes (scope, owner_user_id, visibility, key_prefix, directory)
         VALUES ($1, $1, 'private', 'x-', 'users/x')",
    )
    .bind(PHIL)
    .execute(&mut *tx)
    .await;
    assert!(
        refused.is_err_and(|error| error.to_string().contains("row-level security")),
        "a person cannot record a wiki scope in someone else's name"
    );
    tx.rollback().await?;
    pool.close().await;
    hub.stop().await
}
