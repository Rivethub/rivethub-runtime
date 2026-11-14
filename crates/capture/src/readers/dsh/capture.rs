use std::collections::HashMap;
use std::path::Path;

use chrono::DateTime;
use chrono::Utc;
use kernel::transcript::TranscriptReaderError;
use serde::Serialize;
use serde_json::value::RawValue;
use wire::JsonObject;
use wire::capture::CaptureMessage;
use wire::message::Role;

use super::super::message::SkippedRecord;
use super::super::message::TranscriptSlice;
use super::super::message::lenient;
use super::super::message::line_number;
use super::super::message::parse_json;
use super::super::message::tool_input;
use super::super::message::unknown;
use super::model::Content;
use super::model::Data;
use super::model::EventKind;
use super::model::Line;
use super::model::Origin;
use crate::records::SubagentLink;

const SOURCE: &str = "dsh-session";

#[derive(Debug, Serialize)]
struct Pointer<'a> {
    source: &'static str,
    session_jsonl_path: &'a str,
    session_jsonl_line: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Call,
    Result { failed: bool },
    Text,
}

struct Row {
    kind: Kind,
    event_id: String,
    call_id: Option<String>,
    role: Role,
    content: String,
    tool_name: Option<String>,
    tool_args: Option<JsonObject>,
    tool_result: Option<String>,
    line: u64,
    at: Option<DateTime<Utc>>,
}

struct Parsed {
    lines: Vec<(u64, Line)>,
    skipped: Vec<SkippedRecord>,
    last: u64,
}

pub(super) fn map_session(
    native_id: &str,
    path: &Path,
    text: &str,
) -> Result<TranscriptSlice, TranscriptReaderError> {
    let Parsed {
        lines,
        mut skipped,
        last,
    } = parse(text)?;
    let session = lines
        .iter()
        .filter(|(_, line)| line.kind == Some(EventKind::Session))
        .filter_map(|(_, line)| line.id.clone().filter(|id| !id.is_empty()))
        .next_back()
        .unwrap_or_else(|| native_id.to_owned());
    let mut rows = Vec::new();
    for (number, line) in lines {
        match row(&session, number, line)? {
            Mapped::Row(row) => rows.push(*row),
            Mapped::Skip(record) => skipped.push(record),
            Mapped::None => {}
        }
    }
    name_results(&mut rows);
    let file = path.display().to_string();
    let messages = fold_results(rows)
        .into_iter()
        .map(|row| message(row, &file))
        .collect::<Result<_, _>>()?;
    Ok(TranscriptSlice {
        messages,
        next_offset: last,
        skipped,
    })
}

pub(super) fn session_title(text: &str) -> Result<Option<String>, TranscriptReaderError> {
    Ok(parse(text)?
        .lines
        .into_iter()
        .filter(|(_, line)| line.kind == Some(EventKind::SessionTitle))
        .filter_map(|(_, line)| line.data.and_then(|data| data.title))
        .rfind(|title| !title.is_empty()))
}

pub(super) fn session_link(
    native_id: &str,
    text: &str,
) -> Result<Option<SubagentLink>, TranscriptReaderError> {
    Ok(parse(text)?
        .lines
        .into_iter()
        .find(|(_, line)| line.kind == Some(EventKind::Session))
        .and_then(|(_, header)| {
            let parent = header.parent_session.filter(|parent| !parent.is_empty())?;
            (header.origin == Some(Origin::Subagent) && parent != native_id).then(|| SubagentLink {
                parent_session_id: parent,
                agent_id: native_id.to_owned(),
                agent_type: None,
                description: None,
                tool_use_id: None,
            })
        }))
}

fn parse(text: &str) -> Result<Parsed, TranscriptReaderError> {
    let mut parsed = Parsed {
        lines: Vec::new(),
        skipped: Vec::new(),
        last: 0,
    };
    for (index, raw) in text.split('\n').enumerate() {
        let number = line_number(index)?;
        parsed.last = number;
        if raw.trim().is_empty() {
            continue;
        }
        match parse_json::<Line>(number, raw.trim()) {
            Ok(line) => parsed.lines.push((number, line)),
            Err(record) => parsed.skipped.push(record),
        }
    }
    Ok(parsed)
}

enum Mapped {
    Row(Box<Row>),
    Skip(SkippedRecord),
    None,
}

