use async_trait::async_trait;
use serde::Deserialize;
use serde::Serialize;

use super::FixedJob;
use super::JobName;
use super::JobQueueError;

pub const MAINTENANCE_TASKS: [FixedJob; 5] = [
    FixedJob::ExtractWiki,
    FixedJob::SummarizeConversation,
    FixedJob::EmbedTarget,
    FixedJob::SynthesizeToolCall,
    FixedJob::FileConversation,
];

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

#[must_use]
pub fn empty_health(task: JobName) -> QueueHealth {
    QueueHealth {
        task,
        pending: 0,
        running: 0,
        scheduled: 0,
        dead: 0,
        recent_dead: 0,
        oldest_pending_minutes: None,
        last_error: None,
    }
}

#[async_trait]
pub trait QueueAdmin: Send + Sync {
    async fn admin_status(&self) -> Result<Vec<QueueHealth>, JobQueueError>;

    async fn admin_retry(
        &self,
        tasks: &[String],
        error: Option<&str>,
        limit: Option<u32>,
        dry_run: bool,
    ) -> Result<u64, JobQueueError>;

    async fn admin_requeue(
        &self,
        task: &JobName,
        limit: u32,
        dry_run: bool,
    ) -> Result<u64, JobQueueError>;
}
