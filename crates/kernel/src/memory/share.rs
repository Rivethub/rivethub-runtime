use std::path::PathBuf;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use thiserror::Error;
use uuid::Uuid;

use super::CaptureBatchRequest;
use super::MemoryStoreError;
use super::SessionDigest;

#[async_trait]
pub trait SessionShare: Send + Sync + std::fmt::Debug {
    async fn digests(&self, keys: &[String]) -> Result<Vec<SessionDigest>, ShareFailure>;

    async fn export(&self, ids: &[Uuid], out: PathBuf) -> Result<usize, ShareFailure>;

    async fn sanitize(&self, job: SanitizeJob) -> Result<SanitizeSummary, ShareFailure>;

    async fn open(&self, path: PathBuf) -> Result<OpenedBundle, ShareFailure>;

    fn redact(&self, text: &str) -> String;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizeJob {
    pub input: PathBuf,
    pub out: PathBuf,
    pub mapping: Option<PathBuf>,
    pub decide: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SanitizeSummary {
    pub sessions: usize,
    pub totals: String,
    pub rule_counts: String,
    pub not_scanned: String,
    pub best_effort: String,
    pub bundle: PathBuf,
    pub report: PathBuf,
    pub local: Option<PathBuf>,
    pub decision: Option<DecisionSummary>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DecisionSummary {
    pub model: String,
    pub scored: usize,
    pub calls: usize,
    pub trivial: usize,
    pub mode: DecideMode,
    pub flags: Vec<FlagSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DecideMode {
    Advisory,
    Block,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FlagSummary {
    pub number: usize,
    pub score: f64,
    pub place: Option<FlagPlace>,
    pub report_line: Option<usize>,
    pub chars: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlagPlace {
    pub session: usize,
    pub message: Option<usize>,
    pub field: String,
    pub more: usize,
}

#[derive(Debug)]
pub enum OpenedBundle {
    Raw,
    Sanitized(SanitizedBundle),
}

#[derive(Debug)]
pub struct SanitizedBundle {
    pub secrets: Option<Secrets>,
    pub withheld: usize,
    pub unreviewed: usize,
    pub sessions: Vec<ListedSession>,
    pub authors: Vec<String>,
    pub batches: Result<Vec<CaptureBatchRequest>, ShareFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Secrets {
    pub count: usize,
    pub first: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedSession {
    pub key: String,
    pub agent: String,
    pub messages: usize,
    pub author: String,
    pub dates: Option<(DateTime<Utc>, DateTime<Utc>)>,
}

#[derive(Debug, Error)]
pub enum ShareFailure {
    #[error(
        "refused: {} already exists; session tools never replace a file, so choose another out name", .0.display()
    )]
    Exists(PathBuf),
    #[error("no session this node can export has the id {0}")]
    UnknownSessions(String),
    #[error("not a session bundle: {}", .0.display())]
    NotBundle(PathBuf),
    #[error(
        "decide needs memory.sanitize.decide.url in the config (the decision model); rh mcp reads it at startup, so restart it after you set the key"
    )]
    DecideUnconfigured,
    #[error("{0}")]
    Failed(String),
    #[error(transparent)]
    Memory(#[from] MemoryStoreError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}