fn row(session: &str, number: u64, line: Line) -> Result<Mapped, TranscriptReaderError> {
    let Some(kind) = line
        .kind
        .filter(|kind| !matches!(kind, EventKind::Session | EventKind::Other))
    else {
        return Ok(Mapped::None);
    };
    let data = line.data.unwrap_or_default();
    let Some(event_id) = event_id(session, line.id.as_deref(), line.seq, &data) else {
        return Ok(Mapped::Skip(unknown(
            number,
            format!("dsh {kind:?} event has neither an id nor a seq"),
        )));
    };
    let at = line.time.and_then(DateTime::from_timestamp_millis);
    let base = |kind: Kind, role: Role, content: String| Row {
        kind,
        event_id: event_id.clone(),
        call_id: None,
        role,
        content,
        tool_name: None,
        tool_args: None,
        tool_result: None,
        line: number,
        at,
    };
    let mapped = match kind {
        EventKind::UserMessage => {
            let from_user = data
                .source
                .as_ref()
                .and_then(|source| source.kind.as_deref())
                .is_none_or(|kind| kind.is_empty() || kind == "user");
            let content = data.content.as_deref().map(content_text).transpose()?;
            content
                .flatten()
                .filter(|_| from_user)
                .map(|content| base(Kind::Text, Role::User, content))
        }
        EventKind::AssistantMessage => {
            let raw = match &data.message {
                Some(message) => message.content.as_deref(),
                None => data.content.as_deref(),
            };
            raw.map(content_text)
                .transpose()?
                .flatten()
                .map(|content| base(Kind::Text, Role::Assistant, content))
        }
        EventKind::ToolCall => {
            let name = nonempty(data.name.clone()).unwrap_or_else(|| "unknown".to_owned());
            let raw = data.arguments.as_deref().or(data.args.as_deref());
            Some(Row {
                call_id: nonempty(data.call_id.clone())
                    .or_else(|| nonempty(data.call_id_snake.clone())),
                tool_args: raw.map(tool_input).transpose()?,
                tool_name: Some(name.clone()),
                ..base(Kind::Call, Role::Tool, format!("[tool] {name}"))
            })
        }
        EventKind::ToolResult => Some(result(&data, base)?),
        EventKind::TurnEnd => Some(base(Kind::Text, Role::System, marker("turn/end", &data))),
        EventKind::CompactionStart => Some(base(
            Kind::Text,
            Role::System,
            marker("compaction/start", &data),
        )),
        EventKind::SessionTitle => Some(base(
            Kind::Text,
            Role::System,
            marker("session/title", &data),
        )),
        EventKind::Session | EventKind::Other => None,
    };
    Ok(mapped.map_or(Mapped::None, |row| Mapped::Row(Box::new(row))))
}

fn result(
    data: &Data,
    base: impl Fn(Kind, Role, String) -> Row,
) -> Result<Row, TranscriptReaderError> {
    let (name, source, call_id, call_id_snake, own) = match &data.message {
        Some(message) => (
            &message.name,
            &message.source,
            &message.call_id,
            &message.call_id_snake,
            message.content.as_deref(),
        ),
        None => (
            &data.name,
            &data.source,
            &data.call_id,
            &data.call_id_snake,
            data.content.as_deref(),
        ),
    };
    let content = own.or(data.content.as_deref());
    let source = source.clone().unwrap_or_default();
    let name = nonempty(name.clone())
        .or_else(|| nonempty(data.name.clone()))
        .or_else(|| nonempty(source.name.clone()))
        .unwrap_or_else(|| "unknown".to_owned());
    let parsed = content.map(lenient::<Content>).transpose()?.flatten();
    let nested_error = matches!(
        own.map(lenient::<Content>).transpose()?.flatten(),
        Some(Content::Parts(parts)) if parts.iter().any(|part| part.is_error == Some(true))
    );
    let failed = data.error.is_some() || nested_error;
    let label = outcome(failed);
    Ok(Row {
        call_id: nonempty(call_id.clone())
            .or_else(|| nonempty(call_id_snake.clone()))
            .or_else(|| nonempty(source.call_id))
            .or_else(|| nonempty(source.call_id_snake))
            .or_else(|| nonempty(data.call_id.clone()))
            .or_else(|| nonempty(data.call_id_snake.clone())),
        tool_result: content.map(|raw| result_text(raw, parsed.as_ref())),
        tool_name: Some(name.clone()),
        ..base(
            Kind::Result { failed },
            Role::Tool,
            format!("[{label}] {name}"),
        )
    })
}

fn event_id(session: &str, id: Option<&str>, seq: Option<i64>, data: &Data) -> Option<String> {
    let message = data.message.as_ref();
    let uuid = [
        id,
        data.id.as_deref(),
        data.call_id.as_deref(),
        data.call_id_snake.as_deref(),
        message.and_then(|message| message.id.as_deref()),
        message.and_then(|message| message.call_id.as_deref()),
        message.and_then(|message| message.call_id_snake.as_deref()),
    ]
    .into_iter()
    .flatten()
    .find(|uuid| !uuid.is_empty());
    match (uuid, seq) {
        (Some(uuid), _) => Some(format!("dsh:{session}:{uuid}")),
        (None, Some(seq)) => Some(format!("dsh:{session}:seq:{seq}")),
        (None, None) => None,
    }
}

fn content_text(raw: &RawValue) -> Result<Option<String>, TranscriptReaderError> {
    Ok(lenient::<Content>(raw)?.as_ref().and_then(text_of))
}

