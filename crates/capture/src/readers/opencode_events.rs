use chrono::DateTime;
use chrono::SecondsFormat;
use kernel::transcript::TranscriptReaderError;
use serde::Deserialize;
use wire::JsonObject;
use wire::message::Role;
use wire::session_id::SessionId;

use super::harness_db::OpencodeEvent;
use super::message::Mapped;
use super::message::SkipRule;
use super::message::SkippedRecord;
use super::message::TextTurn;
use super::message::ToolFields;
use super::message::TranscriptSlice;
use super::message::assemble;
use super::message::parse_json;
use super::message::push_mapped;
use super::message::row_offset;
use super::message::row_position;
use super::message::text_turn;
use super::message::tool_turn;
use super::message::unknown;
use super::opencode::Mode;

pub(crate) const FORMAT: &str = "opencode-session-message";
const SLOTS: u64 = 1 << 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq, strum::EnumString)]
#[strum(serialize_all = "kebab-case")]
enum MessageType {
    User,
    Assistant,
    System,
    Synthetic,
    Idle,
    AgentSwitched,
    ModelSwitched,
    LocationSwitched,
}

#[derive(Debug, Deserialize)]
struct TextData {
    text: String,
}

#[derive(Debug, Deserialize)]
struct AssistantData {
    time: AssistantTime,
    content: Vec<Content>,
}

