mod capture;
mod subagent;

use jiff::Timestamp;
use std::collections::BTreeMap;
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
use super::message::StoreRole;
use super::message::TextTurn;
use super::message::ToolFields;
use super::message::TranscriptSlice;
use super::message::TypeTag;
use super::message::arguments;
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
use super::scan::read_whole;
use crate::store_path::DirState;
use crate::store_path::Root;
use crate::store_path::dir_state;
use crate::store_path::jsonl;
use crate::store_path::session_component;
use crate::store_path::utf8_stem;

const FORMAT: &str = "codex-rollout";

#[derive(Debug)]
pub struct CodexTranscriptReader {
    root: PathBuf,
}

impl CodexTranscriptReader {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub async fn messages(
        &self,
        session: &SessionId,
        offset: u64,
    ) -> Result<TranscriptSlice, TranscriptReaderError> {
        let Some(native_id) = native(session, HarnessId::Codex).map(str::to_owned) else {
            return Ok(TranscriptSlice::empty(offset));
        };
        let root = self.root.clone();
        let session = session.clone();
        let slice = blocking(move || load(&root, &session, &native_id, offset)).await?;
        report(FORMAT, &slice.skipped);
        Ok(slice)
    }

    pub async fn capture(
        &self,
        session: &SessionId,
    ) -> Result<TranscriptSlice, TranscriptReaderError> {
        let Some(native_id) = native(session, HarnessId::Codex).map(str::to_owned) else {
            return Ok(TranscriptSlice::empty(0));
        };
        let root = self.root.clone();
        let slice = blocking(move || {
            session_component(&native_id)?;
            let root = Root::open(&root)?;
            let Some((_, path)) = rollout_files(&root)?
                .into_iter()
                .find(|(id, _)| *id == native_id)
            else {
                return Ok(TranscriptSlice::empty(0));
            };
            match read_whole(&root, &path)? {
                Some(text) => capture::map_rollout(&native_id, &path, &text),
                None => Ok(TranscriptSlice::empty(0)),
            }
        })
        .await?;
        report(FORMAT, &slice.skipped);
        Ok(slice)
    }
}

