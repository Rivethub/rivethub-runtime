use std::path::Path;
use std::path::PathBuf;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::transcript::TranscriptReader;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::Turn;
use wire::harness::HarnessId;
use wire::message::Role;
use wire::session_id::SessionId;

use super::harness_db::DbRows;
use super::harness_db::HarnessDb;
use super::harness_db::OpencodeCursor;
use super::harness_db::OpencodePart;
use super::harness_db::OpencodeTranscript;
use super::harness_db::PartKind;
use super::harness_db::Speaker;
use super::harness_db::read_error;

use super::message::Mapped;
use super::message::SkipRule;
use super::message::SkippedRecord;
use super::message::TextTurn;
use super::message::ToolFields;
use super::message::TranscriptSlice;
use super::message::arguments;
use super::message::assemble;
use super::message::blocking;
use super::message::json_failed;
use super::message::native;
use super::message::project_turns;
use super::message::push_mapped;
use super::message::report;
use super::message::row_offset;
use super::message::row_position;
use super::message::text_turn;
use super::message::tool_turn;
use super::message::unknown;
use super::opencode_events;
use super::opencode_link::opencode_link;
use super::sqlite::database_file;
use super::sqlite::require_component;
use crate::records::SubagentLink;

const FORMAT: &str = "opencode-part";
const FILE: &str = "opencode.db";

#[derive(Debug)]
pub struct OpencodeTranscriptReader {
    root: PathBuf,
}

impl OpencodeTranscriptReader {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub async fn messages(
        &self,
        session: &SessionId,
        offset: u64,
    ) -> Result<TranscriptSlice, TranscriptReaderError> {
        self.slice(session, offset, Mode::Transcript).await
    }

    pub async fn capture(
        &self,
        session: &SessionId,
    ) -> Result<TranscriptSlice, TranscriptReaderError> {
        self.slice(session, 0, Mode::Capture).await
    }

    async fn slice(
        &self,
        session: &SessionId,
        offset: u64,
        mode: Mode,
    ) -> Result<TranscriptSlice, TranscriptReaderError> {
        let Some(native_id) = native(session, HarnessId::Opencode) else {
            return Ok(TranscriptSlice::empty(offset));
        };
        let root = self.root.clone();
        let native_owned = native_id.to_owned();
        let Some(path) = blocking(move || located(&root, &native_owned)).await? else {
            return Ok(TranscriptSlice::empty(offset));
        };
        let cursor = OpencodeCursor {
            after_row: row_offset(offset)?,
            from_seq: opencode_events::first_seq(offset)?,
        };
        let rows = HarnessDb::opencode_transcript(&path, native_id, cursor)
            .await
            .map_err(read_error)?;
        let (format, mut slice) = match rows {
            OpencodeTranscript::Unreadable => (FORMAT, TranscriptSlice::empty(offset)),
            OpencodeTranscript::Parts(rows) => (FORMAT, map_parts(session, &rows, offset, mode)?),
            OpencodeTranscript::Events(rows) => (
                opencode_events::FORMAT,
                opencode_events::map_events(session, &rows, offset, mode)?,
            ),
        };
        if mode == Mode::Capture {
            capture_positions(&path, &mut slice)?;
        }
        report(format, &slice.skipped);
        Ok(slice)
    }
}

