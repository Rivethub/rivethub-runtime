use wire::gateway::sessions;

pub(crate) fn turns(
    turns: Vec<kernel::transcript::HarnessTranscriptTurn>,
) -> Vec<sessions::HarnessTranscriptTurn> {
    turns
        .into_iter()
        .map(harness_transcript_turn_body)
        .collect()
}

fn harness_transcript_turn_body(
    value: kernel::transcript::HarnessTranscriptTurn,
) -> sessions::HarnessTranscriptTurn {
    sessions::HarnessTranscriptTurn {
        role: session_message_role_body(value.role),
        text: value.text,
        reply_text: value.reply_text,
        thinking: value.thinking,
        tools: value
            .tools
            .map(|v| v.into_iter().map(harness_transcript_tool_body).collect()),
        usage: value.usage.map(message_usage_body),
        model: value.model,
        stop_reason: value.stop_reason,
        last_block: value.last_block.map(transcript_block_body),
        attachments: value
            .attachments
            .map(|v| v.into_iter().map(transcript_attachment_body).collect()),
        complete: value.complete,
        compact: value.compact,
        started_at_ms: value.started_at_ms,
        ended_at_ms: value.ended_at_ms,
    }
}

fn harness_transcript_tool_body(
    value: kernel::transcript::HarnessTranscriptTool,
) -> sessions::HarnessTranscriptTool {
    sessions::HarnessTranscriptTool {
        name: value.name,
        status: transcript_tool_status_body(value.status),
        args: value.args,
        id: value.id,
        input: value.input,
        result_text: value.result_text,
    }
}

fn transcript_attachment_body(
    value: kernel::transcript::TranscriptAttachment,
) -> sessions::TranscriptAttachment {
    sessions::TranscriptAttachment {
        media_type: value.media_type,
        name: value.name,
        uri: value.uri,
        upload_id: value.upload_id,
    }
}

const fn message_usage_body(value: kernel::transcript::MessageUsage) -> sessions::MessageUsage {
    sessions::MessageUsage {
        prompt_tokens: value.prompt_tokens,
        completion_tokens: value.completion_tokens,
        cached_tokens: value.cached_tokens,
    }
}

const fn transcript_block_body(
    value: kernel::transcript::TranscriptBlock,
) -> sessions::TranscriptBlock {
    match value {
        kernel::transcript::TranscriptBlock::Thinking => sessions::TranscriptBlock::Thinking,
        kernel::transcript::TranscriptBlock::Text => sessions::TranscriptBlock::Text,
        kernel::transcript::TranscriptBlock::ToolUse => sessions::TranscriptBlock::ToolUse,
        kernel::transcript::TranscriptBlock::ToolResult => sessions::TranscriptBlock::ToolResult,
    }
}

const fn transcript_tool_status_body(
    value: kernel::transcript::TranscriptToolStatus,
) -> sessions::TranscriptToolStatus {
    match value {
        kernel::transcript::TranscriptToolStatus::Running => {
            sessions::TranscriptToolStatus::Running
        }
        kernel::transcript::TranscriptToolStatus::Done => sessions::TranscriptToolStatus::Done,
        kernel::transcript::TranscriptToolStatus::Error => sessions::TranscriptToolStatus::Error,
    }
}

const fn session_message_role_body(
    value: kernel::transcript::SessionMessageRole,
) -> sessions::SessionMessageRole {
    match value {
        kernel::transcript::SessionMessageRole::User => sessions::SessionMessageRole::User,
        kernel::transcript::SessionMessageRole::Assistant => {
            sessions::SessionMessageRole::Assistant
        }
    }
}

pub(crate) fn harness_tool_output_body(
    value: kernel::transcript::HarnessToolOutput,
) -> sessions::HarnessToolOutput {
    sessions::HarnessToolOutput {
        text: value.text,
        is_error: value.is_error,
        truncated: value.truncated,
    }
}
