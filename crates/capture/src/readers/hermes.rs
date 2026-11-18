use jiff::Timestamp;
use std::path::Path;
use std::path::PathBuf;

use async_trait::async_trait;
use kernel::transcript::TranscriptReader;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::Turn;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use super::harness_db::DbRows;
use super::harness_db::HarnessDb;
use super::harness_db::HermesMessage;
use super::harness_db::Speaker;
use super::harness_db::read_error;

use super::message::Mapped;
use super::message::SkipRule;
use super::message::SkippedRecord;
use super::message::TextTurn;
use super::message::TranscriptSlice;
use super::message::assemble;
use super::message::blocking;
use super::message::native;
use super::message::project_turns;
use super::message::push_mapped;
use super::message::report;
use super::message::row_offset;
use super::message::row_position;
use super::message::text_turn;
use super::sqlite::database_file;
use super::sqlite::require_component;

const FORMAT: &str = "hermes-messages";
const FILE: &str = "state.db";

#[derive(Debug)]
pub struct HermesTranscriptReader {
    root: PathBuf,
}

impl HermesTranscriptReader {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub async fn messages(
        &self,
        session: &SessionId,
        offset: u64,
    ) -> Result<TranscriptSlice, TranscriptReaderError> {
        let Some(native_id) = native(session, HarnessId::Hermes) else {
            return Ok(TranscriptSlice::empty(offset));
        };
        let root = self.root.clone();
        let native_owned = native_id.to_owned();
        let Some(path) = blocking(move || located(&root, &native_owned)).await? else {
            return Ok(TranscriptSlice::empty(offset));
        };
        let after = row_offset(offset)?;
        let rows = HarnessDb::hermes_messages(&path, native_id, after)
            .await
            .map_err(read_error)?;
        let slice = match rows {
            DbRows::Unreadable => TranscriptSlice::empty(offset),
            DbRows::Ready(rows) => map_rows(session, &rows, offset)?,
        };
        report(FORMAT, &slice.skipped);
        Ok(slice)
    }
}

#[async_trait]
impl TranscriptReader for HermesTranscriptReader {
    async fn sessions(&self, store: &Path) -> Result<Vec<SessionId>, TranscriptReaderError> {
        let store = store.to_path_buf();
        let Some(path) = blocking(move || database_file(&store, FILE)).await? else {
            return Ok(Vec::new());
        };
        match HarnessDb::hermes_sessions(&path)
            .await
            .map_err(read_error)?
        {
            DbRows::Unreadable => Ok(Vec::new()),
            DbRows::Ready(ids) => {
                let mut sessions = ids
                    .iter()
                    .map(|id| SessionId::new(HarnessId::Hermes, id))
                    .collect::<Vec<_>>();
                sessions.sort();
                sessions.dedup();
                Ok(sessions)
            }
        }
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

fn located(store: &Path, native_id: &str) -> Result<Option<PathBuf>, TranscriptReaderError> {
    require_component(native_id)?;
    database_file(store, FILE)
}

fn map_rows(
    session: &SessionId,
    rows: &[HermesMessage],
    offset: u64,
) -> Result<TranscriptSlice, TranscriptReaderError> {
    let mut drafts = Vec::new();
    let mut skipped = Vec::new();
    let mut last = offset;
    for row in rows {
        let position = row_position(row.row_id)?;
        last = last.max(position);
        push_mapped(map_row(session, position, row)?, &mut drafts, &mut skipped);
    }
    Ok(assemble(offset, last, drafts, skipped))
}

fn map_row(
    session: &SessionId,
    position: u64,
    row: &HermesMessage,
) -> Result<Mapped, TranscriptReaderError> {
    let Speaker::Known(role) = &row.speaker else {
        return Ok(Mapped::Skip(unrecognized(position, &row.speaker)));
    };
    let Some(at) = row.at.as_deref() else {
        return Ok(Mapped::Skip(SkippedRecord {
            position,
            rule: SkipRule::UnparseableTimestamp,
            detail: "timestamp is not an instant".to_owned(),
        }));
    };
    text_turn(&TextTurn {
        session,
        position,
        role: *role,
        content: &row.content,
        at,
        format: FORMAT,
    })
}

fn unrecognized(position: u64, speaker: &Speaker) -> SkippedRecord {
    let Speaker::Unrecognized { text, parse_error } = speaker else {
        return SkippedRecord {
            position,
            rule: SkipRule::UnrecognizedSpeaker,
            detail: "hermes speaker".to_owned(),
        };
    };
    SkippedRecord {
        position,
        rule: SkipRule::UnrecognizedSpeaker,
        detail: format!("{text}: {parse_error}"),
    }
}
