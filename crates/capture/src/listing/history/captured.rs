use kernel::transcript::HarnessToolOutput;
use kernel::transcript::HarnessTranscriptTool;
use kernel::transcript::HarnessTranscriptTurn;
use kernel::transcript::SessionMessageRole;
use kernel::transcript::StoreTranscript;
use kernel::transcript::TranscriptBlock;
use kernel::transcript::TranscriptReaderError;
use kernel::transcript::TranscriptToolStatus;
use wire::JsonObject;
use wire::capture::CaptureMessage;
use wire::message::Role;

use super::display::tail_chars;
use super::tools::RESULT_CHARS;
use super::tools::args_summary;
use super::tools::is_prompt_tool;
use super::tools::keeps_input;
use super::tools::kept_object;

const THINKING: &str = "[thinking] ";
const CALL: &str = "[tool] ";
const RESULT: &str = "[tool-result]";
const FAILURE: &str = "[tool-failure]";
const OUTPUT_BYTES: usize = 64 * 1024;

pub(super) fn transcript(
    messages: Vec<CaptureMessage>,
) -> Result<StoreTranscript, TranscriptReaderError> {
    Ok(StoreTranscript {
        turns: fold(messages)?.turns,
        earlier: None,
        context: None,
    })
}

pub(super) fn tool_output(
    messages: Vec<CaptureMessage>,
    call: &str,
) -> Result<Option<HarnessToolOutput>, TranscriptReaderError> {
    let found = fold(messages)?
        .outputs
        .into_iter()
        .find(|output| output.call == call);
    Ok(found.map(|output| {
        let truncated = output.text.len() > OUTPUT_BYTES;
        HarnessToolOutput {
            text: if truncated {
                tail_bytes(&output.text)
            } else {
                output.text
            },
            is_error: output.failed,
            truncated,
        }
    }))
}

fn fold(messages: Vec<CaptureMessage>) -> Result<Folder, TranscriptReaderError> {
    let mut folder = Folder::default();
    for message in messages {
        folder.message(message)?;
    }
    folder.flush();
    Ok(folder)
}

fn tail_bytes(text: &str) -> String {
    let start = text.ceil_char_boundary(text.len().saturating_sub(OUTPUT_BYTES));
    text.get(start..).unwrap_or_default().to_owned()
}

#[derive(Debug)]
struct Output {
    call: String,
    text: String,
    failed: bool,
}

#[derive(Debug, Default)]
struct Folder {
    turns: Vec<HarnessTranscriptTurn>,
    pending: Option<Draft>,
    outputs: Vec<Output>,
}

#[derive(Debug, Default)]
struct Draft {
    text: String,
    thinking: String,
    tools: Vec<HarnessTranscriptTool>,
    last_block: Option<TranscriptBlock>,
    started_at_ms: Option<i64>,
    ended_at_ms: Option<i64>,
}

impl Folder {
    fn message(&mut self, message: CaptureMessage) -> Result<(), TranscriptReaderError> {
        let at = message.created_at.map(|at| at.timestamp_millis());
        match message.role {
            Role::User => self.user(message.content, at),
            Role::Assistant => match message.content.strip_prefix(THINKING) {
                Some(thought) => self.reply(at, true).thought(thought),
                None => self.reply(at, false).text(&message.content),
            },
            Role::Tool => {
                let Some(name) = message.tool_name else {
                    return Ok(());
                };
                if message.content.starts_with(CALL) {
                    self.draft(at)
                        .open(message.event_id, name, message.tool_args.as_ref())?;
                } else if message.content.starts_with(RESULT)
                    || message.content.starts_with(FAILURE)
                {
                    let failed = message.content.starts_with(FAILURE);
                    self.answer(&name, message.tool_result.unwrap_or_default(), failed, at);
                } else {
                    self.draft(at).open(
                        message.event_id,
                        name.clone(),
                        message.tool_args.as_ref(),
                    )?;
                    let result = message.tool_result.unwrap_or(message.content);
                    self.answer(&name, result, false, at);
                }
            }
            Role::System => {}
        }
        Ok(())
    }

    fn user(&mut self, text: String, at: Option<i64>) {
        self.flush();
        self.turns.push(HarnessTranscriptTurn {
            role: SessionMessageRole::User,
            text,
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

    fn reply(&mut self, at: Option<i64>, thought: bool) -> &mut Draft {
        let spoken = self
            .pending
            .as_ref()
            .is_some_and(|draft| !draft.tools.is_empty() || (thought && !draft.text.is_empty()));
        if spoken {
            self.flush();
        }
        self.draft(at)
    }

    fn draft(&mut self, at: Option<i64>) -> &mut Draft {
        let draft = self.pending.get_or_insert_with(Draft::default);
        draft.stamp(at);
        draft
    }

    fn answer(&mut self, name: &str, result: String, failed: bool, at: Option<i64>) {
        let waiting = |tool: &&mut HarnessTranscriptTool| {
            tool.status == TranscriptToolStatus::Running && tool.name == name
        };
        let pending = self
            .pending
            .as_mut()
            .and_then(|draft| draft.tools.iter_mut().find(waiting));
        let tool = match pending {
            Some(tool) => Some(tool),
            None => self
                .turns
                .iter_mut()
                .rev()
                .filter_map(|turn| turn.tools.as_mut())
                .find_map(|tools| tools.iter_mut().find(waiting)),
        };
        let Some(tool) = tool else {
            return;
        };
        tool.status = if failed {
            TranscriptToolStatus::Error
        } else {
            TranscriptToolStatus::Done
        };
        if is_prompt_tool(&tool.name) {
            tool.result_text = Some(result.chars().take(RESULT_CHARS).collect());
        }
        if let Some(call) = tool.id.clone() {
            self.outputs.push(Output {
                call,
                text: result,
                failed,
            });
        }
        if let Some(draft) = self.pending.as_mut() {
            draft.stamp(at);
            draft.last_block = Some(TranscriptBlock::ToolResult);
        }
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
        if text.trim().is_empty() {
            return;
        }
        if !self.thinking.is_empty() {
            self.thinking.push_str("\n\n");
        }
        self.thinking.push_str(text.trim());
        self.last_block = Some(TranscriptBlock::Thinking);
    }

    fn open(
        &mut self,
        id: String,
        name: String,
        input: Option<&JsonObject>,
    ) -> Result<(), TranscriptReaderError> {
        let kept = match input.filter(|_| keeps_input(&name)) {
            Some(input) => kept_object(input).map_err(|error| TranscriptReaderError::Read {
                message: format!("tool call {id} input did not serialize"),
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

    fn close(self) -> Option<HarnessTranscriptTurn> {
        if self.text.is_empty() && self.thinking.is_empty() && self.tools.is_empty() {
            return None;
        }
        Some(HarnessTranscriptTurn {
            role: SessionMessageRole::Assistant,
            text: self.text,
            thinking: (!self.thinking.is_empty()).then(|| tail_chars(&self.thinking)),
            tools: (!self.tools.is_empty()).then_some(self.tools),
            usage: None,
            model: None,
            stop_reason: None,
            last_block: self.last_block,
            attachments: None,
            complete: false,
            compact: false,
            started_at_ms: self.started_at_ms,
            ended_at_ms: self.ended_at_ms,
        })
    }
}
