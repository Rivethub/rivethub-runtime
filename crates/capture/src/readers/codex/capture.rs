use std::collections::HashMap;
use std::path::Path;

use chrono::DateTime;
use chrono::Utc;
use kernel::transcript::TranscriptReaderError;
use serde::Deserialize;
use serde::Serialize;
use serde_json::value::RawValue;
use serde_with::DefaultOnError;
use serde_with::serde_as;
use wire::JsonObject;
use wire::capture::CaptureMessage;
use wire::message::Role;

use super::super::message::StoreRole;
use super::super::message::TranscriptSlice;
use super::super::message::lenient;
use super::super::message::line_number;
use super::super::message::parse_json;
use super::super::message::tool_input;
use crate::listing::codex::injected;

const SOURCE: &str = "codex-rollout";

#[serde_as]
#[derive(Debug, Deserialize)]
struct Line {
    #[serde_as(as = "DefaultOnError")]
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    timestamp: Option<DateTime<Utc>>,
    #[serde(default)]
    payload: Option<Payload>,
}

#[serde_as]
#[derive(Debug, Deserialize)]
struct Payload {
    #[serde_as(as = "DefaultOnError")]
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    id: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    call_id: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    role: Option<StoreRole>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    name: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    tool: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    content: Option<Box<RawValue>>,
    #[serde(default)]
    summary: Option<Box<RawValue>>,
    #[serde(default)]
    input: Option<Box<RawValue>>,
    #[serde(default)]
    arguments: Option<Box<RawValue>>,
    #[serde(default)]
    output: Option<Box<RawValue>>,
    #[serde(default)]
    error: Option<Box<RawValue>>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum Content {
    Text(String),
    Blocks(Vec<Block>),
}

#[serde_as]
#[derive(Debug, Deserialize)]
struct Block {
    #[serde_as(as = "DefaultOnError")]
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    text: Option<String>,
}

#[derive(Debug, Serialize)]
struct Pointer<'a> {
    source: &'static str,
    session_jsonl_path: &'a str,
    session_jsonl_line: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_use_id: Option<&'a str>,
}

struct Row {
    role: Role,
    content: String,
    tool_name: Option<String>,
    tool_args: Option<JsonObject>,
    tool_result: Option<String>,
}

pub(super) fn map_rollout(
    native_id: &str,
    path: &Path,
    text: &str,
) -> Result<TranscriptSlice, TranscriptReaderError> {
    let file = path.display().to_string();
    let mut session = native_id.to_owned();
    let mut tools: HashMap<String, String> = HashMap::new();
    let mut messages = Vec::new();
    let mut skipped = Vec::new();
    let mut last = 0;
    for (index, raw) in text.split('\n').enumerate() {
        let number = line_number(index)?;
        last = number;
        if raw.trim().is_empty() {
            continue;
        }
        let line = match parse_json::<Line>(number, raw.trim()) {
            Ok(line) => line,
            Err(record) => {
                skipped.push(record);
                continue;
            }
        };
        let Some(payload) = line.payload else {
            continue;
        };
        match line.kind.as_deref() {
            Some("session_meta") => {
                if let Some(id) = payload.id.filter(|id| uuid::Uuid::parse_str(id).is_ok()) {
                    session = id;
                }
            }
            Some("response_item") => {
                let event_id = payload
                    .id
                    .clone()
                    .filter(|id| !id.is_empty())
                    .or_else(|| payload.call_id.clone().filter(|id| !id.is_empty()))
                    .unwrap_or_else(|| format!("codex:{session}:line:{index}"));
                let Some(row) = fold(&payload, &mut tools)? else {
                    continue;
                };
                messages.push(CaptureMessage {
                    event_id,
                    role: row.role,
                    content: row.content,
                    tool_name: row.tool_name,
                    tool_args: row.tool_args,
                    tool_result: row.tool_result,
                    metadata: Some(pointer(
                        &file,
                        number,
                        (row.role == Role::Tool)
                            .then_some(payload.call_id.as_deref())
                            .flatten()
                            .filter(|id| !id.is_empty()),
                    )?),
                    created_at: line.timestamp,
                });
            }
            Some(_) | None => {}
        }
    }
    Ok(TranscriptSlice {
        messages,
        next_offset: last,
        skipped,
    })
}

