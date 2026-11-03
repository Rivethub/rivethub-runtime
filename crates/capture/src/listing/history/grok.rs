use std::path::Path;

use kernel::transcript::HarnessTranscriptTool;
use kernel::transcript::HarnessTranscriptTurn;
use kernel::transcript::SessionMessageRole;
use kernel::transcript::StoreTranscript;
use kernel::transcript::TranscriptBlock;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::TranscriptToolStatus;
use wire::JsonObject;

use super::display::tail_chars;
use super::tools::RESULT_CHARS;
use super::tools::args_summary;
use super::tools::is_prompt_tool;
use super::tools::keeps_input;
use super::tools::kept_object;
use crate::readers::SkippedRecord;
use crate::readers::grok::FORMAT;
use crate::readers::grok::tool_result;
use crate::readers::grok::update_lines;
use crate::readers::grok::updates::SessionUpdate;
use crate::readers::grok::updates::ToolStatus;
use crate::readers::grok::updates::ToolUpdate;
use crate::readers::report;

const END_TURN: &str = "end_turn";

pub(crate) fn transcript(root: &Path, id: &str) -> Result<StoreTranscript, TranscriptReaderError> {
    let parsed = update_lines(root, id)?;
    let mut folder = Folder {
        skipped: parsed.skipped,
        ..Folder::default()
    };
    for (position, line) in parsed.lines {
        folder.line(
            position,
            line.params.meta.agent_timestamp_ms,
            line.params.update,
        )?;
    }
    folder.flush();
    report(FORMAT, &folder.skipped);
    Ok(StoreTranscript {
        turns: folder.turns,
        earlier: None,
        context: None,
    })
}

#[derive(Debug, Default)]
struct Folder {
    turns: Vec<HarnessTranscriptTurn>,
    pending: Option<Draft>,
    skipped: Vec<SkippedRecord>,
}

#[derive(Debug, Default)]
struct Draft {
    text: String,
    thinking: String,
    tools: Vec<HarnessTranscriptTool>,
    stop_reason: Option<String>,
    last_block: Option<TranscriptBlock>,
    started_at_ms: Option<i64>,
    ended_at_ms: Option<i64>,
}

impl Folder {
    fn line(
        &mut self,
        position: u64,
        at: Option<i64>,
        update: SessionUpdate,
    ) -> Result<(), TranscriptReaderError> {
        match update {
            SessionUpdate::UserMessageChunk { content } => {
                if let Some(text) = content.text() {
                    self.user(text, at);
                }
            }
            SessionUpdate::AgentMessageChunk { content } => {
                if let Some(text) = content.text() {
                    self.draft(at).text(text);
                }
            }
            SessionUpdate::AgentThoughtChunk { content } => {
                if let Some(text) = content.text() {
                    self.draft(at).thought(text);
                }
            }
            SessionUpdate::ToolCall(call) => {
                self.draft(at)
                    .open(call.tool_call_id, call.title, call.raw_input.as_ref())?;
            }
            SessionUpdate::ToolCallUpdate(update) => {
                if let Some(skipped) = self.draft(at).update(position, update)? {
                    self.skipped.push(skipped);
                }
            }
            SessionUpdate::TurnCompleted { stop_reason, .. } => {
                if let Some(draft) = self.pending.as_mut() {
                    draft.stamp(at);
                    draft.stop_reason = stop_reason;
                }
                self.flush();
            }
            SessionUpdate::MemoryFlushStarted
            | SessionUpdate::MemoryFlushCompleted
            | SessionUpdate::Unknown
            | SessionUpdate::HookExecution
            | SessionUpdate::AvailableCommandsUpdate
            | SessionUpdate::Plan
            | SessionUpdate::BackgroundTasks
            | SessionUpdate::TaskBackgrounded
            | SessionUpdate::TaskCompleted
            | SessionUpdate::RetryState
            | SessionUpdate::AutoCompactStarted
            | SessionUpdate::CompactionCheckpoint
            | SessionUpdate::AutoCompactCompleted
            | SessionUpdate::SessionRecap
            | SessionUpdate::SubagentSpawned
            | SessionUpdate::SubagentFinished
            | SessionUpdate::GoalUpdated
            | SessionUpdate::MemoryDreamQueued
            | SessionUpdate::MemoryDreamStarted
            | SessionUpdate::MemoryDreamCompleted => {}
        }
        Ok(())
    }

