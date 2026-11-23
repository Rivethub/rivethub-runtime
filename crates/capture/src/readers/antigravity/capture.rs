use jiff::Timestamp;
use std::collections::VecDeque;
use std::path::Path;

use kernel::transcript::TranscriptReaderError;
use serde::Deserialize;
use serde::Serialize;
use serde_json::value::RawValue;
use serde_with::DefaultOnError;
use serde_with::serde_as;
use uuid::Uuid;
use wire::JsonObject;
use wire::capture::CaptureMessage;
use wire::message::Role;

use super::super::message::SkippedRecord;
use super::super::message::TranscriptSlice;
use super::super::message::line_number;
use super::super::message::parse_json;
use super::super::message::tool_input;

const SOURCE: &str = "antigravity-transcript";

#[serde_as]
#[derive(Debug, Deserialize)]
struct Step {
    #[serde_as(as = "DefaultOnError")]
    #[serde(rename = "step_index", default)]
    index: Option<u64>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(rename = "type", default)]
    kind: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    status: Option<String>,
    #[serde_as(as = "DefaultOnError<Option<wire::time::Rfc3339>>")]
    #[serde(default)]
    created_at: Option<Timestamp>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    content: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    thinking: Option<String>,
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
}

#[serde_as]
#[derive(Debug, Deserialize)]
struct ToolCall {
    #[serde_as(as = "DefaultOnError")]
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    args: Option<Box<RawValue>>,
}

#[derive(Debug, Serialize)]
struct Pointer<'a> {
    source: &'static str,
    session_jsonl_path: &'a str,
    session_jsonl_line: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    step_index: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_use_id: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SpawnedSubagent {
    pub(crate) conversation_id: String,
    pub(crate) agent_type: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) tool_use_id: String,
}

#[derive(Debug)]
struct PendingTool {
    name: String,
    call_event_id: String,
    spawn_type: Option<String>,
    spawn_role: Option<String>,
}

#[derive(Debug, Deserialize)]
struct InvokeSubagentArgs {
    #[serde(rename = "TypeName", default)]
    type_name: Option<String>,
    #[serde(rename = "Role", default)]
    description: Option<String>,
}

struct TranscriptMapper<'a> {
    native_id: &'a str,
    file: String,
    offset: u64,
    pending: VecDeque<PendingTool>,
    messages: Vec<CaptureMessage>,
    skipped: Vec<SkippedRecord>,
    last: u64,
}

impl<'a> TranscriptMapper<'a> {
    fn new(native_id: &'a str, path: &Path, offset: u64) -> Self {
        Self {
            native_id,
            file: path.display().to_string(),
            offset,
            pending: VecDeque::new(),
            messages: Vec::new(),
            skipped: Vec::new(),
            last: offset,
        }
    }

    fn process_line(&mut self, number: u64, raw: &str) -> Result<(), TranscriptReaderError> {
        self.last = self.last.max(number);
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let step = match parse_json::<Step>(number, trimmed) {
            Ok(step) => step,
            Err(record) => {
                if number > self.offset {
                    self.skipped.push(record);
                }
                return Ok(());
            }
        };
        let step_num = step.index.unwrap_or(number);
        match step.kind.as_deref() {
            Some("USER_INPUT") => {
                if let Some(clean) = step.content.as_deref().and_then(clean_user_input) {
                    let event_id = format!("antigravity:{}:step:{step_num}", self.native_id);
                    self.push_text(event_id, Role::User, clean, number, &step)?;
                }
            }
            Some("SYSTEM_MESSAGE") => {
                if let Some((_, body)) = step.content.as_deref().and_then(extract_agent_message) {
                    let event_id = format!("antigravity:{}:step:{step_num}", self.native_id);
                    self.push_text(event_id, Role::User, body, number, &step)?;
                }
            }
            Some("PLANNER_RESPONSE") => self.push_planner(number, step_num, &step)?,
            Some("GENERIC") => self.push_generic(number, step_num, &step)?,
            _ => {}
        }
        Ok(())
    }

    fn push_text(
        &mut self,
        event_id: String,
        role: Role,
        content: String,
        number: u64,
        step: &Step,
    ) -> Result<(), TranscriptReaderError> {
        if number <= self.offset {
            return Ok(());
        }
        self.messages.push(CaptureMessage {
            event_id,
            role,
            content,
            tool_name: None,
            tool_args: None,
            tool_result: None,
            metadata: Some(pointer(&self.file, number, step.index, None)?),
            created_at: step.created_at,
        });
        Ok(())
    }