#[async_trait]
impl TranscriptReader for CodexTranscriptReader {
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

pub(crate) async fn subagent_link(
    store: PathBuf,
    native_id: String,
) -> Result<Option<crate::records::SubagentLink>, TranscriptReaderError> {
    blocking(move || subagent::link(&store, &native_id)).await
}

fn list_sessions(store: &Path) -> Result<Vec<SessionId>, TranscriptReaderError> {
    let mut sessions = Vec::new();
    for (id, _) in rollout_files(&Root::open(store)?)? {
        sessions.push(SessionId::new(HarnessId::Codex, &id));
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
    let Some((_, path)) = rollout_files(&root)?
        .into_iter()
        .find(|(id, _)| id == native_id)
    else {
        return Ok(TranscriptSlice::empty(offset));
    };
    let Some(text) = read_optional(&root, &path)? else {
        return Ok(TranscriptSlice::empty(offset));
    };
    map_text(session, native_id, &text, offset)
}

fn rollout_files(root: &Root) -> Result<Vec<(String, PathBuf)>, TranscriptReaderError> {
    let sessions = root.canonical().join("sessions");
    if dir_state(root, &sessions)? == DirState::Absent {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for year in root.directories(&sessions)? {
        for month in root.directories(&year)? {
            for day in root.directories(&month)? {
                for file in root.files(&day)? {
                    if jsonl(&file)
                        && let Some(id) = codex_id(&utf8_stem(&file)?)
                    {
                        found.push((id, file));
                    }
                }
            }
        }
    }
    Ok(found)
}

pub(crate) fn codex_id(stem: &str) -> Option<String> {
    let rest = stem.strip_prefix("rollout-")?;
    let split_at = rest.len().checked_sub(36)?;
    let (prefix, uuid) = rest.split_at(split_at);
    if !prefix.ends_with('-') {
        return None;
    }
    uuid::Uuid::parse_str(uuid)
        .is_ok()
        .then_some(uuid.to_owned())
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
    let mut pending = BTreeMap::new();
    for record in &records {
        push_mapped(
            map_record(session, native_id, record, &mut pending)?,
            &mut drafts,
            &mut skipped,
        );
    }
    for (_, call) in pending {
        push_mapped(leftover_call(session, call)?, &mut drafts, &mut skipped);
    }
    Ok(assemble(offset, last, drafts, skipped))
}

fn map_record(
    session: &SessionId,
    native_id: &str,
    record: &super::scan::RawLine,
    pending: &mut BTreeMap<String, PendingCall>,
) -> Result<Mapped, TranscriptReaderError> {
    let line = match parse_json::<CodexLine>(record.number, &record.text) {
        Ok(line) => line,
        Err(skipped) => return Ok(Mapped::Skip(skipped)),
    };
    match line.body {
        CodexBody::SessionMeta { payload } => Ok(session_meta(record.number, native_id, &payload)),
        CodexBody::EventMsg { payload } => {
            event_msg(session, record.number, &line.timestamp, payload)
        }
        CodexBody::ResponseItem { payload } => {
            response_item(session, record.number, &line.timestamp, payload, pending)
        }
        CodexBody::TurnContext | CodexBody::Compacted => Ok(Mapped::None),
        CodexBody::Unknown => Ok(Mapped::Skip(unknown(record.number, type_detail(record)))),
    }
}

fn session_meta(position: u64, native_id: &str, payload: &SessionMeta) -> Mapped {
    if payload.id == native_id {
        Mapped::None
    } else {
        Mapped::Skip(super::message::unparseable(
            position,
            format!(
                "session meta id {} does not match the rollout file {native_id}",
                payload.id
            ),
        ))
    }
}

fn event_msg(
    session: &SessionId,
    position: u64,
    at: &str,
    payload: EventBody,
) -> Result<Mapped, TranscriptReaderError> {
    match payload {
        EventBody::UserMessage { message } => text_turn(&TextTurn {
            session,
            position,
            role: Role::User,
            content: &message,
            at,
            format: FORMAT,
        }),
        EventBody::ItemCompleted { item } => completed_item(session, position, at, &item),
        EventBody::AgentMessage | EventBody::TokenCount => Ok(Mapped::None),
        EventBody::Other => Ok(Mapped::Skip(unknown(position, "unknown codex event"))),
    }
}

fn completed_item(
    session: &SessionId,
    position: u64,
    at: &str,
    item: &CompletedItem,
) -> Result<Mapped, TranscriptReaderError> {
    match item {
        CompletedItem::UserMessage { content } => text_turn(&TextTurn {
            session,
            position,
            role: Role::User,
            content: &joined(content.iter().filter_map(ItemPart::text)),
            at,
            format: FORMAT,
        }),
        CompletedItem::AgentMessage => Ok(Mapped::None),
        CompletedItem::Other => Ok(Mapped::Skip(unknown(position, "unknown codex item"))),
    }
}

fn response_item(
    session: &SessionId,
    position: u64,
    at: &str,
    payload: ResponseBody,
    pending: &mut BTreeMap<String, PendingCall>,
) -> Result<Mapped, TranscriptReaderError> {
    match payload {
        ResponseBody::Message { role, content } => {
            response_message(session, position, at, role, &content)
        }
        ResponseBody::FunctionCall {
            name,
            arguments,
            call_id,
        } => {
            drop(pending.insert(
                call_id,
                PendingCall {
                    name,
                    arguments,
                    at: at.to_owned(),
                    position,
                },
            ));
            Ok(Mapped::None)
        }
        ResponseBody::FunctionCallOutput { call_id, output } => {
            emit_output(session, position, at, &call_id, &output, pending)
        }
        ResponseBody::Other => Ok(Mapped::Skip(unknown(
            position,
            "unknown codex response item",
        ))),
    }
}

fn response_message(
    session: &SessionId,
    position: u64,
    at: &str,
    role: StoreRole,
    content: &[CodexPart],
) -> Result<Mapped, TranscriptReaderError> {
    let Some(role) = role.known() else {
        return Ok(Mapped::Skip(super::message::SkippedRecord {
            position,
            rule: super::message::SkipRule::UnrecognizedSpeaker,
            detail: "codex message role".to_owned(),
        }));
    };
    if role == Role::User {
        return Ok(Mapped::None);
    }
    let text = joined(content.iter().filter_map(CodexPart::text));
    text_turn(&TextTurn {
        session,
        position,
        role,
        content: &text,
        at,
        format: FORMAT,
    })
}

fn emit_output(
    session: &SessionId,
    position: u64,
    at: &str,
    call_id: &str,
    output: &str,
    pending: &mut BTreeMap<String, PendingCall>,
) -> Result<Mapped, TranscriptReaderError> {
    let Some(call) = pending.remove(call_id) else {
        return tool_turn(
            &TextTurn {
                session,
                position,
                role: Role::Tool,
                content: output,
                at,
                format: FORMAT,
            },
            ToolFields {
                name: None,
                arguments: None,
                result: Some(output.to_owned()),
            },
        );
    };
    let arguments = match arguments(position, &call.arguments) {
        Ok(arguments) => arguments,
        Err(skipped) => return Ok(Mapped::Skip(skipped)),
    };
    tool_turn(
        &TextTurn {
            session,
            position,
            role: Role::Tool,
            content: output,
            at,
            format: FORMAT,
        },
        ToolFields {
            name: Some(call.name),
            arguments: Some(arguments),
            result: Some(output.to_owned()),
        },
    )
}

fn leftover_call(session: &SessionId, call: PendingCall) -> Result<Mapped, TranscriptReaderError> {
    let content = format!("[tool call] {}", call.name);
    let arguments = match arguments(call.position, &call.arguments) {
        Ok(arguments) => arguments,
        Err(skipped) => return Ok(Mapped::Skip(skipped)),
    };
    tool_turn(
        &TextTurn {
            session,
            position: call.position,
            role: Role::Tool,
            content: &content,
            at: &call.at,
            format: FORMAT,
        },
        ToolFields {
            name: Some(call.name),
            arguments: Some(arguments),
            result: None,
        },
    )
}

fn joined<'a>(pieces: impl Iterator<Item = &'a str>) -> String {
    pieces.collect::<Vec<_>>().join("\n")
}

fn type_detail(record: &super::scan::RawLine) -> String {
    match parse_json::<TypeTag>(record.number, &record.text) {
        Ok(tag) => tag.record_type,
        Err(skipped) => skipped.detail,
    }
}

struct PendingCall {
    name: String,
    arguments: String,
    at: String,
    position: u64,
}

#[derive(Debug, Deserialize)]
struct CodexLine {
    timestamp: String,
    #[serde(flatten)]
    body: CodexBody,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CodexBody {
    SessionMeta {
        payload: SessionMeta,
    },
    ResponseItem {
        payload: ResponseBody,
    },
    EventMsg {
        payload: EventBody,
    },
    TurnContext,
    Compacted,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Deserialize)]
struct SessionMeta {
    id: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ResponseBody {
    Message {
        role: StoreRole,
        content: Vec<CodexPart>,
    },
    FunctionCall {
        name: String,
        arguments: String,
        call_id: String,
    },
    FunctionCallOutput {
        call_id: String,
        output: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CodexPart {
    OutputText {
        text: String,
    },
    InputText {
        text: String,
    },
    #[serde(other)]
    Other,
}

impl CodexPart {
    fn text(&self) -> Option<&str> {
        match self {
            Self::OutputText { text } | Self::InputText { text } => Some(text),
            Self::Other => None,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum EventBody {
    UserMessage {
        message: String,
    },
    AgentMessage,
    TokenCount,
    ItemCompleted {
        item: CompletedItem,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum CompletedItem {
    UserMessage {
        content: Vec<ItemPart>,
    },
    AgentMessage,
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ItemPart {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}

impl ItemPart {
    fn text(&self) -> Option<&str> {
        match self {
            Self::Text { text } => Some(text),
            Self::Other => None,
        }
    }
}
