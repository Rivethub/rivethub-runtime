use std::collections::HashMap;
use std::path::Path;

use chrono::DateTime;
use chrono::Utc;
use kernel::memory::spoken_title;
use wire::JsonObject;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::message::Role;

use super::FALLBACK_TITLE;
use super::HookError;
use super::base_batch;
use super::pending::Pending;
use crate::event_id::OccurrenceKind;
use crate::event_id::occurrence_digest;
use crate::event_id::occurrence_event_id;
use crate::records::HookEventMetadata;
use crate::records::SOURCE;
use crate::records::TranscriptMetadata;
use crate::transcript::Block;
use crate::transcript::Line;
use crate::transcript::LineContent;
use crate::transcript::LineKind;
use crate::transcript::ToolUse;

const TOOL_CALL: &str = "[tool call]";

struct Folded<'a> {
    line: &'a Line,
    role: Role,
    content: String,
    has_result: bool,
    tools: Vec<ToolUse<'a>>,
}

impl Folded<'_> {
    fn spoken(&self) -> bool {
        !self.content.is_empty() && !self.content.starts_with(TOOL_CALL)
    }

    fn assistant(&self) -> bool {
        self.role == Role::Assistant && self.spoken()
    }

    fn prompt(&self) -> bool {
        self.role == Role::User && !self.has_result && self.spoken()
    }

    fn file_line(&self) -> usize {
        self.line.number.saturating_sub(1)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowKind {
    Assistant,
    User,
    Tool,
}

struct Results {
    text: HashMap<String, String>,
    first_line: HashMap<String, usize>,
}

struct Source<'a> {
    session_key: &'a str,
    path: Option<String>,
    rows: Vec<String>,
}

pub(super) fn build(
    path: Option<&Path>,
    session_key: &str,
    transcript: &[Line],
    finalize: bool,
    pending: &[Pending],
) -> Result<CaptureBatchRequest, HookError> {
    let mut source = Source {
        session_key,
        path: path.map(|path| path.display().to_string()),
        rows: Vec::new(),
    };
    let results = results(transcript);
    let folded: Vec<Folded<'_>> = transcript.iter().filter_map(fold).collect();
    let mut batch = base_batch(session_key);
    batch.finalize = finalize.then_some(true);
    for message in &folded {
        if message.assistant() {
            let event_id = source.id(message, OccurrenceKind::Assistant);
            batch
                .messages
                .push(source.text_row(message, event_id, Role::Assistant)?);
        }
        for tool in &message.tools {
            batch
                .messages
                .push(source.tool_row(message, tool, &results)?);
        }
        if message.prompt() {
            let event_id = source.id(message, OccurrenceKind::User);
            batch
                .messages
                .push(source.text_row(message, event_id, Role::User)?);
        }
    }
    for prompt in pending {
        if !folded
            .iter()
            .any(|message| message.prompt() && message.content == prompt.prompt)
        {
            batch.messages.push(CaptureMessage {
                event_id: format!("{session_key}:hook:{}", prompt.key),
                role: Role::User,
                content: prompt.prompt.clone(),
                tool_name: None,
                tool_args: None,
                tool_result: None,
                metadata: Some(JsonObject::from_serialize(&HookEventMetadata {
                    source: SOURCE,
                    hook_event: "UserPromptSubmit",
                    ingest_key: &prompt.key,
                })?),
                created_at: Some(prompt.at),
            });
        }
    }
    batch.title = Some(
        folded
            .iter()
            .filter(|message| message.prompt())
            .find_map(|message| spoken_title(&message.content).map(str::to_owned))
            .unwrap_or_else(|| FALLBACK_TITLE.to_owned()),
    );
    Ok(batch)
}

pub(super) fn holds_reply(transcript: &[Line], reply: &str) -> bool {
    let folded: Vec<Folded<'_>> = transcript.iter().filter_map(fold).collect();
    folded
        .iter()
        .rev()
        .take_while(|message| !message.prompt())
        .find(|message| message.assistant())
        .is_some_and(|last| reply.ends_with(last.content.as_str()))
}

fn fold(line: &Line) -> Option<Folded<'_>> {
    if line.is_meta == Some(true) || !matches!(line.r#type, LineKind::User | LineKind::Assistant) {
        return None;
    }
    let message = line.message.as_ref()?;
    let tools = line.tool_uses();
    let (mut content, results) = match &message.content {
        LineContent::Text(text) => (text.clone(), Vec::new()),
        LineContent::Blocks(blocks) => block_content(blocks),
    };
    let result = (!results.is_empty()).then(|| results.join("\n"));
    if content.is_empty() && !tools.is_empty() {
        let names: Vec<&str> = tools.iter().map(|tool| tool.name).collect();
        content = format!("{TOOL_CALL} {}", names.join(", "));
    }
    if content.is_empty()
        && let Some(result) = &result
    {
        content.clone_from(result);
    }
    if content.is_empty() {
        return None;
    }
    Some(Folded {
        line,
        role: message.role,
        content,
        has_result: result.is_some(),
        tools,
    })
}