    fn user(&mut self, text: &str, at: Option<i64>) {
        self.flush();
        self.turns.push(HarnessTranscriptTurn {
            role: SessionMessageRole::User,
            text: text.to_owned(),
            thinking: None,
            tools: None,
            usage: None,
            model: None,
            stop_reason: None,
            last_block: None,
            attachments: None,
            complete: false,
            compact: false,
            started_at_ms: at,
            ended_at_ms: at,
        });
    }

    fn draft(&mut self, at: Option<i64>) -> &mut Draft {
        let draft = self.pending.get_or_insert_with(Draft::default);
        draft.stamp(at);
        draft
    }

    fn flush(&mut self) {
        if let Some(turn) = self.pending.take().and_then(Draft::close) {
            self.turns.push(turn);
        }
    }
}

impl Draft {
    fn stamp(&mut self, at: Option<i64>) {
        if let Some(at) = at {
            self.started_at_ms.get_or_insert(at);
            self.ended_at_ms = Some(at);
        }
    }

    fn text(&mut self, text: &str) {
        if text.trim().is_empty() {
            return;
        }
        if !self.text.is_empty() {
            self.text.push_str("\n\n");
        }
        self.text.push_str(text.trim());
        self.last_block = Some(TranscriptBlock::Text);
    }

    fn thought(&mut self, text: &str) {
        if !self.thinking.is_empty() {
            self.thinking.push_str("\n\n");
        }
        self.thinking.push_str(text);
        self.last_block = Some(TranscriptBlock::Thinking);
    }

    fn open(
        &mut self,
        id: String,
        title: Option<String>,
        input: Option<&JsonObject>,
    ) -> Result<(), TranscriptReaderError> {
        let Some(name) = title else {
            return Ok(());
        };
        let kept = match input.filter(|_| keeps_input(&name)) {
            Some(input) => kept_object(input).map_err(|error| TranscriptReaderError::Read {
                message: format!("grok tool call {id} input did not serialize"),
                source: Box::new(error),
            })?,
            None => None,
        };
        self.tools.push(HarnessTranscriptTool {
            status: TranscriptToolStatus::Running,
            args: input.and_then(args_summary),
            id: Some(id),
            input: kept,
            result_text: None,
            name,
        });
        self.last_block = Some(TranscriptBlock::ToolUse);
        Ok(())
    }

    fn update(
        &mut self,
        position: u64,
        update: ToolUpdate,
    ) -> Result<Option<SkippedRecord>, TranscriptReaderError> {
        let ToolUpdate {
            tool_call_id,
            status,
            title,
            raw_input,
            raw_output,
            content,
        } = update;
        let status = match status {
            Some(ToolStatus::Completed) => TranscriptToolStatus::Done,
            Some(ToolStatus::Failed) => TranscriptToolStatus::Error,
            Some(ToolStatus::Pending | ToolStatus::InProgress | ToolStatus::Other) | None => {
                return Ok(None);
            }
        };
        if !self.tools.iter().any(|tool| is_call(tool, &tool_call_id)) {
            self.open(tool_call_id.clone(), title, raw_input.as_ref())?;
        }
        self.last_block = Some(TranscriptBlock::ToolResult);
        let Some(tool) = self
            .tools
            .iter_mut()
            .find(|tool| is_call(tool, &tool_call_id))
        else {
            return Ok(None);
        };
        tool.status = status;
        if !is_prompt_tool(&tool.name) {
            return Ok(None);
        }
        match tool_result(position, raw_output, &content) {
            Ok(text) => {
                tool.result_text = text.map(|text| text.chars().take(RESULT_CHARS).collect());
                Ok(None)
            }
            Err(skipped) => Ok(Some(skipped)),
        }
    }

    fn close(self) -> Option<HarnessTranscriptTurn> {
        if self.text.is_empty() && self.thinking.is_empty() && self.tools.is_empty() {
            return None;
        }
        let complete = self.stop_reason.as_deref() == Some(END_TURN)
            && !self
                .tools
                .iter()
                .any(|tool| tool.status == TranscriptToolStatus::Running);
        Some(HarnessTranscriptTurn {
            role: SessionMessageRole::Assistant,
            text: self.text,
            thinking: (!self.thinking.is_empty()).then(|| tail_chars(&self.thinking)),
            tools: (!self.tools.is_empty()).then_some(self.tools),
            usage: None,
            model: None,
            stop_reason: self.stop_reason,
            last_block: self.last_block,
            attachments: None,
            complete,
            compact: false,
            started_at_ms: self.started_at_ms,
            ended_at_ms: self.ended_at_ms,
        })
    }
}

fn is_call(tool: &HarnessTranscriptTool, id: &str) -> bool {
    tool.id.as_deref() == Some(id)
}
