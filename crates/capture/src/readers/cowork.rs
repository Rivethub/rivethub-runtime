mod lines;

use jiff::Timestamp;
use std::path::Path;
use std::path::PathBuf;

use async_trait::async_trait;
use kernel::transcript::TranscriptReader;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::Turn;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use super::message::TranscriptSlice;
use super::message::blocking;
use super::message::native;
use super::message::project_turns;
use super::message::report;
use crate::listing::cowork::transcripts;
use crate::store_path::session_component;
use crate::transcript::parse_lines;
use crate::transcript::read_capped;

const FORMAT: &str = "cowork-jsonl";

#[derive(Debug)]
pub struct CoworkTranscriptReader {
    root: PathBuf,
}

impl CoworkTranscriptReader {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub async fn messages(
        &self,
        session: &SessionId,
        offset: u64,
    ) -> Result<TranscriptSlice, TranscriptReaderError> {
        let Some(native_id) = native(session, HarnessId::Cowork).map(str::to_owned) else {
            return Ok(TranscriptSlice::empty(offset));
        };
        let root = self.root.clone();
        let session = session.clone();
        let slice = blocking(move || load(&root, &session, &native_id, offset)).await?;
        report(FORMAT, &slice.skipped);
        Ok(slice)
    }
}

#[async_trait]
impl TranscriptReader for CoworkTranscriptReader {
    async fn sessions(&self, store: &Path) -> Result<Vec<SessionId>, TranscriptReaderError> {
        let store = store.to_path_buf();
        blocking(move || Ok(list_sessions(&store))).await
    }

    async fn read(&self, session: &SessionId) -> Result<Vec<Turn>, TranscriptReaderError> {
        Ok(project_turns(&self.messages(session, 0).await?))
    }

    async fn tail(
        &self,
        session: &SessionId,
        since: Timestamp,
    ) -> Result<Vec<Turn>, TranscriptReaderError> {
        Ok(self
            .read(session)
            .await?
            .into_iter()
            .filter(|turn| turn.at > since)
            .collect())
    }
}

fn list_sessions(store: &Path) -> Vec<SessionId> {
    let mut sessions: Vec<SessionId> = transcripts(&[store.to_path_buf()])
        .into_iter()
        .map(|(id, _)| SessionId::new(HarnessId::Cowork, &id))
        .collect();
    sessions.sort();
    sessions.dedup();
    sessions
}

fn load(
    store: &Path,
    session: &SessionId,
    native_id: &str,
    offset: u64,
) -> Result<TranscriptSlice, TranscriptReaderError> {
    session_component(native_id)?;
    let Some((_, path)) = transcripts(&[store.to_path_buf()])
        .into_iter()
        .find(|(id, _)| id == native_id)
    else {
        return Ok(TranscriptSlice::empty(offset));
    };
    let parsed = parse_lines(&read_capped(&path)?);
    lines::map(session, &parsed, offset)
}
