#![cfg(test)]

use crate::wiki_support;

use std::sync::Arc;
use std::time::Duration;

use kernel::queue::{FixedJob, Job, JobKeyMode, JobName, JobQueue, WorkerRole};
use memory_postgres::{PostgresMemory, PostgresQueue};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use wiki_support::{TestResult, now, patch, provider, seed};
use wire::JsonObject;
use workers::wiki_builder::{
    WikiBuilderDeps, WikiBuilderEnv, extract_wiki_job, handle_extract_wiki,
};
use workers::{EmbedSettings, ProcessRole, Runner, RunnerConfig, SummarySettings, WorkerDeps};

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn wiki_runner_claims_cron_extracts_and_shuts_down(pool: PgPool) -> TestResult {
    testkit::graphile_schema::install_graphile_worker(&pool).await?;
    let queue = Arc::new(PostgresQueue::connect(pool.clone()).await?);
    let wiki = tempfile::tempdir()?;
    let store =
        Arc::new(PostgresMemory::new(pool.clone()).with_wiki_directory(wiki.path().to_path_buf()));
    let model = Arc::new(provider(&[patch()]));
    let source = seed(&pool).await?;
    wiki_support::filed(&store, &source).await?;
    queue
        .enqueue(Job {
            name: JobName::Fixed(FixedJob::ReapDeadJobs),
            payload: JsonObject::default(),
            key: None,
            key_mode: JobKeyMode::Replace,
            priority: 0,
            run_at: None,
            max_attempts: None,
        })
        .await?;
    queue
        .enqueue(Job {
            name: JobName::Fixed(FixedJob::EnqueueWikiBackfill),
            payload: JsonObject::default(),
            key: None,
            key_mode: JobKeyMode::Replace,
            priority: 10,
            run_at: None,
            max_attempts: None,
        })
        .await?;
    let runner = Runner::new(
        Arc::clone(&queue),
        RunnerConfig::new(ProcessRole::WikiBuilder, 2)?,
        WorkerDeps {
            targets: store.clone(),
            summaries: store.clone(),
            summarizer: model.clone(),
            categorizer: workers::RoleModel {
                provider: model.clone(),
                model: "fake-chat".to_owned(),
            },
            embedder: model.clone(),
            embed: EmbedSettings::standard(),
            summary: SummarySettings::standard("fake-chat")?,
            scopes: None,
        },
    )
    .with_wiki_builder(WikiBuilderDeps {
        store: store.clone(),
        consolidate: None,
        recompile: None,
        provider: model.clone(),
        model: "fake-chat".to_owned(),
        load: None,
    })
    .with_topic_pages(store.clone(), store.clone(), store.clone())
    .with_session_topics(workers::SessionTopics {
        store: store.clone(),
        filing: store,
        lineage: Arc::new(testkit::wiki_routes::NoParents),
    });
    let mut idle = runner.idle();
    let cancel = CancellationToken::new();
    let task_cancel = cancel.clone();
    let task = tokio::spawn(async move { runner.run(task_cancel).await });
    tokio::time::timeout(Duration::from_secs(10), idle.wait_for(|value| *value)).await??;
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(10), task).await???;
    let extraction: (String,) =
        sqlx::query_as("SELECT status FROM rh_wiki_extractions WHERE summary_id = $1")
            .bind(source.summary_id)
            .fetch_one(&pool)
            .await?;
    assert_eq!(extraction.0, "done");
    assert_eq!(model.chat_requests().await.len(), 1);
    assert!(
        queue
            .claim(&kernel::queue::ClaimSelector::matching(
                queue.task_names(WorkerRole::WikiBuilder)
            ))
            .await?
            .is_none()
    );
    let untouched: (i64,) = sqlx::query_as("SELECT count(*) FROM graphile_worker._private_jobs j JOIN graphile_worker._private_tasks t ON t.id = j.task_id WHERE t.identifier = 'reap-dead-jobs' AND j.attempts = 0")
        .fetch_one(&pool).await?;
    assert_eq!(untouched.0, 1);
    assert!(
        queue
            .claim(&kernel::queue::ClaimSelector::matching(
                queue.task_names(WorkerRole::Embedder)
            ))
            .await?
            .is_some()
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn wiki_queue_failure_retries_delivery_without_reextracting(pool: PgPool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool.clone()).with_wiki_directory(wiki.path().to_path_buf());
    let model = provider(&[patch()]);
    let cancel = CancellationToken::new();
    let source = seed(&pool).await?;
    wiki_support::filed(&store, &source).await?;
    let payload = extract_wiki_job(&source)?.payload;
    testkit::install_graphile_worker(&pool).await?;
    let absent = PostgresQueue::connect(pool.clone()).await?;
    sqlx::query("DROP SCHEMA graphile_worker CASCADE")
        .execute(&pool)
        .await?;
    let env = WikiBuilderEnv {
        store: &store,
        provider: &model,
        queue: &absent,
        model: "fake-chat",
        now: now(),
        cancel: &cancel,
        topics: Some(wiki_support::ports(&store)),
    };
    assert!(matches!(
        handle_extract_wiki(&env, &payload).await,
        Err(workers::WorkerError::Queue(_))
    ));
    testkit::graphile_schema::install_graphile_worker(&pool).await?;
    let queue = PostgresQueue::connect(pool.clone()).await?;
    let env = WikiBuilderEnv {
        queue: &queue,
        ..env
    };
    assert_eq!(
        handle_extract_wiki(&env, &payload).await?,
        vec!["acmekit-deploys"]
    );
    assert_eq!(model.chat_requests().await.len(), 1);
    assert!(
        queue
            .claim(&kernel::queue::ClaimSelector::matching(
                queue.task_names(WorkerRole::Embedder)
            ))
            .await?
            .is_some()
    );
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn wiki_cancels_an_inflight_provider_request(pool: PgPool) -> TestResult {
    let wiki = tempfile::tempdir()?;
    let store = PostgresMemory::new(pool.clone()).with_wiki_directory(wiki.path().to_path_buf());
    let job = seed(&pool).await?;
    let model = WaitingProvider {
        entered: tokio::sync::Notify::new(),
        other: testkit::fakes::FakeProvider::default(),
    };
    let queue = testkit::fakes::FakeJobQueue::default();
    let cancel = CancellationToken::new();
    let env = WikiBuilderEnv {
        store: &store,
        provider: &model,
        queue: &queue,
        model: "fake-chat",
        now: now(),
        cancel: &cancel,
        topics: Some(wiki_support::ports(&store)),
    };
    let payload = extract_wiki_job(&job)?.payload;
    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
        tokio::join!(handle_extract_wiki(&env, &payload), async {
            model.entered.notified().await;
            cancel.cancel();
        })
    })
    .await?;
    assert!(matches!(result, Err(workers::WorkerError::Cancelled)));
    let count: (i64,) = sqlx::query_as("SELECT count(*) FROM rh_wiki_extractions")
        .fetch_one(&pool)
        .await?;
    assert_eq!(count.0, 0);
    Ok(())
}

struct WaitingProvider {
    entered: tokio::sync::Notify,
    other: testkit::fakes::FakeProvider,
}

#[async_trait::async_trait]
impl kernel::model::ModelProvider for WaitingProvider {
    async fn chat(
        &self,
        _request: kernel::model::ModelRequest,
    ) -> Result<kernel::model::LlmResponse, kernel::model::ModelProviderError> {
        self.entered.notify_one();
        std::future::pending().await
    }

    async fn stream(
        &self,
        request: kernel::model::ModelRequest,
    ) -> Result<
        tokio::sync::mpsc::Receiver<kernel::model::ModelStreamItem>,
        kernel::model::ModelProviderError,
    > {
        self.other.stream(request).await
    }

    async fn embed(
        &self,
        texts: Vec<String>,
    ) -> Result<Vec<kernel::model::Embedding>, kernel::model::ModelProviderError> {
        self.other.embed(texts).await
    }

    async fn list_models(&self) -> Result<Vec<String>, kernel::model::ModelProviderError> {
        self.other.list_models().await
    }
}
