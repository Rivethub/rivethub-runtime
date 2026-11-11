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

pub const MAX_CHOSEN: usize = 50;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ChoiceError {
    #[error("a redact term is blank; name the text to redact")]
    BlankTerm,
    #[error("redact names no rule; give at least one term or drop_tool_outputs: true")]
    NoRule,
    #[error("{0} sessions are chosen; choose from 1 to {MAX_CHOSEN}")]
    Count(usize),
    #[error("the session {0} is chosen twice; give each session one decision")]
    Duplicate(Uuid),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Term(String);

impl Term {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Term {
    type Error = ChoiceError;

    fn try_from(text: String) -> Result<Self, Self::Error> {
        if text.trim().is_empty() {
            return Err(ChoiceError::BlankTerm);
        }
        Ok(Self(text))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRedaction {
    terms: Vec<Term>,
    drop_tool_outputs: bool,
}

impl SessionRedaction {
    pub fn new(terms: Vec<Term>, drop_tool_outputs: bool) -> Result<Self, ChoiceError> {
        if terms.is_empty() && !drop_tool_outputs {
            return Err(ChoiceError::NoRule);
        }
        Ok(Self {
            terms,
            drop_tool_outputs,
        })
    }

    #[must_use]
    pub fn terms(&self) -> &[Term] {
        &self.terms
    }

    #[must_use]
    pub const fn drops_tool_outputs(&self) -> bool {
        self.drop_tool_outputs
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Approve,
    Redact(SessionRedaction),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionChoices(Vec<(Uuid, Choice)>);

impl SessionChoices {
    #[must_use]
    pub fn as_slice(&self) -> &[(Uuid, Choice)] {
        &self.0
    }

    pub fn ids(&self) -> impl Iterator<Item = Uuid> + '_ {
        self.0.iter().map(|(id, _)| *id)
    }
}

impl TryFrom<Vec<(Uuid, Choice)>> for SessionChoices {
    type Error = ChoiceError;

    fn try_from(chosen: Vec<(Uuid, Choice)>) -> Result<Self, Self::Error> {
        if chosen.is_empty() || chosen.len() > MAX_CHOSEN {
            return Err(ChoiceError::Count(chosen.len()));
        }
        for (index, (id, _)) in chosen.iter().enumerate() {
            if chosen.iter().take(index).any(|(seen, _)| seen == id) {
                return Err(ChoiceError::Duplicate(*id));
            }
        }
        Ok(Self(chosen))
    }
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