    fn push_planner(
        &mut self,
        number: u64,
        step_num: u64,
        step: &Step,
    ) -> Result<(), TranscriptReaderError> {
        if let Some(thinking) = nonempty_trimmed(step.thinking.as_deref()) {
            let event_id = format!("antigravity:{}:step:{step_num}:thinking", self.native_id);
            self.push_text(
                event_id,
                Role::Assistant,
                format!("[thinking] {thinking}"),
                number,
                step,
            )?;
        }
        if let Some(reply) = nonempty_trimmed(step.content.as_deref()) {
            let event_id = format!("antigravity:{}:step:{step_num}:text", self.native_id);
            self.push_text(event_id, Role::Assistant, reply, number, step)?;
        }
        for (tc_index, call) in step.tool_calls.iter().enumerate() {
            let name =
                nonempty_trimmed(call.name.as_deref()).unwrap_or_else(|| "unknown".to_owned());
            let call_event_id = format!(
                "antigravity:{}:step:{step_num}:tool:{tc_index}",
                self.native_id
            );
            let tool_args = call.args.as_deref().map(tool_input).transpose()?;
            self.pending.push_back(PendingTool {
                name: name.clone(),
                call_event_id: call_event_id.clone(),
                spawn_type: None,
                spawn_role: None,
            });
            if number > self.offset {
                self.messages.push(CaptureMessage {
                    event_id: call_event_id.clone(),
                    role: Role::Tool,
                    content: format!("[tool] {name}"),
                    tool_name: Some(name),
                    tool_args,
                    tool_result: None,
                    metadata: Some(pointer(
                        &self.file,
                        number,
                        step.index,
                        Some(&call_event_id),
                    )?),
                    created_at: step.created_at,
                });
            }
        }
        Ok(())
    }

    fn push_generic(
        &mut self,
        number: u64,
        step_num: u64,
        step: &Step,
    ) -> Result<(), TranscriptReaderError> {
        let matched = self.pending.pop_front();
        if number <= self.offset {
            return Ok(());
        }
        let name = matched
            .as_ref()
            .map_or_else(|| "unknown".to_owned(), |item| item.name.clone());
        let call_id = matched.as_ref().map(|item| item.call_event_id.as_str());
        let failed = step.status.as_deref() == Some("ERROR")
            || nonempty_trimmed(step.error.as_deref()).is_some();
        let outcome = if failed {
            "tool-failure"
        } else {
            "tool-result"
        };
        let result_body = nonempty_trimmed(step.content.as_deref())
            .or_else(|| nonempty_trimmed(step.error.as_deref()));
        self.messages.push(CaptureMessage {
            event_id: format!("antigravity:{}:step:{step_num}", self.native_id),
            role: Role::Tool,
            content: format!("[{outcome}] {name}"),
            tool_name: Some(name),
            tool_args: None,
            tool_result: result_body,
            metadata: Some(pointer(&self.file, number, step.index, call_id)?),
            created_at: step.created_at,
        });
        Ok(())
    }
}

pub(super) fn map_transcript(
    native_id: &str,
    path: &Path,
    text: &str,
    offset: u64,
) -> Result<TranscriptSlice, TranscriptReaderError> {
    let mut mapper = TranscriptMapper::new(native_id, path, offset);
    for (index, raw) in text.split('\n').enumerate() {
        let number = line_number(index + 1)?;
        mapper.process_line(number, raw)?;
    }
    Ok(TranscriptSlice {
        messages: mapper.messages,
        next_offset: mapper.last,
        skipped: mapper.skipped,
    })
}

pub(crate) fn parent_from_transcript(text: &str) -> Option<String> {
    for raw in text.split('\n').take(8) {
        if raw.trim().is_empty() {
            continue;
        }
        #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
        let Ok(step) = serde_json::from_str::<Step>(raw.trim()) else {
            continue;
        };
        if step.kind.as_deref() != Some("SYSTEM_MESSAGE") {
            continue;
        }
        if let Some((sender, _)) = step.content.as_deref().and_then(extract_agent_message)
            && Uuid::parse_str(&sender).is_ok()
        {
            return Some(sender);
        }
    }
    None
}

pub(crate) fn spawned_subagents(native_id: &str, text: &str) -> Vec<SpawnedSubagent> {
    let mut pending: VecDeque<PendingTool> = VecDeque::new();
    let mut found = Vec::new();
    for (index, raw) in text.split('\n').enumerate() {
        if raw.trim().is_empty() {
            continue;
        }
        #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
        let Ok(step) = serde_json::from_str::<Step>(raw.trim()) else {
            continue;
        };
        let step_num = step.index.unwrap_or(index as u64);
        match step.kind.as_deref() {
            Some("PLANNER_RESPONSE") => {
                for (tc_index, call) in step.tool_calls.iter().enumerate() {
                    let name = nonempty_trimmed(call.name.as_deref())
                        .unwrap_or_else(|| "unknown".to_owned());
                    let call_event_id =
                        format!("antigravity:{native_id}:step:{step_num}:tool:{tc_index}");
                    let (spawn_type, spawn_role) = if name == "invoke_subagent" {
                        call.args
                            .as_deref()
                            .and_then(|raw| {
                                #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
                                let parsed =
                                    serde_json::from_str::<InvokeSubagentArgs>(raw.get()).ok();
                                parsed
                            })
                            .map_or((None, None), |args| {
                                (
                                    nonempty_trimmed(args.type_name.as_deref()),
                                    nonempty_trimmed(args.description.as_deref()),
                                )
                            })
                    } else {
                        (None, None)
                    };
                    pending.push_back(PendingTool {
                        name,
                        call_event_id,
                        spawn_type,
                        spawn_role,
                    });
                }
            }
            Some("GENERIC") => {
                let Some(tool) = pending.pop_front() else {
                    continue;
                };
                if tool.name != "invoke_subagent" {
                    continue;
                }
                let Some(child_id) = step
                    .content
                    .as_deref()
                    .and_then(extract_conversation_id_field)
                else {
                    continue;
                };
                if child_id == native_id {
                    continue;
                }
                found.push(SpawnedSubagent {
                    conversation_id: child_id,
                    agent_type: tool.spawn_type,
                    description: tool.spawn_role,
                    tool_use_id: tool.call_event_id,
                });
            }
            _ => {}
        }
    }
    found
}

