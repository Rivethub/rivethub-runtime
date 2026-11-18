use jiff::Timestamp;
use std::collections::HashMap;

use kernel::transcript::TranscriptReaderError;
use serde::Serialize;
use wire::JsonObject;
use wire::capture::CaptureMessage;
use wire::message::Role;
use wire::session_id::SessionId;

use super::super::message::Draft;
use super::super::message::SkipRule;
use super::super::message::SkippedRecord;
use super::super::message::TranscriptSlice;
use super::super::message::assemble;
use super::super::message::line_number;
use super::super::message::unknown;
use crate::event_id::OccurrenceKind;
use crate::event_id::occurrence_digest;
use crate::event_id::occurrence_event_id;
use crate::transcript::Line;
use crate::transcript::LineKind;
use crate::transcript::Parsed;

const SOURCE: &str = "cowork-transcript";
const UNNAMED_TOOL: &str = "tool";

#[derive(Serialize)]
struct Meta<'a> {
    source: &'static str,
    position: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_use_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_result: Option<bool>,
}

struct Call<'a> {
    name: &'a str,
    position: u64,
}

struct Stitch<'a> {
    session: &'a SessionId,
    offset: u64,
    calls: HashMap<&'a str, Call<'a>>,
    results: HashMap<String, String>,
    seen: HashMap<String, usize>,
}

struct Row<'a> {
    position: u64,
    at: Option<Timestamp>,
    tool_use_id: Option<&'a str>,
    pending_result: Option<bool>,
}

pub(super) fn map(
    session: &SessionId,
    parsed: &Parsed,
    offset: u64,
) -> Result<TranscriptSlice, TranscriptReaderError> {
    let mut stitch = Stitch::new(session, offset, &parsed.lines)?;
    let mut drafts = Vec::new();
    let mut skipped = Vec::new();
    let mut last = offset;
    for line in &parsed.skipped {
        let position = line_number(line.number)?;
        last = last.max(position);
        skipped.push(SkippedRecord {
            position,
            rule: SkipRule::UnparseableRecord,
            detail: line.reason.clone(),
        });
    }
    for line in &parsed.lines {
        let position = line_number(line.number)?;
        last = last.max(position);
        match stitch.line(line, position)? {
            Ok(mut found) => drafts.append(&mut found),
            Err(record) => skipped.push(record),
        }
    }
    Ok(assemble(offset, last, drafts, skipped))
}

impl<'a> Stitch<'a> {
    fn new(
        session: &'a SessionId,
        offset: u64,
        lines: &'a [Line],
    ) -> Result<Self, TranscriptReaderError> {
        let mut calls = HashMap::new();
        let mut results = HashMap::new();
        for line in lines {
            let position = line_number(line.number)?;
            for tool in line.tool_uses() {
                calls.insert(
                    tool.id,
                    Call {
                        name: tool.name,
                        position,
                    },
                );
            }
            for outcome in line.tool_results() {
                if let Some(id) = outcome.tool_use_id {
                    results.insert(id, outcome.text);
                }
            }
        }
        Ok(Self {
            session,
            offset,
            calls,
            results,
            seen: HashMap::new(),
        })
    }

