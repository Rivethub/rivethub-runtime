#![cfg(test)]

#[expect(
    unreachable_pub,
    reason = "the harness is a private module of each test binary"
)]
#[expect(
    dead_code,
    reason = "each test binary uses a different subset of the harness"
)]
mod common;

#[path = "common/wiki.rs"]
mod wiki;

use std::sync::Arc;
use std::time::Duration;

use common::Phase;
use common::Queue;
use common::Store;
use common::settings;
use common::target_payload;
use common::wait_at_least;
use common::wait_bool;
use common::width;
use kernel::model::ModelProvider;
use kernel::queue::FixedJob;
use kernel::queue::JobName;
use kernel::queue::WorkerRole;
use testkit::FakeProvider;
use testkit::Script;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use wire::JsonObject;
use workers::EMBED_CONCURRENCY;
use workers::EmbedTable;
use workers::POLL_INTERVAL;
use workers::ProcessRole;
use workers::Runner;
use workers::RunnerConfig;
use workers::SummarySettings;
use workers::WorkerDeps;
use workers::WorkerError;
use workers::jobs_for;

#[test]
fn roles_match_the_kernel_task_lists() {
    assert_eq!(
        names(WorkerRole::Embedder),
        vec![
            "embed-target".to_owned(),
            "enqueue-unembedded".to_owned(),
            "enqueue-unchunked".to_owned()
        ]
    );
    assert_eq!(
        names(WorkerRole::Summarizer),
        vec![
            "compact-conversation".to_owned(),
            "synthesize-tool-call".to_owned(),
            "enqueue-idle".to_owned(),
            "enqueue-stale-compaction".to_owned(),
            "reap-dead-jobs".to_owned(),
            "sync-topic-page".to_owned(),
            "purge-deleted-sessions".to_owned(),
            "digest-day".to_owned(),
        ]
    );
    assert_eq!(
        names(WorkerRole::Categorizer),
        vec!["file-conversation".to_owned(), "propose-tidy".to_owned()]
    );
    assert_eq!(
        names(WorkerRole::WikiBuilder),
        vec![
            "extract-wiki".to_owned(),
            "enqueue-wiki-backfill".to_owned(),
            "consolidate-wiki".to_owned(),
            "recompile-wiki".to_owned(),
            "enqueue-stale-wiki".to_owned(),
        ]
    );
    assert!(accepts(&fixed(FixedJob::EmbedTarget), WorkerRole::Embedder));
    assert!(!accepts(
        &fixed(FixedJob::SummarizeConversation),
        WorkerRole::Embedder
    ));
    assert!(accepts(
        &fixed(FixedJob::ReapDeadJobs),
        WorkerRole::Summarizer
    ));
    assert!(!accepts(
        &fixed(FixedJob::ExtractWiki),
        WorkerRole::Categorizer
    ));
}

#[test]
fn unknown_roles_list_the_enum_and_zero_concurrency_is_rejected() {
    assert_eq!(
        ProcessRole::parse("embedder").expect("role"),
        ProcessRole::Embedder
    );
    assert_eq!(
        ProcessRole::parse("wiki-builder").expect("role"),
        ProcessRole::WikiBuilder
    );
    assert_eq!(ProcessRole::parse("all").expect("role"), ProcessRole::All);
    let error = ProcessRole::parse("wiki_builder").expect_err("unknown");
    assert_eq!(
        error.to_string(),
        "unknown worker role: wiki_builder; expected embedder, summarizer, categorizer, wiki-builder, entity-builder, all"
    );
    assert!(matches!(
        RunnerConfig::new(ProcessRole::Embedder, 0),
        Err(WorkerError::Concurrency)
    ));
    assert_eq!(POLL_INTERVAL, Duration::from_secs(60));
    assert_eq!(EMBED_CONCURRENCY, 4);
    assert_eq!(RunnerConfig::embedder().concurrency, EMBED_CONCURRENCY);
    assert_eq!(RunnerConfig::embedder().poll_interval, POLL_INTERVAL);
}

