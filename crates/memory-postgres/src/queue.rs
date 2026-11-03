use crate::error::Error;
use crate::graphile::GraphileQueue;
use async_trait::async_trait;
use kernel::queue::ClaimSelector;
use kernel::queue::ClaimedJob;
use kernel::queue::Job;
use kernel::queue::JobId;
use kernel::queue::JobName;
use kernel::queue::JobQueue;
use kernel::queue::JobQueueError;
use kernel::queue::WorkerRole;
use serde::Deserialize;
use serde::Serialize;
use sqlx::PgPool;

#[derive(Debug)]
pub struct PostgresQueue {
    inner: GraphileQueue,
}

pub use kernel::memory::schedule::CronEntry;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueHealth {
    pub task: JobName,
    pub pending: u64,
    pub running: u64,
    pub scheduled: u64,
    pub dead: u64,
    pub recent_dead: u64,
    pub oldest_pending_minutes: Option<u64>,
    pub last_error: Option<String>,
}

impl PostgresQueue {
    pub async fn connect(pool: PgPool) -> Result<Self, Error> {
        if GraphileQueue::schema_present(&pool).await? {
            Ok(Self {
                inner: GraphileQueue::new(pool),
            })
        } else {
            Err(Error::QueueAbsent)
        }
    }

    #[must_use]
    pub fn flagging_after(self, failures: u32) -> Self {
        Self {
            inner: self.inner.flagging_after(failures),
        }
    }

    #[must_use]
    pub fn claiming_at_most(self, priority: Option<i16>) -> Self {
        Self {
            inner: self.inner.claiming_at_most(priority),
        }
    }

    #[must_use]
    pub fn task_list(role: WorkerRole) -> &'static [JobName] {
        role.task_list()
    }

    #[must_use]
    pub fn crontab(role: WorkerRole) -> Vec<CronEntry> {
        role.crontab(false)
    }

    pub async fn dead_job_ids(&self, task: &JobName, limit: u32) -> Result<Vec<JobId>, Error> {
        self.inner.dead_job_ids(task, limit).await
    }

    pub async fn reschedule_dead(
        &self,
        task: &JobName,
        limit: u32,
        priority: i16,
    ) -> Result<u64, Error> {
        self.inner.reschedule_dead(task, limit, priority).await
    }

    pub async fn reap_dead_corpses(&self, allowlist: &[JobName], limit: u32) -> Result<u64, Error> {
        self.inner.reap_dead_corpses(allowlist, limit).await
    }

    pub async fn queue_health(&self, task: &JobName) -> Result<Option<QueueHealth>, Error> {
        self.inner.queue_health(task).await
    }
}

#[async_trait]
impl JobQueue for PostgresQueue {
    fn task_names(&self, role: WorkerRole) -> Vec<JobName> {
        Self::task_list(role).to_vec()
    }

    async fn subscribe(&self) -> Result<Box<dyn kernel::queue::JobNotifications>, JobQueueError> {
        self.inner.subscribe().await.map_err(JobQueueError::from)
    }

    async fn enqueue(&self, job: Job) -> Result<JobId, JobQueueError> {
        self.inner
            .enqueue(&job)
            .await
            .map_err(|source| JobQueueError::Unavailable(Box::new(source)))
    }

    async fn claim(&self, selector: &ClaimSelector) -> Result<Option<ClaimedJob>, JobQueueError> {
        self.inner
            .claim(&selector.names, selector.only_id)
            .await
            .map_err(JobQueueError::from)
    }

    async fn complete(&self, id: JobId) -> Result<(), JobQueueError> {
        self.inner
            .complete(id)
            .await
            .map(|_| ())
            .map_err(|source| JobQueueError::Unavailable(Box::new(source)))
    }

    async fn fail(&self, id: JobId, error: &str) -> Result<(), JobQueueError> {
        self.inner
            .fail(id, error)
            .await
            .map_err(|source| JobQueueError::Unavailable(Box::new(source)))
    }

    async fn release(&self, id: JobId) -> Result<(), JobQueueError> {
        released(id, self.inner.release(id, None).await)
    }

    async fn postpone(&self, id: JobId, delay: std::time::Duration) -> Result<(), JobQueueError> {
        released(id, self.inner.release(id, Some(delay)).await)
    }

    async fn flag_stuck(&self) -> Result<u64, JobQueueError> {
        self.inner.flag_stuck().await.map_err(JobQueueError::from)
    }

    async fn read(&self, id: JobId) -> Result<Option<kernel::queue::StoredJob>, JobQueueError> {
        self.inner.read(id).await
    }
}

fn released(id: JobId, result: Result<u64, crate::error::Error>) -> Result<(), JobQueueError> {
    match result {
        Ok(0) => Err(JobQueueError::Missing { id }),
        Ok(_) => Ok(()),
        Err(source) => Err(JobQueueError::Unavailable(Box::new(source))),
    }
}