    fn line(
        &mut self,
        line: &Line,
        position: u64,
    ) -> Result<Result<Vec<Draft>, SkippedRecord>, TranscriptReaderError> {
        if line.is_meta == Some(true) {
            return Ok(Ok(Vec::new()));
        }
        if line.r#type == LineKind::Other {
            return Ok(Err(unknown(position, "unknown record type")));
        }
        let at = match instant(line.timestamp.as_deref(), position) {
            Ok(at) => at,
            Err(record) => return Ok(Err(record)),
        };
        let row = Row {
            position,
            at,
            tool_use_id: None,
            pending_result: None,
        };
        let mut drafts = Vec::new();
        if let Some(draft) = self.text(line, &row)? {
            drafts.push(draft);
        }
        self.calls_of(line, &row, &mut drafts)?;
        self.results_of(line, &row, &mut drafts)?;
        Ok(Ok(drafts))
    }

    fn text(&mut self, line: &Line, row: &Row<'_>) -> Result<Option<Draft>, TranscriptReaderError> {
        let uses = line.tool_uses();
        let Some(content) = line.text().filter(|text| !text.is_empty()) else {
            return Ok(None);
        };
        if uses.is_empty() && !line.tool_results().is_empty() {
            return Ok(None);
        }
        let assistant = line.r#type == LineKind::Assistant
            || line
                .message
                .as_ref()
                .is_some_and(|message| message.role == Role::Assistant)
            || !uses.is_empty();
        let (role, kind) = if assistant {
            (Role::Assistant, OccurrenceKind::Assistant)
        } else {
            (Role::User, OccurrenceKind::User)
        };
        let event_id = match line.uuid.as_deref() {
            Some(uuid) => format!("{}:{uuid}", self.session),
            None => self.occurrence(kind, &content),
        };
        let message = CaptureMessage {
            event_id,
            role,
            content,
            tool_name: None,
            tool_args: None,
            tool_result: None,
            metadata: Some(metadata(row)?),
            created_at: row.at,
        };
        Ok(Some(draft(row, message)))
    }

    fn calls_of(
        &self,
        line: &Line,
        row: &Row<'_>,
        drafts: &mut Vec<Draft>,
    ) -> Result<(), TranscriptReaderError> {
        for tool in line.tool_uses() {
            let result = self.results.get(tool.id).cloned();
            let tool_row = Row {
                tool_use_id: Some(tool.id),
                pending_result: result.is_none().then_some(true),
                ..*row
            };
            let message = CaptureMessage {
                event_id: format!("{}:tool:{}", self.session, tool.id),
                role: Role::Tool,
                content: format!("[tool call] {}", tool.name),
                tool_name: Some(tool.name.to_owned()),
                tool_args: Some(tool.input.arguments().clone()),
                tool_result: result,
                metadata: Some(metadata(&tool_row)?),
                created_at: row.at,
            };
            drafts.push(draft(row, message));
        }
        Ok(())
    }

    fn results_of(
        &self,
        line: &Line,
        row: &Row<'_>,
        drafts: &mut Vec<Draft>,
    ) -> Result<(), TranscriptReaderError> {
        let uses = line.tool_uses();
        for outcome in line.tool_results() {
            let Some(id) = outcome.tool_use_id.as_deref() else {
                continue;
            };
            if uses.iter().any(|tool| tool.id == id) {
                continue;
            }
            let call = self.calls.get(id);
            if call.is_some_and(|call| call.position > self.offset) {
                continue;
            }
            let name = call.map_or(UNNAMED_TOOL, |call| call.name);
            let result_row = Row {
                tool_use_id: Some(id),
                pending_result: Some(false),
                ..*row
            };
            let message = CaptureMessage {
                event_id: format!("{}:tool:{id}", self.session),
                role: Role::Tool,
                content: format!("[tool call] {name}"),
                tool_name: Some(name.to_owned()),
                tool_args: None,
                tool_result: Some(outcome.text),
                metadata: Some(metadata(&result_row)?),
                created_at: row.at,
            };
            drafts.push(draft(row, message));
        }
        Ok(())
    }

    fn occurrence(&mut self, kind: OccurrenceKind, content: &str) -> String {
        let digest = occurrence_digest(kind, content, "", None);
        let count = self.seen.entry(digest.clone()).or_insert(0);
        let occurrence = *count;
        *count += 1;
        occurrence_event_id(self.session.as_str(), &digest, occurrence)
    }
}

fn draft(row: &Row<'_>, message: CaptureMessage) -> Draft {
    Draft {
        position: row.position,
        message,
    }
}

fn instant(raw: Option<&str>, position: u64) -> Result<Option<Timestamp>, SkippedRecord> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    wire::time::parse_rfc3339(raw)
        .map(Some)
        .map_err(|error| SkippedRecord {
            position,
            rule: SkipRule::UnparseableTimestamp,
            detail: format!("timestamp {raw:?} is not RFC 3339: {error}"),
        })
}

fn metadata(row: &Row<'_>) -> Result<JsonObject, TranscriptReaderError> {
    JsonObject::from_serialize(&Meta {
        source: SOURCE,
        position: row.position,
        tool_use_id: row.tool_use_id,
        pending_result: row.pending_result,
    })
    .map_err(|error| TranscriptReaderError::Read {
        message: format!("cowork transcript metadata encoding failed: {error}"),
        source: Box::new(error),
    })
}
