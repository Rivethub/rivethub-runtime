use std::sync::Arc;
use std::time::Duration;

use kernel::model::ModelProvider;
use kernel::queue::FixedJob;
use kernel::queue::JobName;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use workers::ProcessRole;
use workers::Runner;
use workers::RunnerConfig;
use workers::SummarySettings;
use workers::WorkerDeps;
use workers::WorkerError;

use super::Queue;
use super::Store;
use super::settings;

pub(crate) struct Rig {
    pub(crate) queue: Arc<Queue>,
    pub(crate) store: Arc<Store>,
    pub(crate) runner: Arc<Runner<Queue>>,
}

pub(crate) fn rig(role: ProcessRole, concurrency: u32, provider: Arc<dyn ModelProvider>) -> Rig {
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

pub(crate) fn deps(
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

pub(crate) struct IdleSummaries;

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

pub(crate) fn fixed(job: FixedJob) -> JobName {
    JobName::Fixed(job)
}

pub(crate) fn spawn(rig: &Rig) -> (CancellationToken, JoinHandle<Result<(), WorkerError>>) {
    let cancel = CancellationToken::new();
    let worker = Arc::clone(&rig.runner);
    let token = cancel.clone();
    let run = tokio::spawn(async move { worker.run(token).await });
    (cancel, run)
}

pub(crate) async fn finish(cancel: CancellationToken, run: JoinHandle<Result<(), WorkerError>>) {
    cancel.cancel();
    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .expect("runner stops")
        .expect("join")
        .expect("runner");
}