fn text_of(content: &Content) -> Option<String> {
    match content {
        Content::Text(text) => nonempty(Some(text.trim().to_owned())),
        Content::Parts(parts) => {
            let mut texts = Vec::new();
            for part in parts {
                match part.kind.as_deref() {
                    Some("text") => texts.extend(trimmed(part.text.as_deref())),
                    Some("think" | "reasoning" | "thinking") => {
                        let body = part
                            .text
                            .as_deref()
                            .or(part.think.as_deref())
                            .or(part.reasoning.as_deref());
                        texts.extend(trimmed(body).map(|body| format!("[thinking] {body}")));
                    }
                    Some(_) | None => {}
                }
            }
            nonempty(Some(texts.join("\n").trim().to_owned()))
        }
    }
}

fn result_text(raw: &RawValue, parsed: Option<&Content>) -> String {
    if let Some(Content::Parts(parts)) = parsed {
        let mut texts = Vec::new();
        for part in parts {
            match part.kind.as_deref() {
                Some("tool-result" | "tool_result") => {
                    texts.extend(part.content.as_ref().and_then(text_of));
                }
                Some("text") => texts.extend(trimmed(part.text.as_deref())),
                Some(_) | None => {}
            }
        }
        if !texts.is_empty() {
            return texts.join("\n");
        }
    }
    parsed
        .and_then(text_of)
        .unwrap_or_else(|| raw.get().to_owned())
}

fn name_results(rows: &mut [Row]) {
    let names: HashMap<String, String> = rows
        .iter()
        .filter(|row| row.kind == Kind::Call)
        .filter_map(|row| Some((row.call_id.clone()?, row.tool_name.clone()?)))
        .filter(|(_, name)| name != "unknown")
        .collect();
    for row in rows.iter_mut() {
        let Kind::Result { failed } = row.kind else {
            continue;
        };
        let Some(name) = row.call_id.as_ref().and_then(|id| names.get(id)) else {
            continue;
        };
        if row
            .tool_name
            .as_deref()
            .is_none_or(|current| current == "unknown")
        {
            row.content = format!("[{}] {name}", outcome(failed));
            row.tool_name = Some(name.clone());
        }
    }
}

fn fold_results(rows: Vec<Row>) -> Vec<Row> {
    let calls: HashMap<String, usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row.kind == Kind::Call)
        .filter_map(|(index, row)| Some((row.call_id.clone()?, index)))
        .collect();
    let mut slots: Vec<Option<Row>> = rows.into_iter().map(Some).collect();
    for index in 0..slots.len() {
        let target = match slots.get(index).and_then(Option::as_ref) {
            Some(row) if matches!(row.kind, Kind::Result { .. }) => {
                row.call_id.as_ref().and_then(|id| calls.get(id)).copied()
            }
            Some(_) | None => None,
        };
        let Some(target) = target else {
            continue;
        };
        let Some(result) = slots.get_mut(index).and_then(Option::take) else {
            continue;
        };
        let body = result
            .tool_result
            .filter(|body| !body.is_empty())
            .unwrap_or(result.content);
        if let Some(call) = slots.get_mut(target).and_then(Option::as_mut)
            && !body.is_empty()
            && call.tool_result.is_none()
        {
            call.tool_result = Some(body);
            if call
                .tool_name
                .as_deref()
                .is_none_or(|name| name == "unknown")
            {
                call.tool_name = result.tool_name;
            }
        }
    }
    slots.into_iter().flatten().collect()
}

fn message(row: Row, file: &str) -> Result<CaptureMessage, TranscriptReaderError> {
    let metadata = JsonObject::from_serialize(&Pointer {
        source: SOURCE,
        session_jsonl_path: file,
        session_jsonl_line: row.line,
    })
    .map_err(|error| TranscriptReaderError::Read {
        message: format!("dsh capture JSON failed: {error}"),
        source: Box::new(error),
    })?;
    Ok(CaptureMessage {
        event_id: row.event_id,
        role: row.role,
        content: row.content,
        tool_name: row.tool_name,
        tool_args: row.tool_args,
        tool_result: row.tool_result,
        metadata: Some(metadata),
        created_at: row.at,
    })
}

fn outcome(failed: bool) -> &'static str {
    if failed {
        "tool-failure"
    } else {
        "tool-result"
    }
}

fn marker(kind: &str, data: &Data) -> String {
    let reason = data
        .reason
        .as_ref()
        .and_then(|reason| nonempty(reason.kind.clone()))
        .or_else(|| nonempty(data.title.clone()))
        .or_else(|| nonempty(data.preset.clone()));
    match reason {
        Some(reason) => format!("[dsh.{kind}] {reason}"),
        None => format!("[dsh.{kind}]"),
    }
}

fn trimmed(text: Option<&str>) -> Option<String> {
    nonempty(text.map(|text| text.trim().to_owned()))
}

fn nonempty(text: Option<String>) -> Option<String> {
    text.filter(|text| !text.is_empty())
}
