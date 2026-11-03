use crate::BoxError;
use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use std::fmt;
use std::str::FromStr;
use thiserror::Error;
use values::JsonObject;

#[derive(Debug, Error)]
pub enum JobQueueError {
    #[error("job queue is unavailable")]
    Unavailable(#[source] BoxError),
    #[error("job claim failed: {message}")]
    Claim {
        message: String,
        #[source]
        source: BoxError,
    },
    #[error("graphile worker queue is absent")]
    Absent,
    #[error("database operation failed: {operation}")]
    Operation {
        operation: &'static str,
        #[source]
        source: BoxError,
    },
    #[error("job {id} is missing")]
    Missing { id: JobId },
    #[error("job payload is not a task job")]
    Payload(#[source] BoxError),
    #[error("stored job field is invalid: {0}")]
    Stored(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct JobId(i64);

impl JobId {
    #[must_use]
    pub fn new(value: i64) -> Self {
        Self(value)
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<JobId> for i64 {
    fn from(value: JobId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum JobNameError {
    #[error("unknown job name: {0}")]
    Unknown(String),
    #[error("heartbeat job name needs a numeric index after the last dash: {0}")]
    HeartbeatIndex(String),
    #[error("run-task job name has an empty node")]
    EmptyNode,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    strum::Display,
    strum::EnumString,
    strum::EnumIter,
)]
#[strum(serialize_all = "kebab-case")]
pub enum FixedJob {
    EmbedTarget,
    EnqueueUnembedded,
    EnqueueUnchunked,
    #[strum(serialize = "compact-conversation")]
    SummarizeConversation,
    SynthesizeToolCall,
    FileConversation,
    ProposeTidy,
    EnqueueIdle,
    ExtractWiki,
    EnqueueWikiBackfill,
    ConsolidateWiki,
    RecompileWiki,
    EnqueueStaleWiki,
    #[strum(serialize = "enqueue-stale-compaction")]
    EnqueueStaleSummaries,
    ReapDeadJobs,
    RunTask,
    SyncTopicPage,
    PurgeDeletedSessions,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum JobName {
    Fixed(FixedJob),
    Heartbeat { agent: String, index: u32 },
    RunTaskOn { node: String },
}

impl fmt::Display for JobName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fixed(fixed) => write!(f, "{fixed}"),
            Self::Heartbeat { agent, index } => write!(f, "heartbeat-{agent}-{index}"),
            Self::RunTaskOn { node } => write!(f, "run-task:{node}"),
        }
    }
}

impl FromStr for JobName {
    type Err = JobNameError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if let Some(rest) = value.strip_prefix("heartbeat-") {
            let Some((agent, index)) = rest.rsplit_once('-') else {
                return Err(JobNameError::HeartbeatIndex(value.to_string()));
            };
            let index = index
                .parse::<u32>()
                .map_err(|_| JobNameError::HeartbeatIndex(value.to_string()))?;
            return Ok(Self::Heartbeat {
                agent: agent.to_string(),
                index,
            });
        }
        if let Some(node) = value.strip_prefix("run-task:") {
            if node.is_empty() {
                return Err(JobNameError::EmptyNode);
            }
            return Ok(Self::RunTaskOn {
                node: node.to_string(),
            });
        }
        FixedJob::from_str(value)
            .map(Self::Fixed)
            .map_err(|_| JobNameError::Unknown(value.to_string()))
    }
}

impl TryFrom<String> for JobName {
    type Error = JobNameError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::from_str(&value)
    }
}

impl From<JobName> for String {
    fn from(value: JobName) -> Self {
        value.to_string()
    }
}

pub const MAINTENANCE_PRIORITY: i16 = -100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKeyMode {
    KeepRunAt,
    Replace,
    Arm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::Display)]
#[strum(serialize_all = "kebab-case")]
pub enum WorkerRole {
    Embedder,
    Summarizer,
    Categorizer,
    WikiBuilder,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Job {
    pub name: JobName,
    pub payload: JsonObject,
    pub key: Option<String>,
    pub key_mode: JobKeyMode,
    pub priority: i16,
    pub run_at: Option<DateTime<Utc>>,
    pub max_attempts: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimSelector {
    pub names: Vec<JobName>,
    pub only_id: Option<JobId>,
}

impl ClaimSelector {
    #[must_use]
    pub fn matching(names: Vec<JobName>) -> Self {
        Self {
            names,
            only_id: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedJob {
    pub id: JobId,
    pub name: JobName,
    pub payload: JsonObject,
    pub attempt: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredJob {
    pub id: JobId,
    pub name: JobName,
    pub payload: JsonObject,
    pub key: Option<String>,
    pub priority: Option<i16>,
    pub max_attempts: i32,
    pub attempts: u32,
    pub last_error: Option<String>,
}

#[async_trait]
pub trait JobQueue: std::fmt::Debug + Send + Sync {
    fn task_names(&self, role: WorkerRole) -> Vec<JobName>;

    async fn subscribe(&self) -> Result<Box<dyn JobNotifications>, JobQueueError>;

    async fn enqueue(&self, job: Job) -> Result<JobId, JobQueueError>;

    async fn claim(&self, selector: &ClaimSelector) -> Result<Option<ClaimedJob>, JobQueueError>;

    async fn complete(&self, id: JobId) -> Result<(), JobQueueError>;

    async fn fail(&self, id: JobId, error: &str) -> Result<(), JobQueueError>;

    async fn release(&self, id: JobId) -> Result<(), JobQueueError>;

    async fn postpone(&self, id: JobId, delay: std::time::Duration) -> Result<(), JobQueueError> {
        let _: std::time::Duration = delay;
        self.release(id).await
    }

    async fn flag_stuck(&self) -> Result<u64, JobQueueError>;

    async fn read(&self, id: JobId) -> Result<Option<StoredJob>, JobQueueError>;
}

#[async_trait]
pub trait JobNotifications: Send {
    async fn recv(&mut self) -> Result<(), JobQueueError>;
}

#[async_trait]
impl JobNotifications for tokio::sync::watch::Receiver<()> {
    async fn recv(&mut self) -> Result<(), JobQueueError> {
        self.changed()
            .await
            .map_err(|source| JobQueueError::Unavailable(Box::new(source)))
    }
}

impl WorkerRole {
    #[must_use]
    pub fn task_list(self) -> &'static [JobName] {
        match self {
            Self::Embedder => &[
                JobName::Fixed(FixedJob::EmbedTarget),
                JobName::Fixed(FixedJob::EnqueueUnembedded),
                JobName::Fixed(FixedJob::EnqueueUnchunked),
            ],
            Self::Summarizer => &[
                JobName::Fixed(FixedJob::SummarizeConversation),
                JobName::Fixed(FixedJob::SynthesizeToolCall),
                JobName::Fixed(FixedJob::EnqueueIdle),
                JobName::Fixed(FixedJob::EnqueueStaleSummaries),
                JobName::Fixed(FixedJob::ReapDeadJobs),
                JobName::Fixed(FixedJob::SyncTopicPage),
                JobName::Fixed(FixedJob::PurgeDeletedSessions),
            ],
            Self::Categorizer => &[
                JobName::Fixed(FixedJob::FileConversation),
                JobName::Fixed(FixedJob::ProposeTidy),
            ],
            Self::WikiBuilder => &[
                JobName::Fixed(FixedJob::ExtractWiki),
                JobName::Fixed(FixedJob::EnqueueWikiBackfill),
                JobName::Fixed(FixedJob::ConsolidateWiki),
                JobName::Fixed(FixedJob::RecompileWiki),
                JobName::Fixed(FixedJob::EnqueueStaleWiki),
            ],
        }
    }
}
