use jiff::Timestamp;
use std::sync::Arc;
use std::time::Duration;

use kernel::owner::Owner;
use kernel::queue::JobQueue;
use kernel::tenancy::{Scope, TenancyStore as _};
use kernel::topics::{
    NewTopic, SLUG_ATTEMPTS, SyncTopicPage, Topic, TopicId, TopicKind, TopicName, TopicStore as _,
};
use memory_postgres::{PostgresMemory, PostgresTenancy};
use testkit::fakes::{FakeJobQueue, FakeProvider};
use tokio_util::sync::CancellationToken;
use workers::{EmbedSettings, OwnerScopes, ProcessRole, RoleModel, Runner, RunnerConfig};

use super::{DANA, Hub, INES, TestResult};

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
                .with_wiki_owner(Owner::new(DANA.to_owned())?),
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
            Timestamp::now(),
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
        self.drain(&queue, ids).await
    }

    async fn drain<Q: JobQueue + 'static>(
        &self,
        queue: &Arc<Q>,
        ids: Vec<kernel::queue::JobId>,
    ) -> TestResult {
        let provider: Arc<dyn kernel::model::ModelProvider> =
            Arc::new(FakeProvider::new(Vec::new()));
        let runner = Runner::new(
            Arc::clone(queue),
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
                    primary: Owner::new(DANA.to_owned())?,
                }),
            },
        )
        .with_topic_pages(
            self.store.clone(),
            self.store.clone(),
            self.store.clone(),
            self.store.clone(),
        );
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
    let danas = worker.private_root(DANA, "Dana garden plans").await?;
    let iness = worker.private_root(INES, "Ines boat repairs").await?;
    worker.run(&[SyncTopicPage::every_topic()]).await?;
    assert!(
        worker.has_page(DANA, danas).await?,
        "the primary person's page"
    );
    assert!(
        worker.has_page(INES, iness).await?,
        "a second person's private topic gets its page too"
    );
    assert!(
        !worker.has_page(DANA, iness).await?,
        "the page stays private to its owner"
    );
    worker.store.pool().close().await;
    hub.stop().await
}

#[tokio::test]
async fn a_sweep_queues_a_failing_topic_on_its_own_and_syncs_the_rest() -> TestResult {
    let hub = Hub::start().await?;
    let worker = Worker::connect(&hub).await?;
    let danas = worker.private_root(DANA, "Dana garden plans").await?;
    let blocked = worker.private_root(INES, "Ines boat repairs").await?;
    let iness = worker.private_root(INES, "Ines zoo trip").await?;
    let pool = sqlx::PgPool::connect(&hub.url).await?;
    every_slug_held(&pool, &[blocked]).await?;
    let queue = Arc::new(worker.store.queue().await?);
    let sweep = queue.enqueue(SyncTopicPage::every_topic().job()?).await?;
    worker.drain(&queue, vec![sweep]).await?;
    assert!(
        own_sync_job(&pool, blocked).await?,
        "the failing topic has a sync job of its own"
    );
    assert!(worker.has_page(DANA, danas).await?, "dana's page");
    assert!(
        worker.has_page(INES, iness).await?,
        "the owner's other topic is synced past the failing one"
    );
    pool.close().await;
    worker.store.pool().close().await;
    hub.stop().await
}