#[async_trait]
impl TranscriptReader for OpencodeTranscriptReader {
    async fn sessions(&self, store: &Path) -> Result<Vec<SessionId>, TranscriptReaderError> {
        let store = store.to_path_buf();
        let Some(path) = blocking(move || database_file(&store, FILE)).await? else {
            return Ok(Vec::new());
        };
        match HarnessDb::opencode_sessions(&path)
            .await
            .map_err(read_error)?
        {
            DbRows::Unreadable => Ok(Vec::new()),
            DbRows::Ready(ids) => {
                let mut sessions = ids
                    .iter()
                    .map(|id| SessionId::new(HarnessId::Opencode, id))
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
        since: DateTime<Utc>,
    ) -> Result<Vec<Turn>, TranscriptReaderError> {
        Ok(self
            .read(session)
            .await?
            .into_iter()
            .filter(|turn| turn.at > since)
            .collect())
    }
}

pub(crate) async fn subagent_link(
    root: PathBuf,
    native_id: String,
) -> Result<Option<SubagentLink>, TranscriptReaderError> {
    blocking(move || match located(&root, &native_id)? {
        Some(path) => opencode_link(&path, &native_id).map_err(read_error),
        None => Ok(None),
    })
    .await
}

fn capture_positions(
    path: &Path,
    slice: &mut TranscriptSlice,
) -> Result<(), TranscriptReaderError> {
    let path = path.display().to_string();
    for row in &mut slice.messages {
        if let Some(metadata) = &mut row.metadata
            && let Some(value) = metadata.get("position")
            && let Some(position) = value.as_u64()
        {
            metadata
                .insert("session_store_position", position)
                .map_err(json_failed)?;
            metadata
                .insert("session_sqlite_path", &path)
                .map_err(json_failed)?;
        }
    }
    Ok(())
}

fn located(store: &Path, native_id: &str) -> Result<Option<PathBuf>, TranscriptReaderError> {
    require_component(native_id)?;
    database_file(store, FILE)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Mode {
    Transcript,
    Capture,
}

fn map_parts(
    session: &SessionId,
    rows: &[OpencodePart],
    offset: u64,
    mode: Mode,
) -> Result<TranscriptSlice, TranscriptReaderError> {
    let mut drafts = Vec::new();
    let mut skipped = Vec::new();
    let mut last = offset;
    for row in rows {
        let position = row_position(row.row_id)?;
        last = last.max(position);
        let mapped = match map_part(session, position, row, mode)? {
            Mapped::Draft(mut draft) => {
                draft.message.event_id.clone_from(&row.part_id);
                Mapped::Draft(draft).with_metadata(|metadata| {
                    if let Some(id) = row.tool_use_id.as_deref().filter(|id| !id.is_empty()) {
                        metadata.insert("tool_use_id", id)?;
                    }
                    Ok(())
                })?
            }
            mapped @ (Mapped::Skip(_) | Mapped::None) => mapped,
        };
        push_mapped(mapped, &mut drafts, &mut skipped);
    }
    Ok(assemble(offset, last, drafts, skipped))
}

fn map_part(
    session: &SessionId,
    position: u64,
    row: &OpencodePart,
    mode: Mode,
) -> Result<Mapped, TranscriptReaderError> {
    if row.kind == PartKind::Ignored
        || (row.kind == PartKind::Reasoning && mode == Mode::Transcript)
    {
        return Ok(Mapped::None);
    }
    if row.kind == PartKind::Unrecognized {
        return Ok(Mapped::Skip(unknown(position, "unknown opencode part")));
    }
    let Speaker::Known(role) = &row.speaker else {
        return Ok(Mapped::Skip(unrecognized(position, &row.speaker)));
    };
    let Some(at) = row.at.as_deref() else {
        return Ok(Mapped::Skip(SkippedRecord {
            position,
            rule: SkipRule::UnparseableTimestamp,
            detail: "part time is not an instant".to_owned(),
        }));
    };
    if row.kind == PartKind::Text {
        let text = row.text.as_deref().unwrap_or_default();
        let content = if mode == Mode::Capture {
            text.trim()
        } else {
            text
        };
        return text_turn(&TextTurn {
            session,
            position,
            role: *role,
            content,
            at,
            format: FORMAT,
        });
    }
    if row.kind == PartKind::Reasoning {
        let Some(text) = row
            .text
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        else {
            return Ok(Mapped::None);
        };
        return text_turn(&TextTurn {
            session,
            position,
            role: Role::Assistant,
            content: &format!("[thinking] {text}"),
            at,
            format: FORMAT,
        });
    }
    let Some(content) = tool_content(row, mode) else {
        return Ok(Mapped::None);
    };
    let arguments = match row.tool_arguments.as_deref() {
        Some(text) => match arguments(position, text) {
            Ok(arguments) => Some(arguments),
            Err(skipped) => return Ok(Mapped::Skip(skipped)),
        },
        None => None,
    };
    tool_turn(
        &TextTurn {
            session,
            position,
            role: Role::Tool,
            content: &content,
            at,
            format: FORMAT,
        },
        ToolFields {
            name: row.tool_name.clone(),
            arguments,
            result: row.tool_result.clone(),
        },
    )
}

fn tool_content(row: &OpencodePart, mode: Mode) -> Option<String> {
    if mode == Mode::Capture {
        let outcome = if row.tool_failed {
            "tool-failure"
        } else {
            "tool-result"
        };
        return Some(format!(
            "[{outcome}] {}",
            row.tool_name.as_deref().unwrap_or("tool")
        ));
    }
    if let Some(text) = &row.text
        && !text.is_empty()
    {
        return Some(text.clone());
    }
    row.tool_result
        .as_ref()
        .filter(|text| !text.is_empty())
        .cloned()
}

fn unrecognized(position: u64, speaker: &Speaker) -> SkippedRecord {
    let Speaker::Unrecognized { text, parse_error } = speaker else {
        return SkippedRecord {
            position,
            rule: SkipRule::UnrecognizedSpeaker,
            detail: "opencode speaker".to_owned(),
        };
    };
    SkippedRecord {
        position,
        rule: SkipRule::UnrecognizedSpeaker,
        detail: format!("{text}: {parse_error}"),
    }
}
