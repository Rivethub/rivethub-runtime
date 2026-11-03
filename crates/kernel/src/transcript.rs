use crate::BoxError;
use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use thiserror::Error;
use values::HarnessId;
use values::Role;
use values::SessionId;

mod turn;

pub use turn::{
    HarnessToolOutput, HarnessTranscriptTool, HarnessTranscriptTurn, MessageUsage,
    SessionMessageRole, TranscriptAttachment, TranscriptBlock, TranscriptToolStatus,
};

const SUBAGENTS: &str = "/subagents/";

#[derive(Debug, Error)]
pub enum TranscriptReaderError {
    #[error("transcript store is unavailable")]
    Unavailable(#[source] BoxError),
    #[error("transcript read failed: {message}")]
    Read {
        message: String,
        #[source]
        source: BoxError,
    },
}

#[derive(Debug, Error)]
pub enum SessionRenameError {
    #[error("{0} sessions cannot be renamed")]
    Unsupported(HarnessId),
    #[error("the session has no record in its harness store")]
    Missing,
    #[error("writing the session title failed: {message}")]
    Write {
        message: String,
        #[source]
        source: BoxError,
    },
    #[error(transparent)]
    Store(#[from] TranscriptReaderError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub role: Role,
    pub content: String,
    pub at: DateTime<Utc>,
}

#[async_trait]
pub trait TranscriptReader: Send + Sync {
    async fn sessions(
        &self,
        store: &std::path::Path,
    ) -> Result<Vec<SessionId>, TranscriptReaderError>;

    async fn read(&self, session: &SessionId) -> Result<Vec<Turn>, TranscriptReaderError>;

    async fn tail(
        &self,
        session: &SessionId,
        since: DateTime<Utc>,
    ) -> Result<Vec<Turn>, TranscriptReaderError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreSession {
    pub harness: HarnessId,
    pub id: String,
    pub title: String,
    pub updated_at: i64,
    pub created_at: Option<i64>,
    pub cwd: Option<String>,
    pub parent_session_id: Option<String>,
    pub agent_name: Option<String>,
    pub model: Option<String>,
    pub task_id: Option<String>,
}

impl StoreSession {
    #[must_use]
    pub fn new(harness: HarnessId, id: &str, updated_at: i64) -> Self {
        Self {
            harness,
            id: id.to_owned(),
            title: id.to_owned(),
            updated_at,
            created_at: None,
            cwd: None,
            parent_session_id: None,
            agent_name: None,
            model: None,
            task_id: None,
        }
    }
}

#[must_use]
pub fn linked_parent(harness: HarnessId, parent: &str) -> Option<SessionId> {
    let linkable = !parent.is_empty()
        && !parent.contains('/')
        && !parent.contains("..")
        && !parent.starts_with(char::is_whitespace)
        && !parent.ends_with(char::is_whitespace);
    linkable.then(|| SessionId::new(harness, parent))
}

pub async fn session_parent(
    lineage: &dyn SessionLister,
    session: &SessionId,
) -> Result<Option<String>, TranscriptReaderError> {
    if let Some((parent, _)) = session.native().split_once(SUBAGENTS)
        && !parent.is_empty()
    {
        return Ok(Some(parent.to_owned()));
    }
    Ok(lineage
        .describe(session.harness(), session.native(), None)
        .await?
        .and_then(|row| row.parent_session_id))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListLimit(usize);

impl ListLimit {
    pub const DEFAULT: Self = Self(100);
    pub const MAX: usize = 500;

    #[must_use]
    pub const fn most() -> Self {
        Self(Self::MAX)
    }

    #[must_use]
    pub const fn per_harness_route() -> Self {
        Self::DEFAULT
    }

    #[must_use]
    pub fn from_query(raw: Option<&str>) -> Self {
        match raw.and_then(LeadingInteger::parse) {
            Some(LeadingInteger::Positive(value)) if value > 0 => Self(value.min(Self::MAX)),
            Some(LeadingInteger::Positive(_) | LeadingInteger::NonPositive) | None => Self::DEFAULT,
        }
    }

    #[must_use]
    pub const fn get(self) -> usize {
        self.0
    }
}

enum LeadingInteger {
    Positive(usize),
    NonPositive,
}

impl LeadingInteger {
    fn parse(raw: &str) -> Option<Self> {
        let text = raw.trim_start();
        let (negative, digits) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text.strip_prefix('+').unwrap_or(text)),
        };
        let run: Vec<u8> = digits
            .bytes()
            .take_while(u8::is_ascii_digit)
            .map(|digit| digit - b'0')
            .collect();
        if run.is_empty() {
            return None;
        }
        let magnitude = run.iter().fold(0_usize, |total, digit| {
            total.saturating_mul(10).saturating_add(usize::from(*digit))
        });
        Some(if negative {
            Self::NonPositive
        } else {
            Self::Positive(magnitude)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct StoreTranscript {
    pub turns: Vec<HarnessTranscriptTurn>,
    pub earlier: Option<u64>,
    pub context: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TranscriptWindow {
    #[default]
    Latest,
    From(u64),
    Before(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptRead {
    Read(StoreTranscript),
    NoSource,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolOutputRead {
    Found(HarnessToolOutput),
    Missing,
    NoSource,
}

#[async_trait]
pub trait SessionLister: Send + Sync {
    async fn list(&self, harnesses: &[HarnessId], limit: ListLimit) -> Vec<StoreSession>;

    async fn describe(
        &self,
        harness: HarnessId,
        native: &str,
        parent: Option<&str>,
    ) -> Result<Option<StoreSession>, TranscriptReaderError>;

    async fn describe_many(
        &self,
        harness: HarnessId,
        natives: &[String],
    ) -> Result<Vec<StoreSession>, TranscriptReaderError> {
        let mut rows = Vec::with_capacity(natives.len());
        for native in natives {
            if let Some(row) = self.describe(harness, native, None).await? {
                rows.push(row);
            }
        }
        Ok(rows)
    }

    async fn store_cwd(
        &self,
        harness: HarnessId,
        native: &str,
    ) -> Result<Option<String>, TranscriptReaderError>;

    async fn permission_mode(
        &self,
        _harness: HarnessId,
        _native: &str,
    ) -> Result<Option<String>, TranscriptReaderError> {
        Ok(None)
    }

    async fn running_elsewhere(
        &self,
        _harness: HarnessId,
        _native: &str,
    ) -> Result<Option<u32>, TranscriptReaderError> {
        Ok(None)
    }

    async fn transcript(
        &self,
        harness: HarnessId,
        native: &str,
        window: TranscriptWindow,
    ) -> Result<TranscriptRead, TranscriptReaderError>;

    async fn rename(
        &self,
        harness: HarnessId,
        native: &str,
        title: &str,
    ) -> Result<StoreSession, SessionRenameError>;

    async fn tool_output(
        &self,
        harness: HarnessId,
        native: &str,
        tool_call_id: &str,
    ) -> Result<ToolOutputRead, TranscriptReaderError>;
}