#[tokio::test]
async fn a_sweep_leaves_a_flagged_topic_with_its_flag() -> TestResult {
    let hub = Hub::start().await?;
    let worker = Worker::connect(&hub).await?;
    let fresh = worker.private_root(INES, "Ines boat repairs").await?;
    let open = worker.private_root(INES, "Ines kite build").await?;
    let dismissed = worker.private_root(INES, "Ines tent patch").await?;
    let pool = sqlx::PgPool::connect(&hub.url).await?;
    every_slug_held(&pool, &[fresh, open, dismissed]).await?;
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('rh.maintenance', 'on', true)")
        .execute(&mut *tx)
        .await?;
    for (topic, dismissed_at) in [(open, None), (dismissed, Some(FLAGGED_AT))] {
        sqlx::query(
            "INSERT INTO rh_flagged_jobs (task, job_key, payload, priority, max_attempts, failures,
                 last_error, first_failed_at, last_failed_at, flagged_at, dismissed_at)
             VALUES ('sync-topic-page', $1, '{}', 0, 5, 5, 'slug held', $2::timestamptz,
                 $2::timestamptz, $2::timestamptz, $3::timestamptz)",
        )
        .bind(format!("sync-topic-page:{topic}"))
        .bind(FLAGGED_AT)
        .bind(dismissed_at)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    let queue = Arc::new(worker.store.queue().await?);
    let sweep = queue.enqueue(SyncTopicPage::every_topic().job()?).await?;
    worker.drain(&queue, vec![sweep]).await?;
    assert!(
        own_sync_job(&pool, fresh).await?,
        "a failing topic with no flag gets its own sync job"
    );
    for topic in [open, dismissed] {
        assert!(
            !own_sync_job(&pool, topic).await?,
            "a flagged topic is not queued again"
        );
    }
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('rh.maintenance', 'on', true)")
        .execute(&mut *tx)
        .await?;
    let flags: Vec<(String, bool, bool)> = sqlx::query_as(
        "SELECT job_key, dismissed_at IS NOT NULL, last_failed_at = $1::timestamptz
           FROM rh_flagged_jobs ORDER BY job_key",
    )
    .bind(FLAGGED_AT)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut expected = vec![
        (format!("sync-topic-page:{open}"), false, true),
        (format!("sync-topic-page:{dismissed}"), true, true),
    ];
    expected.sort();
    assert_eq!(flags, expected, "the flags are as the person left them");
    pool.close().await;
    worker.store.pool().close().await;
    hub.stop().await
}

const FLAGGED_AT: &str = "2026-11-01T00:00:00Z";

async fn every_slug_held(pool: &sqlx::PgPool, topics: &[TopicId]) -> TestResult {
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT set_config('rh.maintenance', 'on', true)")
        .execute(&mut *tx)
        .await?;
    for topic in topics {
        let held = sqlx::query(
            "INSERT INTO rh_wiki_topics (slug, title, owner_user_id)
             SELECT CASE WHEN n = 1 THEN slug ELSE slug || '-' || n END, name, owner_user_id
               FROM rh_topics, generate_series(1, $2) AS n WHERE id = $1",
        )
        .bind(topic.uuid())
        .bind(i32::try_from(SLUG_ATTEMPTS)?)
        .execute(&mut *tx)
        .await?;
        assert_eq!(
            held.rows_affected(),
            u64::from(SLUG_ATTEMPTS),
            "other pages hold every page slug the sweep can try"
        );
    }
    sqlx::query("DELETE FROM graphile_worker._private_jobs")
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

async fn own_sync_job(pool: &sqlx::PgPool, topic: TopicId) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM graphile_worker._private_jobs WHERE key = $1)")
        .bind(format!("sync-topic-page:{topic}"))
        .fetch_one(pool)
        .await
}

#[tokio::test]
async fn a_single_topic_sync_runs_as_the_topics_owner() -> TestResult {
    let hub = Hub::start().await?;
    let worker = Worker::connect(&hub).await?;
    let iness = worker.private_root(INES, "Ines kitchen remodel").await?;
    worker.run(&[SyncTopicPage::topic(iness)]).await?;
    assert!(
        worker.has_page(INES, iness).await?,
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
        .bind(INES)
        .execute(&mut *tx)
        .await?;
    let refused = sqlx::query(
        "INSERT INTO rh_wiki_scopes (scope, owner_user_id, visibility, key_prefix, directory)
         VALUES ($1, $1, 'private', 'x-', 'users/x')",
    )
    .bind(DANA)
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