#[derive(Debug, Deserialize)]
struct AssistantTime {
    completed: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Content {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
    },
    Tool {
        name: String,
        state: ToolState,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum ToolState {
    Completed {
        input: JsonObject,
        content: Vec<ToolOutput>,
    },
    Error {
        input: JsonObject,
        error: ToolError,
    },
    Streaming,
    Running,
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ToolOutput {
    Text {
        text: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct ToolError {
    message: String,
}

#[derive(Debug)]
enum Record {
    Text(Role, TextData),
    Assistant(AssistantData),
    Marker,
    Skip(SkippedRecord),
}

#[derive(Debug, Clone, Copy)]
struct Row<'a> {
    session: &'a SessionId,
    base: u64,
    at: &'a str,
    mode: Mode,
}

pub(crate) fn first_seq(offset: u64) -> Result<i64, TranscriptReaderError> {
    row_offset(offset / SLOTS)
}

pub(crate) fn map_events(
    session: &SessionId,
    rows: &[OpencodeEvent],
    offset: u64,
    mode: Mode,
) -> Result<TranscriptSlice, TranscriptReaderError> {
    let mut drafts = Vec::new();
    let mut skipped = Vec::new();
    let mut last = offset;
    for (index, row) in rows.iter().enumerate() {
        let base = slot_base(row.seq)?;
        let record = record(base, row);
        if index + 1 == rows.len() && streaming(&record) {
            break;
        }
        last = last.max(base.saturating_add(SLOTS - 1));
        for mapped in map_record(session, base, row.time_created, record, mode)? {
            push_mapped(mapped, &mut drafts, &mut skipped);
        }
    }
    Ok(assemble(offset, last, drafts, skipped))
}

fn slot_base(seq: i64) -> Result<u64, TranscriptReaderError> {
    row_position(seq)?
        .checked_mul(SLOTS)
        .ok_or_else(|| read_error(format!("opencode message sequence {seq} does not fit")))
}

fn record(base: u64, row: &OpencodeEvent) -> Record {
    let position = base + 1;
    let kind = match row.record_type.parse::<MessageType>() {
        Ok(kind) => kind,
        Err(error) => {
            return Record::Skip(unknown(
                position,
                format!("opencode message type {}: {error}", row.record_type),
            ));
        }
    };
    let parsed = match kind {
        MessageType::User => {
            parse_json(position, &row.data).map(|data| Record::Text(Role::User, data))
        }
        MessageType::System | MessageType::Synthetic => {
            parse_json(position, &row.data).map(|data| Record::Text(Role::System, data))
        }
        MessageType::Assistant => assistant_data(position, row).map(Record::Assistant),
        MessageType::Idle
        | MessageType::AgentSwitched
        | MessageType::ModelSwitched
        | MessageType::LocationSwitched => Ok(Record::Marker),
    };
    match parsed {
        Ok(record) => record,
        Err(skipped) => Record::Skip(skipped),
    }
}

fn assistant_data(position: u64, row: &OpencodeEvent) -> Result<AssistantData, SkippedRecord> {
    let mut data: AssistantData = parse_json(position, &row.data)?;
    if let Some(parts) = row.parts.as_deref() {
        let mut content: Vec<Content> = parse_json(position, parts)?;
        content.append(&mut data.content);
        data.content = content;
    }
    Ok(data)
}

fn streaming(record: &Record) -> bool {
    matches!(record, Record::Assistant(data) if data.time.completed.is_none())
}

fn map_record(
    session: &SessionId,
    base: u64,
    time_created: i64,
    record: Record,
    mode: Mode,
) -> Result<Vec<Mapped>, TranscriptReaderError> {
    let position = base + 1;
    let Some(at) = DateTime::from_timestamp_millis(time_created) else {
        return Ok(vec![Mapped::Skip(SkippedRecord {
            position,
            rule: SkipRule::UnparseableTimestamp,
            detail: format!("time_created {time_created} is not a millisecond instant"),
        })]);
    };
    let at = at.to_rfc3339_opts(SecondsFormat::Millis, true);
    let row = Row {
        session,
        base,
        at: &at,
        mode,
    };
    match record {
        Record::Text(role, data) => Ok(vec![text_turn(&turn(row, position, role, &data.text))?]),
        Record::Assistant(data) => assistant(row, data.content),
        Record::Marker => Ok(Vec::new()),
        Record::Skip(skipped) => Ok(vec![Mapped::Skip(skipped)]),
    }
}

fn assistant(row: Row<'_>, content: Vec<Content>) -> Result<Vec<Mapped>, TranscriptReaderError> {
    let mut mapped = Vec::with_capacity(content.len());
    for (index, item) in content.into_iter().enumerate() {
        let position = item_position(row.base, index)?;
        mapped.push(match item {
            Content::Text { text } => text_turn(&turn(
                row,
                position,
                Role::Assistant,
                reply_text(row.mode, &text),
            ))?,
            Content::Tool { name, state } => tool(row, position, name, state)?,
            Content::Reasoning { text } => thinking(row, position, &text)?,
            Content::Other => Mapped::Skip(unknown(position, "unknown opencode content")),
        });
    }
    Ok(mapped)
}

fn reply_text(mode: Mode, text: &str) -> &str {
    match mode {
        Mode::Capture => text.trim(),
        Mode::Transcript => text,
    }
}

fn thinking(row: Row<'_>, position: u64, text: &str) -> Result<Mapped, TranscriptReaderError> {
    let text = text.trim();
    if row.mode == Mode::Transcript || text.is_empty() {
        return Ok(Mapped::None);
    }
    text_turn(&turn(
        row,
        position,
        Role::Assistant,
        &format!("[thinking] {text}"),
    ))
}

fn tool(
    row: Row<'_>,
    position: u64,
    name: String,
    state: ToolState,
) -> Result<Mapped, TranscriptReaderError> {
    let (input, result, failed) = match state {
        ToolState::Completed { input, content } => (input, output_text(content), false),
        ToolState::Error { input, error } => (input, error.message, true),
        ToolState::Streaming | ToolState::Running => return Ok(Mapped::None),
        ToolState::Other => {
            return Ok(Mapped::Skip(unknown(
                position,
                "unknown opencode tool status",
            )));
        }
    };
    let content = if failed && row.mode == Mode::Capture {
        format!("[tool-failure] {name}")
    } else {
        result.clone()
    };
    tool_turn(
        &turn(row, position, Role::Tool, &content),
        ToolFields {
            name: Some(name),
            arguments: Some(input),
            result: Some(result),
        },
    )
}

fn output_text(content: Vec<ToolOutput>) -> String {
    content
        .into_iter()
        .filter_map(|output| match output {
            ToolOutput::Text { text } => Some(text),
            ToolOutput::Other => None,
        })
        .collect()
}

fn turn<'a>(row: Row<'a>, position: u64, role: Role, content: &'a str) -> TextTurn<'a> {
    TextTurn {
        session: row.session,
        position,
        role,
        content,
        at: row.at,
        format: FORMAT,
    }
}

fn item_position(base: u64, index: usize) -> Result<u64, TranscriptReaderError> {
    let slot = u64::try_from(index)
        .map_err(|error| read_error(format!("opencode content index does not fit: {error}")))?
        .saturating_add(1);
    if slot >= SLOTS {
        return Err(read_error(format!(
            "opencode message holds more than {} items",
            SLOTS - 1
        )));
    }
    Ok(base + slot)
}

fn read_error(message: String) -> TranscriptReaderError {
    TranscriptReaderError::Read {
        source: message.clone().into(),
        message,
    }
}
