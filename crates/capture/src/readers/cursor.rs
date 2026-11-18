use jiff::Timestamp;
use std::path::Path;
use std::path::PathBuf;

use async_trait::async_trait;
use kernel::transcript::TranscriptReader;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::Turn;
use serde::Deserialize;
use wire::JsonObject;
use wire::harness::HarnessId;
use wire::message::Role;
use wire::session_id::SessionId;

use super::message::Mapped;
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
use crate::store_path::Child;
use crate::store_path::DirState;
use crate::store_path::Root;
use crate::store_path::dir_state;
use crate::store_path::session_component;

const FORMAT: &str = "cursor-jsonl";

#[derive(Debug)]
pub struct CursorTranscriptReader {
    root: PathBuf,
}

impl CursorTranscriptReader {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub async fn messages(
        &self,
        session: &SessionId,
        offset: u64,
    ) -> Result<TranscriptSlice, TranscriptReaderError> {
        let Some(native_id) = native(session, HarnessId::Cursor).map(str::to_owned) else {
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
impl TranscriptReader for CursorTranscriptReader {
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
        sessions.push(SessionId::new(HarnessId::Cursor, &id));
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
    map_text(session, &text, offset)
}

fn transcripts(root: &Root) -> Result<Vec<(String, PathBuf)>, TranscriptReaderError> {
    let projects = root.canonical().join("projects");
    if dir_state(root, &projects)? == DirState::Absent {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for slug in root.directories(&projects)? {
        let transcripts = slug.join("agent-transcripts");
        if dir_state(root, &transcripts)? == DirState::Absent {
            continue;
        }
        for session_dir in root.directories(&transcripts)? {
            let Some(id) = dir_name(&session_dir)? else {
                continue;
            };
            if uuid::Uuid::parse_str(&id).is_err() {
                continue;
            }
            let file = session_dir.join(format!("{id}.jsonl"));
            if root.classify(&file)? == Child::File {
                found.push((id, file));
            }
        }
    }
    Ok(found)
}

fn dir_name(path: &Path) -> Result<Option<String>, TranscriptReaderError> {
    match path.file_name().and_then(|name| name.to_str()) {
        Some(name) => Ok(Some(name.to_owned())),
        None => Err(crate::store_path::refused(
            path,
            "directory name is not utf-8",
        )),
    }
}

fn map_text(
    session: &SessionId,
    text: &str,
    offset: u64,
) -> Result<TranscriptSlice, TranscriptReaderError> {
    let (records, last) = lines(text)?;
    let mut drafts = Vec::new();
    let mut skipped = Vec::new();
    for record in &records {
        let mapped = match parse_json::<CursorLine>(record.number, &record.text) {
            Ok(line) => map_line(session, record.number, line)?,
            Err(skipped) => Mapped::Skip(skipped),
        };
        push_mapped(mapped, &mut drafts, &mut skipped);
    }
    Ok(assemble(offset, last, drafts, skipped))
}

fn map_line(
    session: &SessionId,
    position: u64,
    line: CursorLine,
) -> Result<Mapped, TranscriptReaderError> {
    match line {
        CursorLine::User { content, timestamp } => text_turn(&TextTurn {
            session,
            position,
            role: Role::User,
            content: &content,
            at: &timestamp,
            format: FORMAT,
        }),
        CursorLine::Assistant { content, timestamp } => text_turn(&TextTurn {
            session,
            position,
            role: Role::Assistant,
            content: &content,
            at: &timestamp,
            format: FORMAT,
        }),
        CursorLine::Tool {
            name,
            arguments,
            result,
            timestamp,
        } => {
            let content = result.clone();
            tool_turn(
                &TextTurn {
                    session,
                    position,
                    role: Role::Tool,
                    content: &content,
                    at: &timestamp,
                    format: FORMAT,
                },
                ToolFields {
                    name: Some(name),
                    arguments: Some(arguments),
                    result: Some(result),
                },
            )
        }
        CursorLine::Unknown => Ok(Mapped::Skip(unknown(position, "unknown cursor record"))),
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
enum CursorLine {
    User {
        content: String,
        timestamp: String,
    },
    Assistant {
        content: String,
        timestamp: String,
    },
    Tool {
        name: String,
        arguments: JsonObject,
        result: String,
        timestamp: String,
    },
    #[serde(other)]
    Unknown,
}