fn fold(
    payload: &Payload,
    tools: &mut HashMap<String, String>,
) -> Result<Option<Row>, TranscriptReaderError> {
    let call_id = payload
        .call_id
        .clone()
        .filter(|id| !id.is_empty())
        .or_else(|| payload.id.clone().filter(|id| !id.is_empty()));
    Ok(match payload.kind.as_deref() {
        Some("message") => {
            let (role, wanted) = match payload.role {
                Some(StoreRole::User) => (Role::User, "input_text"),
                Some(StoreRole::Assistant) => (Role::Assistant, "output_text"),
                Some(StoreRole::System | StoreRole::Tool | StoreRole::Other) | None => {
                    return Ok(None);
                }
            };
            message_text(payload.content.as_deref(), wanted)?.map(|content| text_row(role, content))
        }
        Some("reasoning") => reasoning_text(payload)?
            .map(|text| text_row(Role::Assistant, format!("[thinking] {text}"))),
        Some("function_call" | "custom_tool_call") => {
            let name = tool_name(payload);
            if let Some(id) = call_id {
                tools.insert(id, name.clone());
            }
            let raw = payload.input.as_deref().or(payload.arguments.as_deref());
            let tool_args = raw.map(tool_input).transpose()?;
            Some(Row {
                role: Role::Tool,
                content: format!("[tool] {name}"),
                tool_name: Some(name),
                tool_args,
                tool_result: None,
            })
        }
        Some("function_call_output" | "custom_tool_call_output") => {
            let name = call_id
                .and_then(|id| tools.get(&id).cloned())
                .unwrap_or_else(|| "unknown".to_owned());
            let failed = payload.error.as_deref().is_some_and(failure);
            let outcome = if failed {
                "tool-failure"
            } else {
                "tool-result"
            };
            Some(Row {
                role: Role::Tool,
                content: format!("[{outcome}] {name}"),
                tool_name: Some(name),
                tool_args: None,
                tool_result: payload
                    .output
                    .as_deref()
                    .or(payload.content.as_deref())
                    .map(result_text)
                    .transpose()?,
            })
        }
        Some(_) | None => None,
    })
}

fn text_row(role: Role, content: String) -> Row {
    Row {
        role,
        content,
        tool_name: None,
        tool_args: None,
        tool_result: None,
    }
}

fn tool_name(payload: &Payload) -> String {
    payload
        .name
        .clone()
        .filter(|name| !name.is_empty())
        .or_else(|| payload.tool.clone().filter(|name| !name.is_empty()))
        .unwrap_or_else(|| "unknown".to_owned())
}

fn message_text(
    raw: Option<&RawValue>,
    wanted: &str,
) -> Result<Option<String>, TranscriptReaderError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let text = match lenient::<Content>(raw)? {
        Some(Content::Text(text)) => text,
        Some(Content::Blocks(blocks)) => blocks
            .into_iter()
            .filter(|block| matches!(block.kind.as_deref(), Some(kind) if kind == wanted || kind == "text"))
            .filter_map(|block| block.text.filter(|text| !text.is_empty()))
            .collect::<Vec<_>>()
            .join("\n"),
        None => return Ok(None),
    };
    let text = text.trim();
    Ok((!text.is_empty() && !injected(text)).then(|| text.to_owned()))
}

fn reasoning_text(payload: &Payload) -> Result<Option<String>, TranscriptReaderError> {
    if let Some(text) = payload.text.as_ref().filter(|text| !text.is_empty()) {
        return Ok(Some(text.clone()));
    }
    let mut joined = String::new();
    for raw in [payload.summary.as_deref(), payload.content.as_deref()]
        .into_iter()
        .flatten()
    {
        for block in lenient::<Vec<Block>>(raw)?.into_iter().flatten() {
            if let Some(text) = block.text.filter(|text| !text.is_empty()) {
                joined.push_str(&text);
            }
        }
    }
    Ok((!joined.is_empty()).then_some(joined))
}

fn failure(raw: &RawValue) -> bool {
    !matches!(raw.get().trim(), "null" | "false" | "\"\"")
}

fn result_text(raw: &RawValue) -> Result<String, TranscriptReaderError> {
    Ok(lenient::<String>(raw)?.unwrap_or_else(|| raw.get().to_owned()))
}

fn pointer(
    file: &str,
    number: u64,
    tool_use_id: Option<&str>,
) -> Result<JsonObject, TranscriptReaderError> {
    JsonObject::from_serialize(&Pointer {
        source: SOURCE,
        session_jsonl_path: file,
        session_jsonl_line: number,
        tool_use_id,
    })
    .map_err(encoding)
}

fn encoding(error: serde_json::Error) -> TranscriptReaderError {
    TranscriptReaderError::Read {
        message: format!("codex capture JSON failed: {error}"),
        source: Box::new(error),
    }
}
