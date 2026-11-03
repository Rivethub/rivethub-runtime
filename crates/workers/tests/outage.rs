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

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use common::Phase;
use common::Queue;
use common::Store;
use common::embed_target;
use common::settings;
use common::target_payload;
use common::width;
use kernel::model::Embedding;
use kernel::model::LlmResponse;
use kernel::model::ModelCallFailure;
use kernel::model::ModelProvider;
use kernel::model::ModelProviderError;
use kernel::model::ModelRequest;
use kernel::model::ModelStreamItem;
use kernel::queue::FixedJob;
use kernel::queue::JobId;
use kernel::queue::JobName;
use parking_lot::Mutex;
use testkit::FakeProvider;
use tokio::sync::mpsc::Receiver;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use wire::JsonObject;
use workers::EmbedTable;
use workers::OutageCause;
use workers::ProcessRole;
use workers::Runner;
use workers::RunnerConfig;
use workers::SummarySettings;
use workers::WorkerDeps;
use workers::WorkerError;

const OUTAGE: &str = "stopped claiming";
const RECOVERY: &str = "resumed claiming";

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn an_unreachable_provider_stops_claiming_until_it_recovers() {
    let flaky = Arc::new(Flaky::new());
    let rig = rig(2, Arc::clone(&flaky) as Arc<dyn ModelProvider>);
    let (guard, lines) = warnings();
    let ids = seed(&rig, &["a", "b", "c", "d", "e", "f"]).await;
    let (cancel, run) = spawn(&rig);
    assert_held(&rig, &ids, 2).await;
    assert!(flaky.calls() > 2);
    assert_eq!(mentions(&lines, OUTAGE), 1);
    flaky.reach();
    finish_ids(&rig, &ids).await;
    assert_eq!(rig.queue.claims(), ids.len());
    assert_eq!(mentions(&lines, RECOVERY), 1);
    flaky.block();
    let more = seed(&rig, &["g", "h", "i", "j"]).await;
    assert_held(&rig, &more, 8).await;
    assert_eq!(mentions(&lines, OUTAGE), 2);
    flaky.reach();
    finish_ids(&rig, &more).await;
    assert_eq!(rig.queue.claims(), ids.len() + more.len());
    assert_eq!(mentions(&lines, RECOVERY), 2);
    drop(guard);
    finish(cancel, run).await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn provider_outages_hold_jobs_without_spending_attempts() {
    let cases = [
        (Fault::Status(401, None), OutageCause::Credentials),
        (Fault::Status(403, None), OutageCause::Credentials),
        (Fault::Credentials, OutageCause::Credentials),
        (Fault::Status(429, None), OutageCause::Throttled),
        (Fault::Status(500, None), OutageCause::ServerError),
        (Fault::Status(503, None), OutageCause::ServerError),
        (Fault::Unreachable, OutageCause::Unreachable),
    ];
    for (fault, cause) in cases {
        let flaky = Arc::new(Flaky::failing(fault));
        let rig = rig(1, Arc::clone(&flaky) as Arc<dyn ModelProvider>);
        let (guard, lines) = warnings();
        let ids = seed(&rig, &["a", "b", "c"]).await;
        let (cancel, run) = spawn(&rig);
        assert_held(&rig, &ids, 1).await;
        assert!(flaky.calls() >= 2, "{fault:?}");
        assert_eq!(
            mentions(&lines, &format!("is {cause}; the worker {OUTAGE}")),
            1
        );
        flaky.reach();
        finish_ids(&rig, &ids).await;
        assert_eq!(rig.queue.claims(), ids.len(), "{fault:?}");
        drop(guard);
        finish(cancel, run).await;
    }
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_retry_after_hint_paces_the_held_job() {
    let hint = Duration::from_secs(300);
    let flaky = Arc::new(Flaky::failing(Fault::Status(429, Some(hint))));
    let rig = rig(1, Arc::clone(&flaky) as Arc<dyn ModelProvider>);
    let ids = seed(&rig, &["r"]).await;
    let (cancel, run) = spawn(&rig);
    rig.queue.wait_claims(1).await;
    for _ in 0..8 {
        tokio::task::yield_now().await;
    }
    assert_eq!(flaky.calls(), 1);
    flaky.reach();
    for _ in 0..40 {
        tokio::time::advance(Duration::from_secs(10)).await;
        tokio::task::yield_now().await;
    }
    finish_ids(&rig, &ids).await;
    assert_eq!(flaky.calls(), 2);
    let gap = flaky.gap().expect("two calls");
    assert!(gap >= hint, "{gap:?}");
    assert!(gap < hint + Duration::from_secs(30), "{gap:?}");
    finish(cancel, run).await;
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn a_model_error_still_fails_the_job_and_claims_the_next() {
    let rig = rig(1, Arc::new(FakeProvider::http_errors(503)));
    let ids = seed(&rig, &["m", "n"]).await;
    let (cancel, run) = spawn(&rig);
    for id in &ids {
        rig.queue.wait_phase(*id, Phase::Backoff).await;
        let job = rig.queue.view(*id).await;
        assert_eq!(job.attempt, 1);
        assert!(job.error.expect("error").contains("503"));
    }
    assert_eq!(rig.queue.claims(), ids.len());
    finish(cancel, run).await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_bad_payload_fails_and_the_next_job_completes() {
    let rig = rig(1, Arc::new(FakeProvider::fnv_embeddings(width())));
    rig.store
        .insert(EmbedTable::Messages, "m", "hello", None)
        .await
        .expect("seed");
    let bad = rig
        .queue
        .push(fixed(FixedJob::EmbedTarget), JsonObject::default(), 1)
        .await;
    let good = rig
        .queue
        .push(
            fixed(FixedJob::EmbedTarget),
            target_payload("ros_messages", "m"),
            5,
        )
        .await;
    let (cancel, run) = spawn(&rig);
    rig.queue.wait_phase(bad, Phase::Dead).await;
    rig.queue.wait_phase(good, Phase::Done).await;
    assert_eq!(rig.queue.claims(), 2);
    assert!(
        rig.queue
            .view(bad)
            .await
            .error
            .expect("error")
            .contains("payload")
    );
    finish(cancel, run).await;
}

#[tokio::test]
async fn connection_refused_is_unreachable_and_http_status_is_not() {
    let store = Store::new();
    store
        .insert(EmbedTable::Messages, "m", "hello", None)
        .await
        .expect("seed");
    let payload = target_payload("ros_messages", "m");
    let cancel = CancellationToken::new();
    let error = embed_target(&store, &KindRefused, settings(), &payload, &cancel)
        .await
        .expect_err("refused");
    assert!(matches!(
        error,
        WorkerError::ProviderOutage {
            cause: OutageCause::Unreachable,
            retry_after: None
        }
    ));
    let provider = FakeProvider::http_errors(503);
    let error = embed_target(&store, &provider, settings(), &payload, &cancel)
        .await
        .expect_err("status");
    assert!(matches!(error, WorkerError::Embed(_)));
    assert!(error.to_string().contains("503"));
    assert!(!matches!(error, WorkerError::ProviderOutage { .. }));
}

async fn assert_held(rig: &Rig, ids: &[JobId], claims: usize) {
    rig.queue.wait_claims(claims).await;
    tokio::time::advance(Duration::from_secs(600)).await;
    tokio::task::yield_now().await;
    assert_eq!(rig.queue.claims(), claims);
    for id in ids {
        let job = rig.queue.view(*id).await;
        match job.phase {
            Phase::Running => assert_eq!(job.attempt, 1),
            Phase::Ready => assert_eq!(job.attempt, 0),
            Phase::Backoff | Phase::Dead | Phase::Done => panic!("settled during the outage"),
        }
    }
}

async fn finish_ids(rig: &Rig, ids: &[JobId]) {
    tokio::time::advance(Duration::from_secs(60)).await;
    tokio::task::yield_now().await;
    for id in ids {
        rig.queue.wait_phase(*id, Phase::Done).await;
        assert_eq!(rig.queue.view(*id).await.attempt, 1);
    }
}

async fn seed(rig: &Rig, names: &[&str]) -> Vec<JobId> {
    let mut ids = Vec::new();
    for name in names {
        rig.store
            .insert(EmbedTable::Messages, name, "hello", None)
            .await
            .expect("seed");
        ids.push(
            rig.queue
                .push(
                    fixed(FixedJob::EmbedTarget),
                    target_payload("ros_messages", name),
                    5,
                )
                .await,
        );
    }
    ids
}

struct Rig {
    queue: Arc<Queue>,
    store: Arc<Store>,
    runner: Arc<Runner<Queue>>,
}

fn rig(concurrency: u32, provider: Arc<dyn ModelProvider>) -> Rig {
    let queue = Arc::new(Queue::new());
    let store = Arc::new(Store::new());
    let runner = Arc::new(Runner::new(
        Arc::clone(&queue),
        RunnerConfig::new(ProcessRole::Embedder, concurrency).expect("concurrency"),
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
        provider,
        embed: settings(),
        summary: SummarySettings::standard("fake-chat").expect("summary settings"),
        scopes: None,
    }
}

struct IdleSummaries;

#[async_trait]
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

fn fixed(job: FixedJob) -> JobName {
    JobName::Fixed(job)
}

fn spawn(rig: &Rig) -> (CancellationToken, JoinHandle<Result<(), WorkerError>>) {
    let cancel = CancellationToken::new();
    let worker = Arc::clone(&rig.runner);
    let token = cancel.clone();
    let run = tokio::spawn(async move { worker.run(token).await });
    (cancel, run)
}

async fn finish(cancel: CancellationToken, run: JoinHandle<Result<(), WorkerError>>) {
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("runner stops")
        .expect("join")
        .expect("runner");
}

#[derive(Debug, Clone, Copy)]
enum Fault {
    Refused,
    Status(u16, Option<Duration>),
    Credentials,
    Unreachable,
}

impl Fault {
    fn error(self) -> ModelProviderError {
        let source = Box::new(std::io::Error::other("synthetic"));
        let failure = match self {
            Self::Refused => return Flaky::refused(),
            Self::Status(status, retry_after) => ModelCallFailure::Http {
                status,
                retry_after,
                source,
            },
            Self::Credentials => ModelCallFailure::Credentials { source },
            Self::Unreachable => ModelCallFailure::Unreachable { source },
        };
        ModelProviderError::Request {
            message: failure.to_string(),
            source: Box::new(failure),
        }
    }
}

struct Flaky {
    fault: Fault,
    reachable: AtomicBool,
    calls: AtomicUsize,
    instants: Mutex<Vec<tokio::time::Instant>>,
}

impl Flaky {
    fn new() -> Self {
        Self::failing(Fault::Refused)
    }

    fn failing(fault: Fault) -> Self {
        Self {
            fault,
            reachable: AtomicBool::new(false),
            calls: AtomicUsize::new(0),
            instants: Mutex::new(Vec::new()),
        }
    }

    fn gap(&self) -> Option<Duration> {
        let (first, second) = {
            let instants = self.instants.lock();
            (*instants.first()?, *instants.get(1)?)
        };
        Some(second.duration_since(first))
    }

    fn reach(&self) {
        self.reachable.store(true, Ordering::Relaxed);
    }

    fn block(&self) {
        self.reachable.store(false, Ordering::Relaxed);
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }

    fn refused() -> ModelProviderError {
        ModelProviderError::Request {
            message: "connection refused".to_owned(),
            source: Box::new(std::io::Error::other("connection refused")),
        }
    }
}

#[async_trait]
impl ModelProvider for Flaky {
    async fn chat(&self, _request: ModelRequest) -> Result<LlmResponse, ModelProviderError> {
        Err(Self::refused())
    }

    async fn stream(
        &self,
        _request: ModelRequest,
    ) -> Result<Receiver<ModelStreamItem>, ModelProviderError> {
        Err(Self::refused())
    }

    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Embedding>, ModelProviderError> {
        let _calls = self.calls.fetch_add(1, Ordering::Relaxed);
        self.instants.lock().push(tokio::time::Instant::now());
        if !self.reachable.load(Ordering::Relaxed) {
            return Err(self.fault.error());
        }
        let vector = vec![1.0_f32; width()];
        Ok(texts
            .into_iter()
            .map(|_| Embedding {
                vector: vector.clone(),
            })
            .collect())
    }

    async fn list_models(&self) -> Result<Vec<String>, ModelProviderError> {
        Ok(vec!["fake-chat".to_owned()])
    }
}

struct KindRefused;

#[async_trait]
impl ModelProvider for KindRefused {
    async fn chat(&self, _request: ModelRequest) -> Result<LlmResponse, ModelProviderError> {
        Err(kind_refused())
    }

    async fn stream(
        &self,
        _request: ModelRequest,
    ) -> Result<Receiver<ModelStreamItem>, ModelProviderError> {
        Err(kind_refused())
    }

    async fn embed(&self, _texts: Vec<String>) -> Result<Vec<Embedding>, ModelProviderError> {
        Err(kind_refused())
    }

    async fn list_models(&self) -> Result<Vec<String>, ModelProviderError> {
        Ok(Vec::new())
    }
}

fn kind_refused() -> ModelProviderError {
    ModelProviderError::Request {
        message: "the model server request failed".to_owned(),
        source: Box::new(std::io::Error::new(
            std::io::ErrorKind::ConnectionRefused,
            "tcp",
        )),
    }
}

struct WarningRecorder(Arc<Mutex<Vec<String>>>);

struct MessageVisitor(Arc<Mutex<Vec<String>>>);

impl tracing::field::Visit for MessageVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0.lock().push(format!("{value:?}"));
        }
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0.lock().push(value.to_owned());
        }
    }
}

impl<S> tracing_subscriber::Layer<S> for WarningRecorder
where
    S: tracing::Subscriber + for<'lookup> tracing_subscriber::registry::LookupSpan<'lookup>,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _context: tracing_subscriber::layer::Context<'_, S>,
    ) {
        event.record(&mut MessageVisitor(Arc::clone(&self.0)));
    }
}

fn warnings() -> (tracing::subscriber::DefaultGuard, Arc<Mutex<Vec<String>>>) {
    use tracing_subscriber::layer::SubscriberExt as _;
    let lines = Arc::new(Mutex::new(Vec::new()));
    let subscriber = tracing_subscriber::registry().with(WarningRecorder(Arc::clone(&lines)));
    (tracing::subscriber::set_default(subscriber), lines)
}

fn mentions(lines: &Mutex<Vec<String>>, needle: &str) -> usize {
    lines
        .lock()
        .iter()
        .filter(|line| line.contains(needle))
        .count()
}
