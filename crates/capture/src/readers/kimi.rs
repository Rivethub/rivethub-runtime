use jiff::Timestamp;
use std::collections::BTreeMap;
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

const FORMAT: &str = "kimi-wire";
const DIR_PREFIX: &str = "session_";

#[derive(Debug)]
pub struct KimiTranscriptReader {
    root: PathBuf,
}

impl KimiTranscriptReader {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub async fn messages(
        &self,
        session: &SessionId,
        offset: u64,
    ) -> Result<TranscriptSlice, TranscriptReaderError> {
        let Some(native_id) = native(session, HarnessId::KimiCode).map(str::to_owned) else {
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
impl TranscriptReader for KimiTranscriptReader {
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
    for (id, _) in session_dirs(&Root::open(store)?)? {
        sessions.push(SessionId::new(HarnessId::KimiCode, &id));
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
    let wanted = native_id.strip_prefix(DIR_PREFIX).unwrap_or(native_id);
    let root = Root::open(store)?;
    let Some(dir) = session_dirs(&root)?
        .into_iter()
        .find_map(|(id, path)| (id == wanted).then_some(path))
    else {
        return Ok(TranscriptSlice::empty(offset));
    };
    let wire = dir.join("agents").join("main").join("wire.jsonl");
    let Some(text) = read_optional(&root, &wire)? else {
        return Ok(TranscriptSlice::empty(offset));
    };
    let mut slice = map_wire(session, &text, offset)?;
    if offset == 0
        && let Some(index) = read_optional(&root, &dir.join("session_index.jsonl"))?
    {
        slice.skipped.extend(index_skips(&index)?);
    }
    Ok(slice)
}

fn session_dirs(root: &Root) -> Result<Vec<(String, PathBuf)>, TranscriptReaderError> {
    let sessions = root.canonical().join("sessions");
    if dir_state(root, &sessions)? == DirState::Absent {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for work in root.directories(&sessions)? {
        for session_dir in root.directories(&work)? {
            let Some(name) = dir_name(&session_dir)? else {
                continue;
            };
            let Some(id) = name.strip_prefix(DIR_PREFIX) else {
                continue;
            };
            if uuid::Uuid::parse_str(id).is_err() {
                continue;
            }
            let wire = session_dir.join("agents").join("main").join("wire.jsonl");
            if root.classify(&wire)? == Child::File {
                found.push((id.to_owned(), session_dir));
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

fn map_wire(
    session: &SessionId,
    text: &str,
    offset: u64,
) -> Result<TranscriptSlice, TranscriptReaderError> {
    let (records, last) = lines(text)?;
    let mut drafts = Vec::new();
    let mut skipped = Vec::new();
    let mut pending = BTreeMap::new();
    for record in &records {
        let mapped = match parse_json::<KimiWire>(record.number, &record.text) {
            Ok(line) => map_line(session, record.number, line, &mut pending)?,
            Err(skipped) => Mapped::Skip(skipped),
        };
        push_mapped(mapped, &mut drafts, &mut skipped);
    }
    Ok(assemble(offset, last, drafts, skipped))
}

fn map_line(
    session: &SessionId,
    position: u64,
    line: KimiWire,
    pending: &mut BTreeMap<String, PendingTool>,
) -> Result<Mapped, TranscriptReaderError> {
    match line {
        KimiWire::User { text, timestamp } => text_turn(&TextTurn {
            session,
            position,
            role: Role::User,
            content: &text,
            at: &timestamp,
            format: FORMAT,
        }),
        KimiWire::Assistant { text, timestamp } => text_turn(&TextTurn {
            session,
            position,
            role: Role::Assistant,
            content: &text,
            at: &timestamp,
            format: FORMAT,
        }),
        KimiWire::ToolCall {
            id,
            name,
            arguments,
        } => {
            drop(pending.insert(id, PendingTool { name, arguments }));
            Ok(Mapped::None)
        }
        KimiWire::ToolResult {
            id,
            text,
            timestamp,
        } => {
            let pending = pending.remove(&id);
            let (name, arguments) =
                pending.map_or((None, None), |tool| (Some(tool.name), Some(tool.arguments)));
            let content = text.clone();
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
                    name,
                    arguments,
                    result: Some(text),
                },
            )
        }
        KimiWire::Unknown => Ok(Mapped::Skip(unknown(position, "unknown kimi record"))),
    }
}

fn index_skips(text: &str) -> Result<Vec<super::message::SkippedRecord>, TranscriptReaderError> {
    let (records, _) = lines(text)?;
    let mut skipped = Vec::new();
    for record in records {
        match parse_json::<KimiIndex>(record.number, &record.text) {
            Ok(KimiIndex::Session { session_id }) if !session_id.is_empty() => {}
            Ok(KimiIndex::Session { .. }) => skipped.push(super::message::unparseable(
                record.number,
                "session index id is empty",
            )),
            Ok(KimiIndex::Unknown) => {
                skipped.push(unknown(record.number, "unknown kimi index record"));
            }
            Err(skipped_record) => skipped.push(skipped_record),
        }
    }
    Ok(skipped)
}

struct PendingTool {
    name: String,
    arguments: JsonObject,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum KimiWire {
    User {
        text: String,
        timestamp: String,
    },
    Assistant {
        text: String,
        timestamp: String,
    },
    ToolCall {
        id: String,
        name: String,
        arguments: JsonObject,
    },
    ToolResult {
        id: String,
        text: String,
        timestamp: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum KimiIndex {
    Session {
        session_id: String,
    },
    #[serde(other)]
    Unknown,
}