fn block_content(blocks: &[Block]) -> (String, Vec<String>) {
    let mut texts = Vec::new();
    let mut thoughts = Vec::new();
    let mut results = Vec::new();
    for block in blocks {
        match block {
            Block::Text { text } => texts.push(text.as_str()),
            Block::Thinking { thinking, text } => match thinking.as_deref().or(text.as_deref()) {
                Some(thought) => thoughts.push(thought),
                None => thoughts.push(""),
            },
            Block::ToolResult { content, .. } => results.push(result_text(content)),
            Block::ToolUse { .. } | Block::Other => {}
        }
    }
    let mut content = texts.join("\n").trim().to_owned();
    if content.is_empty() && !thoughts.is_empty() {
        content = format!("[thinking] {}", thoughts.join("\n").trim());
    }
    (content, results)
}

fn result_text(content: &LineContent) -> String {
    match content {
        LineContent::Text(text) => text.clone(),
        LineContent::Blocks(blocks) => blocks
            .iter()
            .map(|block| match block {
                Block::Text { text } => text.as_str(),
                Block::ToolUse { .. }
                | Block::ToolResult { .. }
                | Block::Thinking { .. }
                | Block::Other => "",
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn results(transcript: &[Line]) -> Results {
    let mut found = Results {
        text: HashMap::new(),
        first_line: HashMap::new(),
    };
    for line in transcript {
        let Some(message) = &line.message else {
            continue;
        };
        let LineContent::Blocks(blocks) = &message.content else {
            continue;
        };
        for block in blocks {
            if let Block::ToolResult {
                tool_use_id: Some(id),
                content,
            } = block
            {
                found.text.insert(id.clone(), result_text(content));
                found
                    .first_line
                    .entry(id.clone())
                    .or_insert_with(|| line.number.saturating_sub(1));
            }
        }
    }
    found
}

impl Source<'_> {
    fn occurrence(&mut self, digest: String) -> usize {
        let earlier = self.rows.iter().filter(|row| **row == digest).count();
        self.rows.push(digest);
        earlier
    }

    fn id(&mut self, message: &Folded<'_>, kind: OccurrenceKind) -> String {
        let digest = occurrence_digest(kind, &message.content, "", None);
        let occurrence = self.occurrence(digest.clone());
        match &message.line.uuid {
            Some(uuid) => format!("{}:{uuid}", self.session_key),
            None => occurrence_event_id(self.session_key, &digest, occurrence),
        }
    }

    fn metadata(
        &self,
        line: usize,
        message: &Folded<'_>,
        kind: RowKind,
    ) -> Result<JsonObject, HookError> {
        Ok(JsonObject::from_serialize(&TranscriptMetadata {
            source: SOURCE,
            session_jsonl_path: self.path.clone(),
            session_jsonl_line: Some(line),
            uuid: message.line.uuid.as_deref(),
            sidechain: (kind == RowKind::Assistant)
                .then(|| message.line.is_sidechain.unwrap_or(false)),
            hook_event: (kind == RowKind::Tool).then_some("PostToolUse"),
            recovered: (kind != RowKind::Assistant).then_some(true),
        })?)
    }

    fn text_row(
        &self,
        message: &Folded<'_>,
        event_id: String,
        role: Role,
    ) -> Result<CaptureMessage, HookError> {
        let kind = if role == Role::Assistant {
            RowKind::Assistant
        } else {
            RowKind::User
        };
        Ok(CaptureMessage {
            event_id,
            role,
            content: message.content.clone(),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            metadata: Some(self.metadata(message.file_line(), message, kind)?),
            created_at: instant(message.line.timestamp.as_deref())?,
        })
    }

    fn tool_row(
        &mut self,
        message: &Folded<'_>,
        tool: &ToolUse<'_>,
        results: &Results,
    ) -> Result<CaptureMessage, HookError> {
        let content = format!("{TOOL_CALL} {}", tool.name);
        let digest = occurrence_digest(OccurrenceKind::Tool, &content, tool.name, Some(tool.input));
        self.occurrence(digest);
        let line = results
            .first_line
            .get(tool.id)
            .copied()
            .unwrap_or_else(|| message.file_line());
        Ok(CaptureMessage {
            event_id: format!("{}:tool:{}", self.session_key, tool.id),
            role: Role::Tool,
            content,
            tool_name: Some(tool.name.to_owned()),
            tool_args: Some(tool.input.arguments().clone()),
            tool_result: results.text.get(tool.id).cloned(),
            metadata: Some(self.metadata(line, message, RowKind::Tool)?),
            created_at: instant(message.line.timestamp.as_deref())?,
        })
    }
}

fn instant(raw: Option<&str>) -> Result<Option<DateTime<Utc>>, HookError> {
    raw.map(|text| DateTime::parse_from_rfc3339(text).map(|parsed| parsed.with_timezone(&Utc)))
        .transpose()
        .map_err(HookError::Timestamp)
}