#[tokio::test(flavor = "current_thread")]
async fn missing_wiki_dependencies_refuse_to_start() {
    let summarizer = ProcessRole::Summarizer
        .claim_roles()
        .expect("the summarizer has a handler");
    assert_eq!(summarizer, vec![WorkerRole::Summarizer]);
    let wiki = rig(ProcessRole::WikiBuilder, 1, fnv_provider());
    let error = wiki
        .runner
        .run(CancellationToken::new())
        .await
        .expect_err("unavailable");
    assert!(matches!(
        error,
        WorkerError::MissingDependencies {
            role: ProcessRole::WikiBuilder
        }
    ));
    assert_eq!(wiki.queue.claims(), 0);
    let categorizer = rig(ProcessRole::Categorizer, 1, fnv_provider());
    let (cancel, run) = spawn(&categorizer);
    tokio::task::yield_now().await;
    assert_eq!(categorizer.queue.claims(), 0);
    finish(cancel, run).await;
}

#[tokio::test(flavor = "current_thread")]
async fn all_refuses_to_start_when_wiki_dependencies_are_missing() {
    let rig = rig(ProcessRole::All, 1, fnv_provider());
    let error = rig
        .runner
        .run(CancellationToken::new())
        .await
        .expect_err("missing wiki dependencies");
    assert!(matches!(
        error,
        WorkerError::MissingDependencies {
            role: ProcessRole::WikiBuilder
        }
    ));
    assert_eq!(rig.queue.claims(), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn an_unhandled_claim_is_failed() {
    let rig = rig(ProcessRole::Embedder, 1, fnv_provider());
    let id = rig
        .queue
        .push_any(
            JobName::Heartbeat {
                agent: "edge".into(),
                index: 0,
            },
            JsonObject::default(),
            1,
        )
        .await;
    let (cancel, run) = spawn(&rig);
    rig.queue.wait_phase(id, Phase::Dead).await;
    let job = rig.queue.view(id).await;
    assert_eq!(
        job.error.as_deref(),
        Some("no handler for job heartbeat-edge-0")
    );
    assert_ne!(job.phase, Phase::Done);
    finish(cancel, run).await;
}

#[tokio::test(flavor = "current_thread")]
async fn concurrency_stays_at_the_configured_limit() {
    let queue = Arc::new(Queue::new());
    let store = Arc::new(Store::holding());
    let runner = Arc::new(Runner::new(
        Arc::clone(&queue),
        RunnerConfig::new(ProcessRole::Embedder, 2).expect("concurrency"),
        deps(
            Arc::clone(&store) as Arc<dyn kernel::memory::EmbeddingStore>,
            fnv_provider(),
        ),
    ));
    let mut ids = Vec::new();
    for name in ["a", "b", "c"] {
        store
            .insert(EmbedTable::Messages, name, "hello", None)
            .await
            .expect("seed");
        ids.push(
            queue
                .push(
                    fixed(FixedJob::EmbedTarget),
                    target_payload("ros_messages", name),
                    5,
                )
                .await,
        );
    }
    let mut loads = store.loads();
    let cancel = CancellationToken::new();
    let worker = Arc::clone(&runner);
    let token = cancel.clone();
    let run = tokio::spawn(async move { worker.run(token).await });
    wait_at_least(&mut loads, 2).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(store.load_count(), 2);
    store.release();
    for id in ids {
        queue.wait_phase(id, Phase::Done).await;
    }
    assert_eq!(store.load_count(), 3);
    assert_eq!(store.writes().await, 3);
    finish(cancel, run).await;
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_releases_every_in_flight_embed() {
    let queue = Arc::new(Queue::new());
    let store = Arc::new(Store::new());
    let provider = Arc::new(common::HangEmbed::new());
    for name in ["m", "n"] {
        store
            .insert(EmbedTable::Messages, name, "hello", None)
            .await
            .expect("seed");
    }
    let runner = Arc::new(Runner::new(
        Arc::clone(&queue),
        RunnerConfig::new(ProcessRole::Embedder, 2).expect("concurrency"),
        deps(
            Arc::clone(&store) as Arc<dyn kernel::memory::EmbeddingStore>,
            Arc::clone(&provider) as Arc<dyn ModelProvider>,
        ),
    ));
    let mut ids = Vec::new();
    for name in ["m", "n"] {
        ids.push(
            queue
                .push(
                    fixed(FixedJob::EmbedTarget),
                    target_payload("ros_messages", name),
                    5,
                )
                .await,
        );
    }
    let cancel = CancellationToken::new();
    let worker = Arc::clone(&runner);
    let token = cancel.clone();
    let run = tokio::spawn(async move { worker.run(token).await });
    queue.wait_claims(2).await;
    cancel.cancel();
    run.await.expect("join").expect("runner");
    for id in ids {
        let job = queue.view(id).await;
        assert_eq!(job.phase, Phase::Ready);
        assert_eq!(job.attempt, 0);
        assert_eq!(job.error, None);
    }
    assert_eq!(store.writes().await, 0);
    assert_eq!(store.failures(EmbedTable::Messages, "m").await, 0);
    assert_eq!(store.failures(EmbedTable::Messages, "n").await, 0);
}

#[tokio::test(flavor = "current_thread")]
async fn a_claim_landing_after_shutdown_is_released() {
    for schedules in [false, true] {
        let queue = Arc::new(Queue::new());
        let store = Arc::new(Store::new());
        store
            .insert(EmbedTable::Messages, "m", "hello", None)
            .await
            .expect("seed");
        let mut runner = Runner::new(
            Arc::clone(&queue),
            RunnerConfig::new(ProcessRole::Embedder, 1).expect("concurrency"),
            deps(
                Arc::clone(&store) as Arc<dyn kernel::memory::EmbeddingStore>,
                Arc::new(common::HangEmbed::new()),
            ),
        );
        if schedules {
            runner = runner.with_schedules(false);
        }
        queue.hold_claims();
        let id = queue
            .push(
                fixed(FixedJob::EmbedTarget),
                target_payload("ros_messages", "m"),
                5,
            )
            .await;
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let run = tokio::spawn(async move { runner.run(token).await });
        queue.wait_held(1).await;
        cancel.cancel();
        tokio::task::yield_now().await;
        queue.open_claims();
        finish(cancel, run).await;
        let job = queue.view(id).await;
        assert_eq!(job.phase, Phase::Ready, "schedules {schedules}");
        assert_eq!(job.attempt, 0, "schedules {schedules}");
        assert_eq!(job.error, None, "schedules {schedules}");
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_database_outage_keeps_the_runner_alive() {
    let rig = rig(ProcessRole::Embedder, 1, fnv_provider());
    rig.queue.fail_next_claim().await;
    let (cancel, run) = spawn(&rig);
    rig.queue.wait_claims(1).await;
    assert!(!run.is_finished());
    finish(cancel, run).await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_failure_backs_off_without_another_claim() {
    let provider =
        Arc::new(FakeProvider::new(vec![Script::EmbedHttpStatus(500)])) as Arc<dyn ModelProvider>;
    let rig = rig(ProcessRole::Embedder, 1, provider);
    rig.store
        .insert(EmbedTable::Messages, "m", "hello", None)
        .await
        .expect("seed");
    let backed = rig
        .queue
        .push(
            fixed(FixedJob::EmbedTarget),
            target_payload("ros_messages", "m"),
            5,
        )
        .await;
    let (cancel, run) = spawn(&rig);
    rig.queue.wait_phase(backed, Phase::Backoff).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    let job = rig.queue.view(backed).await;
    assert_eq!(job.attempt, 1);
    assert_eq!(rig.queue.claims(), 1);
    assert!(job.error.as_deref().expect("error").contains("500"));
    finish(cancel, run).await;
}

#[tokio::test(flavor = "current_thread")]
async fn the_last_attempt_is_dead_and_not_completed() {
    let provider =
        Arc::new(FakeProvider::new(vec![Script::EmbedHttpStatus(500)])) as Arc<dyn ModelProvider>;
    let rig = rig(ProcessRole::Embedder, 1, provider);
    rig.store
        .insert(EmbedTable::Messages, "m", "hello", None)
        .await
        .expect("seed");
    let dead = rig
        .queue
        .push(
            fixed(FixedJob::EmbedTarget),
            target_payload("ros_messages", "m"),
            1,
        )
        .await;
    let (cancel, run) = spawn(&rig);
    rig.queue.wait_phase(dead, Phase::Dead).await;
    let job = rig.queue.view(dead).await;
    assert_eq!(job.attempt, 1);
    assert_ne!(job.phase, Phase::Done);
    finish(cancel, run).await;
}

#[tokio::test(flavor = "current_thread")]
async fn queue_notification_claims_without_waiting_out_the_poll() {
    let rig = rig(ProcessRole::Embedder, 1, fnv_provider());
    let mut idle = rig.runner.idle();
    let (cancel, run) = spawn(&rig);
    wait_bool(&mut idle, true).await;
    let id = rig
        .queue
        .push(
            fixed(FixedJob::EmbedTarget),
            target_payload("ros_messages", "m"),
            5,
        )
        .await;
    rig.queue.wait_phase(id, Phase::Done).await;
    assert_eq!(rig.queue.claims(), 1);
    finish(cancel, run).await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_parked_runner_waits_out_the_poll_interval() {
    let rig = rig(ProcessRole::Embedder, 1, fnv_provider());
    let mut idle = rig.runner.idle();
    let (cancel, run) = spawn(&rig);
    wait_bool(&mut idle, true).await;
    let id = rig
        .queue
        .push_without_notification(
            fixed(FixedJob::EmbedTarget),
            target_payload("ros_messages", "m"),
            5,
        )
        .await;
    tokio::time::advance(Duration::from_secs(59)).await;
    tokio::task::yield_now().await;
    assert_eq!(rig.queue.claims(), 0);
    assert_eq!(rig.queue.view(id).await.phase, Phase::Ready);
    tokio::time::advance(Duration::from_secs(1)).await;
    rig.queue.wait_phase(id, Phase::Done).await;
    finish(cancel, run).await;
}

struct Rig {
    queue: Arc<Queue>,
    store: Arc<Store>,
    runner: Arc<Runner<Queue>>,
}

fn rig(role: ProcessRole, concurrency: u32, provider: Arc<dyn ModelProvider>) -> Rig {
    let queue = Arc::new(Queue::new());
    let store = Arc::new(Store::new());
    let runner = Arc::new(Runner::new(
        Arc::clone(&queue),
        RunnerConfig::new(role, concurrency).expect("concurrency"),
        deps(
            Arc::clone(&store) as Arc<dyn kernel::memory::EmbeddingStore>,
            provider,
        ),
    ));
    Rig {
        queue,
        store,
        runner,
    }
}

fn deps(
    targets: Arc<dyn kernel::memory::EmbeddingStore>,
    provider: Arc<dyn ModelProvider>,
) -> WorkerDeps {
    WorkerDeps {
        targets,
        summaries: Arc::new(IdleSummaries),
        summarizer: provider.clone(),
        categorizer: workers::RoleModel {
            provider: provider.clone(),
            model: "fake-chat".to_owned(),
        },
        embedder: provider,
        embed: settings(),
        summary: SummarySettings::standard("fake-chat").expect("summary settings"),
        scopes: None,
    }
}

struct IdleSummaries;

#[async_trait::async_trait]
impl kernel::memory::SummaryStore for IdleSummaries {
    async fn conversation(
        &self,
        _id: uuid::Uuid,
    ) -> Result<Option<kernel::memory::ConversationWindow>, kernel::memory::SummaryStoreError> {
        Err(kernel::memory::SummaryStoreError::Closed)
    }

    async fn pending(
        &self,
    ) -> Result<Vec<kernel::memory::PendingConversation>, kernel::memory::SummaryStoreError> {
        Err(kernel::memory::SummaryStoreError::Closed)
    }

    async fn write_summary(
        &self,
        _summary: kernel::memory::NewSummary,
    ) -> Result<kernel::memory::SummaryWrite, kernel::memory::SummaryStoreError> {
        Err(kernel::memory::SummaryStoreError::Closed)
    }

    async fn retitle(
        &self,
        _id: uuid::Uuid,
        _title: &str,
    ) -> Result<bool, kernel::memory::SummaryStoreError> {
        Err(kernel::memory::SummaryStoreError::Closed)
    }
}

fn fnv_provider() -> Arc<dyn ModelProvider> {
    Arc::new(FakeProvider::fnv_embeddings(width()))
}

fn fixed(job: FixedJob) -> JobName {
    JobName::Fixed(job)
}

fn names(role: WorkerRole) -> Vec<String> {
    jobs_for(role).iter().map(ToString::to_string).collect()
}

fn accepts(name: &JobName, role: WorkerRole) -> bool {
    workers::accepts(role, name)
}

fn spawn(rig: &Rig) -> (CancellationToken, JoinHandle<Result<(), WorkerError>>) {
    let cancel = CancellationToken::new();
    let worker = Arc::clone(&rig.runner);
    let token = cancel.clone();
    let run = tokio::spawn(async move { worker.run(token).await });
    (cancel, run)
}

struct IdleEntities;

#[async_trait::async_trait]
impl kernel::wiki::entity_pass::EntityStore for IdleEntities {
    async fn entity_work(
        &self,
        _job: &kernel::wiki::entity_pass::ExtractEntities,
    ) -> Result<kernel::wiki::entity_pass::EntityWork, kernel::wiki::entity_pass::EntityStoreError>
    {
        Err(kernel::wiki::entity_pass::EntityStoreError::DirectoryMissing)
    }

    async fn following(
        &self,
        _lead: &kernel::wiki::entity_pass::EntityLeaf,
        _limit: u32,
    ) -> Result<
        Vec<kernel::wiki::entity_pass::FollowingLeaf>,
        kernel::wiki::entity_pass::EntityStoreError,
    > {
        Err(kernel::wiki::entity_pass::EntityStoreError::DirectoryMissing)
    }

    async fn observe_context(
        &self,
        _leaves: &[kernel::wiki::entity_pass::EntityLeaf],
    ) -> Result<
        kernel::wiki::entity_pass::ObserveContext,
        kernel::wiki::entity_pass::EntityStoreError,
    > {
        Err(kernel::wiki::entity_pass::EntityStoreError::DirectoryMissing)
    }

    async fn transcript(
        &self,
        _leaves: &[kernel::wiki::entity_pass::EntityLeaf],
    ) -> Result<
        Vec<kernel::wiki::entity_pass::TranscriptMessage>,
        kernel::wiki::entity_pass::EntityStoreError,
    > {
        Err(kernel::wiki::entity_pass::EntityStoreError::DirectoryMissing)
    }

    async fn commit(
        &self,
        _leaves: &[kernel::wiki::entity_pass::EntityLeaf],
        _prepared: &kernel::wiki::entity_pass::PreparedEntity,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<
        Option<kernel::wiki::entity_pass::EntityView>,
        kernel::wiki::entity_pass::EntityStoreError,
    > {
        Err(kernel::wiki::entity_pass::EntityStoreError::DirectoryMissing)
    }

    async fn publish(
        &self,
        _view: &kernel::wiki::entity_pass::EntityView,
        _prose: &kernel::wiki::entity_pass::EntityProse,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<
        kernel::wiki::entity_pass::PublishOutcome,
        kernel::wiki::entity_pass::EntityStoreError,
    > {
        Err(kernel::wiki::entity_pass::EntityStoreError::DirectoryMissing)
    }

    async fn finish_entities(
        &self,
        _leaves: &[uuid::Uuid],
        _slugs: &[String],
        _git_sha: Option<&str>,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), kernel::wiki::entity_pass::EntityStoreError> {
        Err(kernel::wiki::entity_pass::EntityStoreError::DirectoryMissing)
    }

    async fn fail_entity(
        &self,
        _summary_id: uuid::Uuid,
        _error: &str,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), kernel::wiki::entity_pass::EntityStoreError> {
        Err(kernel::wiki::entity_pass::EntityStoreError::DirectoryMissing)
    }

    async fn entity_backfill(
        &self,
        _budget: kernel::wiki::builder::BackfillBudget,
    ) -> Result<
        Vec<kernel::wiki::entity_pass::ExtractEntities>,
        kernel::wiki::entity_pass::EntityStoreError,
    > {
        Ok(Vec::new())
    }

    async fn stale_entities(
        &self,
        _limit: u32,
    ) -> Result<
        Vec<kernel::wiki::entity_pass::ExtractEntities>,
        kernel::wiki::entity_pass::EntityStoreError,
    > {
        Ok(Vec::new())
    }

    async fn revive_dead_entities(
        &self,
        _limit: u32,
    ) -> Result<u64, kernel::wiki::entity_pass::EntityStoreError> {
        Ok(0)
    }

    async fn match_candidates(
        &self,
        _query: &kernel::wiki::entity_pass::CandidateQuery,
    ) -> Result<
        Vec<kernel::wiki::entity_pass::MatchCandidate>,
        kernel::wiki::entity_pass::EntityStoreError,
    > {
        Ok(Vec::new())
    }

    async fn remember_embedding(
        &self,
        _entity: uuid::Uuid,
        _embedding: &[f32],
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), kernel::wiki::entity_pass::EntityStoreError> {
        Ok(())
    }

    async fn hold_candidate(
        &self,
        _summary: uuid::Uuid,
        _prepared: &kernel::wiki::entity_pass::PreparedEntity,
        _confidence: f32,
        _reason: &str,
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), kernel::wiki::entity_pass::EntityStoreError> {
        Ok(())
    }

    async fn lease_entity(
        &self,
        _id: uuid::Uuid,
    ) -> Result<
        Box<dyn kernel::wiki::entity_pass::EntityLease>,
        kernel::wiki::entity_pass::EntityStoreError,
    > {
        Err(kernel::wiki::entity_pass::EntityStoreError::DirectoryMissing)
    }
}

async fn finish(cancel: CancellationToken, run: JoinHandle<Result<(), WorkerError>>) {
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("runner stops")
        .expect("join")
        .expect("runner");
}

#[tokio::test(flavor = "current_thread")]
async fn all_claims_every_role_that_has_a_handler() {
    let mut rig = rig(ProcessRole::All, 1, fnv_provider());
    let Ok(runner) = Arc::try_unwrap(rig.runner) else {
        panic!("unstarted runner is owned");
    };
    rig.runner = Arc::new(
        runner
            .with_wiki_builder(workers::wiki_builder::WikiBuilderDeps {
                store: Arc::new(wiki::WikiStore::default()),
                consolidate: None,
                recompile: None,
                provider: fnv_provider(),
                model: "wiki-model".to_owned(),
                load: None,
            })
            .with_entity_builder(workers::entity_builder::EntityBuilderDeps {
                store: Arc::new(IdleEntities),
                provider: fnv_provider(),
                model: "entity-model".to_owned(),
                settings: workers::entity_builder::MatcherSettings::default(),
            }),
    );
    rig.store
        .insert(EmbedTable::Messages, "m", "hello", None)
        .await
        .expect("seed");
    let embed = rig
        .queue
        .push(
            fixed(FixedJob::EmbedTarget),
            target_payload("ros_messages", "m"),
            5,
        )
        .await;
    let summarize = rig
        .queue
        .push(
            fixed(FixedJob::SummarizeConversation),
            JsonObject::default(),
            5,
        )
        .await;
    let tags = rig
        .queue
        .push(fixed(FixedJob::FileConversation), JsonObject::default(), 2)
        .await;
    let wiki_job = rig
        .queue
        .push(
            fixed(FixedJob::EnqueueWikiBackfill),
            JsonObject::default(),
            2,
        )
        .await;
    let (cancel, run) = spawn(&rig);
    rig.queue.wait_phase(wiki_job, Phase::Done).await;
    rig.queue.wait_phase(embed, Phase::Done).await;
    rig.queue.wait_phase(summarize, Phase::Backoff).await;
    rig.queue.wait_phase(tags, Phase::Backoff).await;
    let failed = rig.queue.view(summarize).await;
    assert_eq!(
        failed.error.as_deref(),
        Some("job payload is not a summarizer job: missing field `conversationId`")
    );
    let tagged = rig.queue.view(tags).await;
    assert_eq!(
        tagged.error.as_deref(),
        Some("job payload is not a file-conversation job: missing field `conversationId`")
    );
    assert_eq!(rig.queue.claims(), 4);
    finish(cancel, run).await;
}

#[tokio::test(flavor = "current_thread")]
async fn embed_jobs_use_the_embedder_provider_and_not_the_summarizer() {
    let embedder = Arc::new(FakeProvider::fnv_embeddings(width()));
    let summarizer = Arc::new(FakeProvider::fnv_embeddings(width()));
    let queue = Arc::new(Queue::new());
    let store = Arc::new(Store::new());
    let runner = Arc::new(Runner::new(
        Arc::clone(&queue),
        RunnerConfig::new(ProcessRole::Embedder, 1).expect("concurrency"),
        WorkerDeps {
            targets: Arc::clone(&store) as Arc<dyn kernel::memory::EmbeddingStore>,
            summaries: Arc::new(IdleSummaries),
            summarizer: summarizer.clone(),
            categorizer: workers::RoleModel {
                provider: summarizer.clone(),
                model: "categorizer-model".to_owned(),
            },
            embedder: embedder.clone(),
            embed: settings(),
            summary: SummarySettings::standard("summarizer-model").expect("summary settings"),
            scopes: None,
        },
    ));
    let rig = Rig {
        queue,
        store,
        runner,
    };
    rig.store
        .insert(EmbedTable::Messages, "m", "hello", None)
        .await
        .expect("seed");
    let embed = rig
        .queue
        .push(
            fixed(FixedJob::EmbedTarget),
            target_payload("ros_messages", "m"),
            5,
        )
        .await;
    let (cancel, run) = spawn(&rig);
    rig.queue.wait_phase(embed, Phase::Done).await;
    finish(cancel, run).await;
    assert_eq!(embedder.embed_requests().await.len(), 1);
    assert_eq!(summarizer.embed_requests().await.len(), 0);
}
