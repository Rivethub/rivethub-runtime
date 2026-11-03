use async_trait::async_trait;
use kernel::model::{LlmResponse, LlmResponseKind};
use kernel::queue::{
    ClaimSelector, ClaimedJob, Job, JobId, JobName, JobNotifications, JobQueue, JobQueueError,
    StoredJob, WorkerRole,
};
use memory_postgres::{PostgresMemory, PostgresQueue};
use parking_lot::Mutex;
use sqlx::PgPool;
use testkit::{FakeProvider, Script};
use uuid::Uuid;

pub(super) type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

pub(super) fn message_id() -> Uuid {
    Uuid::parse_str("00000000-0000-4000-8000-000000000002").expect("fixture id")
}

pub(super) async fn seed(pool: &PgPool) -> PostgresMemory {
    sqlx::raw_sql(include_str!("../../fixtures/memory_jobs/seed.txt"))
        .execute(pool)
        .await
        .expect("seed fixture");
    PostgresMemory::new(pool.clone())
}

pub(super) fn response(text: &str) -> FakeProvider {
    FakeProvider::new(vec![Script::Chat(LlmResponse {
        r#type: LlmResponseKind::Text,
        content: Some(text.to_owned()),
        tool_calls: None,
        usage: None,
        finish_reason: None,
    })])
}

#[derive(Debug)]
pub(super) struct RecordingQueue {
    pub(super) inner: PostgresQueue,
    pub(super) jobs: Mutex<Vec<Job>>,
}

impl RecordingQueue {
    pub(super) async fn new(pool: &PgPool) -> Self {
        Self {
            inner: PostgresQueue::connect(pool.clone()).await.expect("queue"),
            jobs: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl JobQueue for RecordingQueue {
    fn task_names(&self, role: WorkerRole) -> Vec<JobName> {
        self.inner.task_names(role)
    }
    async fn subscribe(&self) -> Result<Box<dyn JobNotifications>, JobQueueError> {
        self.inner.subscribe().await
    }
    async fn enqueue(&self, job: Job) -> Result<JobId, JobQueueError> {
        let id = self.inner.enqueue(job.clone()).await?;
        self.jobs.lock().push(job);
        Ok(id)
    }
    async fn claim(&self, selector: &ClaimSelector) -> Result<Option<ClaimedJob>, JobQueueError> {
        self.inner.claim(selector).await
    }
    async fn complete(&self, id: JobId) -> Result<(), JobQueueError> {
        self.inner.complete(id).await
    }
    async fn fail(&self, id: JobId, error: &str) -> Result<(), JobQueueError> {
        self.inner.fail(id, error).await
    }
    async fn release(&self, id: JobId) -> Result<(), JobQueueError> {
        self.inner.release(id).await
    }
    async fn flag_stuck(&self) -> Result<u64, JobQueueError> {
        self.inner.flag_stuck().await
    }
    async fn read(&self, id: JobId) -> Result<Option<StoredJob>, JobQueueError> {
        self.inner.read(id).await
    }
}