pub(crate) fn clean_user_input(raw: &str) -> Option<String> {
    let mut text = raw.to_owned();
    if let Some(inner) = between_tags(&text, "<USER_REQUEST>", "</USER_REQUEST>") {
        text = inner.to_owned();
    } else {
        text = strip_tag_block(&text, "<ADDITIONAL_METADATA>", "</ADDITIONAL_METADATA>");
        text = strip_tag_block(&text, "<USER_SETTINGS_CHANGE>", "</USER_SETTINGS_CHANGE>");
    }
    if let Some(inner) = between_tags(&text, "<user_request>", "</user_request>") {
        text = inner.to_owned();
    } else {
        text = strip_tag_block(&text, "<t3_code_instructions>", "</t3_code_instructions>");
        text = strip_tag_block(&text, "<runtime_info>", "</runtime_info>");
        text = strip_tag_block(&text, "<pull_request_linking>", "</pull_request_linking>");
    }
    nonempty_trimmed(Some(&text))
}

fn extract_agent_message(raw: &str) -> Option<(String, String)> {
    let inner = last_between_tags(raw, "<SYSTEM_MESSAGE>", "</SYSTEM_MESSAGE>").unwrap_or(raw);
    let line = inner.trim();
    let rest = line.strip_prefix("[Message]")?;
    let sender_start = rest.find("sender=")?;
    let after_sender = rest.get(sender_start + "sender=".len()..)?;
    let sender_end = after_sender
        .find(char::is_whitespace)
        .unwrap_or(after_sender.len());
    let sender = after_sender.get(..sender_end)?.trim();
    if sender.is_empty() || sender == "system" {
        return None;
    }
    let content_key = "content=";
    let content_start = after_sender.find(content_key)?;
    let body = after_sender
        .get(content_start + content_key.len()..)?
        .trim();
    if body.is_empty() {
        return None;
    }
    Some((sender.to_owned(), body.to_owned()))
}

fn last_between_tags<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.rfind(open)? + open.len();
    let rest = text.get(start..)?;
    let end = rest.find(close)?;
    rest.get(..end)
}

fn extract_conversation_id_field(text: &str) -> Option<String> {
    let marker = "\"conversationId\"";
    let start = text.find(marker)?;
    let rest = text.get(start + marker.len()..)?;
    let colon = rest.find(':')?;
    let after_colon = rest.get(colon + 1..)?.trim_start();
    let quoted = after_colon.strip_prefix('"')?;
    let end = quoted.find('"')?;
    let id = quoted.get(..end)?;
    Uuid::parse_str(id).is_ok().then(|| id.to_owned())
}

fn between_tags<'a>(text: &'a str, open: &str, close: &str) -> Option<&'a str> {
    let start = text.find(open)? + open.len();
    let rest = text.get(start..)?;
    let end = rest.rfind(close)?;
    rest.get(..end)
}

fn strip_tag_block(text: &str, open: &str, close: &str) -> String {
    let mut out = text.to_owned();
    while let Some(start) = out.find(open) {
        let after = start + open.len();
        if let Some(rel_end) = out.get(after..).and_then(|tail| tail.find(close)) {
            let end = after + rel_end + close.len();
            out.replace_range(start..end, "");
        } else {
            break;
        }
    }
    out
}

fn nonempty_trimmed(text: Option<&str>) -> Option<String> {
    let trimmed = text?.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn pointer(
    file: &str,
    number: u64,
    step_index: Option<u64>,
    tool_use_id: Option<&str>,
) -> Result<JsonObject, TranscriptReaderError> {
    JsonObject::from_serialize(&Pointer {
        source: SOURCE,
        session_jsonl_path: file,
        session_jsonl_line: number,
        step_index,
        tool_use_id,
    })
    .map_err(|error| TranscriptReaderError::Read {
        message: format!("antigravity capture JSON failed: {error}"),
        source: Box::new(error),
    })
}
