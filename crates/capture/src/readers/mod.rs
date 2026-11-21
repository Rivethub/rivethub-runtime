pub(crate) mod antigravity;
mod claude;
pub(crate) mod codex;
mod cowork;
mod cursor;
pub(crate) mod dsh;
pub(crate) mod grok;
pub(crate) mod harness_db;
mod hermes;
mod kimi;
mod message;
pub(crate) mod omp;
pub(crate) mod opencode;
mod opencode_events;
mod opencode_link;
mod scan;
pub(crate) mod sqlite;

use jiff::Timestamp;
use std::path::PathBuf;

use async_trait::async_trait;
use kernel::transcript::TranscriptReader;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::Turn;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

pub(crate) use claude::from_parsed;
pub use message::SkipRule;
pub use message::SkippedRecord;
pub use message::TranscriptSlice;
pub(crate) use message::blocking;
pub(crate) use message::report;

#[derive(Debug)]
pub enum StoreReader {
    Claude(crate::transcript::ClaudeTranscriptReader),
    Codex(codex::CodexTranscriptReader),
    Grok(grok::GrokTranscriptReader),
    Hermes(hermes::HermesTranscriptReader),
    Kimi(kimi::KimiTranscriptReader),
    Opencode(opencode::OpencodeTranscriptReader),
    Omp(omp::OmpTranscriptReader),
    Cursor(cursor::CursorTranscriptReader),
    Cowork(cowork::CoworkTranscriptReader),
    Antigravity(antigravity::AntigravityTranscriptReader),
}

impl StoreReader {
    #[must_use]
    pub fn open(harness: HarnessId, root: PathBuf) -> Option<Self> {
        Some(match harness {
            HarnessId::ClaudeCode => {
                Self::Claude(crate::transcript::ClaudeTranscriptReader::new(root))
            }
            HarnessId::Codex => Self::Codex(codex::CodexTranscriptReader::new(root)),
            HarnessId::GrokBuild => Self::Grok(grok::GrokTranscriptReader::new(root)),
            HarnessId::Hermes => Self::Hermes(hermes::HermesTranscriptReader::new(root)),
            HarnessId::KimiCode => Self::Kimi(kimi::KimiTranscriptReader::new(root)),
            HarnessId::Opencode => Self::Opencode(opencode::OpencodeTranscriptReader::new(root)),
            HarnessId::Omp => Self::Omp(omp::OmpTranscriptReader::new(root)),
            HarnessId::Dsh => return None,
            HarnessId::Cursor => Self::Cursor(cursor::CursorTranscriptReader::new(root)),
            HarnessId::Cowork => Self::Cowork(cowork::CoworkTranscriptReader::new(root)),
            HarnessId::Antigravity => {
                Self::Antigravity(antigravity::AntigravityTranscriptReader::new(vec![root]))
            }
        })
    }

    pub async fn messages(
        &self,
        session: &SessionId,
        offset: u64,
    ) -> Result<TranscriptSlice, TranscriptReaderError> {
        match self {
            Self::Claude(reader) => reader.capture_messages(session, offset).await,
            Self::Codex(reader) => reader.messages(session, offset).await,
            Self::Grok(reader) => reader.messages(session, offset).await,
            Self::Hermes(reader) => reader.messages(session, offset).await,
            Self::Kimi(reader) => reader.messages(session, offset).await,
            Self::Opencode(reader) => reader.messages(session, offset).await,
            Self::Omp(reader) => reader.messages(session, offset).await,
            Self::Cursor(reader) => reader.messages(session, offset).await,
            Self::Cowork(reader) => reader.messages(session, offset).await,
            Self::Antigravity(reader) => reader.messages(session, offset).await,
        }
    }
}

#[async_trait]
impl TranscriptReader for StoreReader {
    async fn sessions(
        &self,
        store: &std::path::Path,
    ) -> Result<Vec<SessionId>, TranscriptReaderError> {
        match self {
            Self::Claude(reader) => reader.sessions(store).await,
            Self::Codex(reader) => reader.sessions(store).await,
            Self::Grok(reader) => reader.sessions(store).await,
            Self::Hermes(reader) => reader.sessions(store).await,
            Self::Kimi(reader) => reader.sessions(store).await,
            Self::Opencode(reader) => reader.sessions(store).await,
            Self::Omp(reader) => reader.sessions(store).await,
            Self::Cursor(reader) => reader.sessions(store).await,
            Self::Cowork(reader) => reader.sessions(store).await,
            Self::Antigravity(reader) => reader.sessions(store).await,
        }
    }

    async fn read(&self, session: &SessionId) -> Result<Vec<Turn>, TranscriptReaderError> {
        match self {
            Self::Claude(reader) => reader.read(session).await,
            Self::Codex(reader) => reader.read(session).await,
            Self::Grok(reader) => reader.read(session).await,
            Self::Hermes(reader) => reader.read(session).await,
            Self::Kimi(reader) => reader.read(session).await,
            Self::Opencode(reader) => reader.read(session).await,
            Self::Omp(reader) => reader.read(session).await,
            Self::Cursor(reader) => reader.read(session).await,
            Self::Cowork(reader) => reader.read(session).await,
            Self::Antigravity(reader) => reader.read(session).await,
        }
    }

    async fn tail(
        &self,
        session: &SessionId,
        since: Timestamp,
    ) -> Result<Vec<Turn>, TranscriptReaderError> {
        match self {
            Self::Claude(reader) => reader.tail(session, since).await,
            Self::Codex(reader) => reader.tail(session, since).await,
            Self::Grok(reader) => reader.tail(session, since).await,
            Self::Hermes(reader) => reader.tail(session, since).await,
            Self::Kimi(reader) => reader.tail(session, since).await,
            Self::Opencode(reader) => reader.tail(session, since).await,
            Self::Omp(reader) => reader.tail(session, since).await,
            Self::Cursor(reader) => reader.tail(session, since).await,
            Self::Cowork(reader) => reader.tail(session, since).await,
            Self::Antigravity(reader) => reader.tail(session, since).await,
        }
    }
}
