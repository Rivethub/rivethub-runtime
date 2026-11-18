use jiff::Timestamp;
use std::path::Path;
use std::path::PathBuf;

use async_trait::async_trait;
use kernel::transcript::TranscriptReader;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::Turn;
use serde::Deserialize;
use wire::harness::HarnessId;
use wire::message::Role;
use wire::session_id::SessionId;

use super::message::Mapped;
use super::message::SkipRule;
use super::message::SkippedRecord;
use super::message::TextTurn;
use super::message::ToolFields;
use super::message::TranscriptSlice;
use super::message::assemble;
use super::message::blocking;
use super::message::native;
use super::message::parse_json;
use super::message::project_turns;
use super::message::push_mapped;
use super::message::report;
use super::message::text_turn;
use super::message::tool_turn;
use super::message::unknown;
use super::scan::lines;
use super::scan::read_optional;
use crate::store_path::DirState;
use crate::store_path::Root;
use crate::store_path::dir_state;
use crate::store_path::jsonl;
use crate::store_path::session_component;
use crate::store_path::utf8_stem;

const FORMAT: &str = "omp-jsonl";

#[derive(Debug)]
pub struct OmpTranscriptReader {
    root: PathBuf,
}

impl OmpTranscriptReader {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub async fn messages(
        &self,
        session: &SessionId,
        offset: u64,
    ) -> Result<TranscriptSlice, TranscriptReaderError> {
        let Some(native_id) = native(session, HarnessId::Omp).map(str::to_owned) else {
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
impl TranscriptReader for OmpTranscriptReader {
    async fn sessions(&self, store: &Path) -> Result<Vec<SessionId>, TranscriptReaderError> {
        let store = store.to_path_buf();
        blocking(move || list_sessions(&store)).await
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

fn list_sessions(store: &Path) -> Result<Vec<SessionId>, TranscriptReaderError> {
    let mut sessions = Vec::new();
    for (id, _) in transcripts(&Root::open(store)?)? {
        sessions.push(SessionId::new(HarnessId::Omp, &id));
    }
    sessions.sort();
    sessions.dedup();
    Ok(sessions)
}

fn load(
    store: &Path,
    session: &SessionId,
    native_id: &str,
    offset: u64,
) -> Result<TranscriptSlice, TranscriptReaderError> {
    session_component(native_id)?;
    let root = Root::open(store)?;
    let Some(path) = transcripts(&root)?
        .into_iter()
        .find_map(|(id, path)| (id == native_id).then_some(path))
    else {
        return Ok(TranscriptSlice::empty(offset));
    };
    let Some(text) = read_optional(&root, &path)? else {
        return Ok(TranscriptSlice::empty(offset));
    };
    map_text(session, native_id, &text, offset)
}

fn transcripts(root: &Root) -> Result<Vec<(String, PathBuf)>, TranscriptReaderError> {
    let sessions = root.canonical().join("sessions");
    if dir_state(root, &sessions)? == DirState::Absent {
        return Ok(Vec::new());
    }
    let mut found = session_files(root, &sessions)?;
    for encoded in root.directories(&sessions)? {
        found.extend(session_files(root, &encoded)?);
    }
    Ok(found)
}

fn session_files(root: &Root, dir: &Path) -> Result<Vec<(String, PathBuf)>, TranscriptReaderError> {
    let mut found = Vec::new();
    for file in root.files(dir)? {
        if !jsonl(&file) {
            continue;
        }
        if let Some((_, id)) = omp_id(&utf8_stem(&file)?) {
            found.push((id, file));
        }
    }
    Ok(found)
}

pub(crate) fn omp_id(stem: &str) -> Option<(String, String)> {
    let (prefix, uuid) = stem.rsplit_once('_')?;
    let hyphenated = uuid.len() == 36 && uuid::Uuid::try_parse(uuid).is_ok();
    (!prefix.is_empty() && hyphenated).then(|| (prefix.to_owned(), uuid.to_owned()))
}

fn map_text(
    session: &SessionId,
    native_id: &str,
    text: &str,
    offset: u64,
) -> Result<TranscriptSlice, TranscriptReaderError> {
    let (records, last) = lines(text)?;
    let mut drafts = Vec::new();
    let mut skipped = Vec::new();
    for record in &records {
        let mapped = match parse_json::<OmpLine>(record.number, &record.text) {
            Ok(line) => map_line(session, native_id, record.number, line)?,
            Err(skipped) => Mapped::Skip(skipped),
        };
        push_mapped(mapped, &mut drafts, &mut skipped);
    }
    Ok(assemble(offset, last, drafts, skipped))
}

fn map_line(
    session: &SessionId,
    native_id: &str,
    position: u64,
    line: OmpLine,
) -> Result<Mapped, TranscriptReaderError> {
    match line {
        OmpLine::Session { id } => Ok(session_header(position, native_id, &id)),
        OmpLine::Title | OmpLine::ModelChange | OmpLine::ThinkingLevelChange | OmpLine::Custom => {
            Ok(Mapped::None)
        }
        OmpLine::Message { message, timestamp } => {
            map_message(session, position, message, &timestamp)
        }
        OmpLine::Unknown => Ok(Mapped::Skip(unknown(position, "unknown omp record"))),
    }
}

fn map_message(
    session: &SessionId,
    position: u64,
    message: OmpMessage,
    at: &str,
) -> Result<Mapped, TranscriptReaderError> {
    let turn = |role, content| TextTurn {
        session,
        position,
        role,
        content,
        at,
        format: FORMAT,
    };
    match message {
        OmpMessage::User { content } => text_turn(&turn(Role::User, &content.text())),
        OmpMessage::Assistant {
            content,
            error_message,
        } => {
            let said = joined(&content);
            let reply = match error_message {
                Some(error) if said.trim().is_empty() => error,
                Some(error) => format!("{said}\n\n{error}"),
                None => said,
            };
            text_turn(&turn(Role::Assistant, &reply))
        }
        OmpMessage::ToolResult { tool_name, content } => {
            let result = joined(&content);
            tool_turn(
                &turn(Role::Tool, &result),
                ToolFields {
                    name: Some(tool_name),
                    arguments: None,
                    result: Some(result.clone()),
                },
            )
        }
        OmpMessage::Other => Ok(Mapped::Skip(speaker(position))),
    }
}

fn session_header(position: u64, native_id: &str, id: &str) -> Mapped {
    if id == native_id {
        Mapped::None
    } else {
        Mapped::Skip(super::message::unparseable(
            position,
            format!("session header id {id} does not match the file {native_id}"),
        ))
    }
}

fn speaker(position: u64) -> SkippedRecord {
    SkippedRecord {
        position,
        rule: SkipRule::UnrecognizedSpeaker,
        detail: "omp message role".to_owned(),
    }
}

fn joined(parts: &[Part]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_str()),
            Part::Other => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OmpLine {
    Title,
    Session {
        id: String,
    },
    ModelChange,
    ThinkingLevelChange,
    Custom,
    Message {
        message: OmpMessage,
        timestamp: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
enum OmpMessage {
    User {
        content: Content,
    },
    #[serde(rename_all = "camelCase")]
    Assistant {
        content: Vec<Part>,
        error_message: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    ToolResult {
        tool_name: String,
        content: Vec<Part>,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Parts(Vec<Part>),
}

impl Content {
    fn text(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::Parts(parts) => joined(parts),
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
enum Part {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}
