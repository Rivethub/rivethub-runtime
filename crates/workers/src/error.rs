use kernel::memory::EmbeddingStoreError;
use kernel::memory::MemoryStoreError;
use kernel::memory::SummaryStoreError;
use kernel::queue::JobQueueError;
use thiserror::Error;
use uuid::Uuid;

use crate::role::ProcessRole;

pub(crate) const NULL_EMBED_ERROR: &str = "Embedding returned null";
pub(crate) const PERMANENT_NULL_EMBED_ERROR: &str = "Embedding returned null (permanent)";

#[derive(Debug, Error)]
pub enum WorkerError {
    #[error(transparent)]
    Tenancy(#[from] kernel::tenancy::TenancyStoreError),
    #[error("{name} must be a positive 32-bit integer")]
    JobSetting { name: String },
    #[error("Empty synth response")]
    EmptySynth,
    #[error("invalid synthesis payload: {source}")]
    SynthPayload {
        #[source]
        source: serde_json::Error,
    },
    #[error(transparent)]
    MemoryJob(#[from] kernel::memory::jobs::MemoryJobError),
    #[error("memory job dependencies were not supplied")]
    MissingMemoryJobs,
    #[error("wiki store failed: {0}")]
    WikiStore(#[from] kernel::wiki::builder::WikiStoreError),
    #[error("invalid wiki JSON: {0}")]
    WikiJson(#[source] serde_json::Error),
    #[error("wiki model failed: {0}")]
    WikiModel(#[source] kernel::model::ModelProviderError),
    #[error("unknown worker role: {text}; expected {expected}")]
    UnknownRole { text: String, expected: String },
    #[error("{role} has no handler in this build")]
    RoleUnavailable { role: ProcessRole },
    #[error("{role} dependencies were not supplied")]
    MissingDependencies { role: ProcessRole },
    #[error("worker concurrency must be at least 1")]
    Concurrency,
    #[error("embedding width must be at least 1")]
    Width,
    #[error("chunk window must be at least 1")]
    ChunkWindow,
    #[error("job payload is not an embed target: {source}")]
    Payload {
        #[source]
        source: serde_json::Error,
    },
    #[error("the job queue failed: {0}")]
    Queue(#[source] JobQueueError),
    #[error("the embed target store failed: {0}")]
    Target(#[source] EmbeddingStoreError),
    #[error("embedding the model failed: {0}")]
    Embed(String),
    #[error("embedding timed out")]
    EmbedTimeout,
    #[error("Embedding returned null")]
    NullEmbedding,
    #[error("no handler for job {name}")]
    Unhandled { name: String },
    #[error("embed text is too long to index")]
    TextTooLong,
    #[error("the worker was cancelled")]
    Cancelled,
    #[error("a worker task ended: {0}")]
    TaskEnded(String),
    #[error("the worker semaphore is closed")]
    Closed,
    #[error("job payload is not a summarizer job: {source}")]
    SummaryPayload {
        #[source]
        source: serde_json::Error,
    },
    #[error("the summary store failed: {0}")]
    Summary(#[source] SummaryStoreError),
    #[error("{message}")]
    Llm { message: String },
    #[error("the model provider is {cause}")]
    ProviderOutage {
        cause: crate::outage::OutageCause,
        retry_after: Option<std::time::Duration>,
    },
    #[error("conversation {id} is not in the summary store")]
    ConversationMissing { id: Uuid },
    #[error("a truncated leaf batch cannot shrink further")]
    LeafShrink,
    #[error("the model returned an empty summary")]
    EmptySummary,
    #[error("the model truncated the summary")]
    Truncated,
    #[error("another worker is rewriting this page route")]
    RouteBusy,
    #[error("the model returned no title")]
    EmptyTitle,
    #[error("the title answer is not the title JSON: {0}")]
    TitleJson(#[source] serde_json::Error),
    #[error("summary settings are invalid: {message}")]
    SummarySettings { message: String },
    #[error("a summary count does not fit")]
    Count,
    #[error("the summary clock overflowed")]
    Clock,
    #[error("job payload is not a file-conversation job: {source}")]
    FilePayload {
        #[source]
        source: serde_json::Error,
    },
    #[error("{message}")]
    Filing { message: String },
    #[error("{message}")]
    Cleanup { message: String },
    #[error("the model gave no answer within {seconds} s")]
    ModelTimeout { seconds: u64 },
    #[error("the topic store failed: {0}")]
    Topics(#[from] kernel::topics::TopicError),
    #[error("session lineage could not be read: {0}")]
    Lineage(#[from] kernel::transcript::TranscriptReaderError),
    #[error("the memory store failed: {0}")]
    Memory(#[source] MemoryStoreError),
    #[error("topic page sync failed: {0}")]
    TopicPage(#[from] kernel::topics::TopicPageError),
    #[error("the tidy store failed: {0}")]
    Tidy(#[from] kernel::tidy::TidyError),
    #[error("{message}")]
    TidyAnswer { message: String },
}

impl WorkerError {
    pub(crate) fn synth_payload(source: serde_json::Error) -> Self {
        Self::SynthPayload { source }
    }

    pub(crate) fn queue(source: JobQueueError) -> Self {
        Self::Queue(source)
    }

    pub(crate) fn database_unreachable(&self) -> bool {
        matches!(self, Self::Queue(JobQueueError::Unavailable(_)))
    }

    pub(crate) fn payload(source: serde_json::Error) -> Self {
        Self::Payload { source }
    }

    pub(crate) fn summary_payload(source: serde_json::Error) -> Self {
        Self::SummaryPayload { source }
    }

    pub(crate) fn file_payload(source: serde_json::Error) -> Self {
        Self::FilePayload { source }
    }
}

impl From<EmbeddingStoreError> for WorkerError {
    fn from(source: EmbeddingStoreError) -> Self {
        Self::Target(source)
    }
}

impl From<SummaryStoreError> for WorkerError {
    fn from(source: SummaryStoreError) -> Self {
        Self::Summary(source)
    }
}

impl From<MemoryStoreError> for WorkerError {
    fn from(source: MemoryStoreError) -> Self {
        Self::Memory(source)
    }
}
