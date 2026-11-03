use serde::Deserialize;
use serde::Serialize;
use serde_json::value::RawValue;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

use crate::event_id::ToolInput;

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::Display, strum::EnumString,
)]
#[serde(rename_all = "PascalCase")]
#[strum(serialize_all = "PascalCase")]
pub enum HookEvent {
    UserPromptSubmit,
    PostToolUse,
    Stop,
    SubagentStop,
    SessionEnd,
    #[serde(other)]
    Other,
}

impl HookEvent {
    #[must_use]
    pub fn reads_transcript(self) -> bool {
        matches!(self, Self::Stop | Self::SubagentStop | Self::SessionEnd)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(from = "HookPayloadWire")]
pub struct HookPayload {
    pub session_id: Option<SessionId>,
    pub transcript_path: Option<std::path::PathBuf>,
    pub cwd: Option<std::path::PathBuf>,
    pub hook_event_name: Option<HookEvent>,
    pub prompt: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Option<ToolInput>,
    pub tool_response: Option<Box<RawValue>>,
    pub tool_result: Option<String>,
    pub tool_use_id: Option<String>,
    pub last_assistant_message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "snake_case")]
struct HookPayloadWire {
    session_id: Option<String>,
    transcript_path: Option<std::path::PathBuf>,
    cwd: Option<std::path::PathBuf>,
    hook_event_name: Option<HookEvent>,
    prompt: Option<String>,
    tool_name: Option<String>,
    tool_input: Option<ToolInput>,
    tool_response: Option<Box<RawValue>>,
    tool_result: Option<String>,
    tool_use_id: Option<String>,
    last_assistant_message: Option<String>,
}

impl HookPayload {
    #[must_use]
    pub fn claude_transcript(&self) -> bool {
        match (&self.session_id, &self.transcript_path) {
            (Some(session), Some(path)) => path
                .file_stem()
                .is_some_and(|stem| stem == session.native()),
            _ => true,
        }
    }
}

impl From<HookPayloadWire> for HookPayload {
    fn from(wire: HookPayloadWire) -> Self {
        Self {
            session_id: wire
                .session_id
                .map(|native| SessionId::new(HarnessId::ClaudeCode, &native)),
            transcript_path: wire.transcript_path,
            cwd: wire.cwd,
            hook_event_name: wire.hook_event_name,
            prompt: wire.prompt,
            tool_name: wire.tool_name,
            tool_input: wire.tool_input,
            tool_response: wire.tool_response,
            tool_result: wire.tool_result,
            tool_use_id: wire.tool_use_id,
            last_assistant_message: wire.last_assistant_message,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HookPayload;

    fn payload(session: &str, path: &str) -> HookPayload {
        serde_json::from_str(&format!(
            r#"{{"session_id":"{session}","transcript_path":"{path}","hook_event_name":"Stop"}}"#
        ))
        .unwrap_or_else(|error| unreachable!("fixture: {error}"))
    }

    #[test]
    fn only_a_transcript_named_for_its_session_is_claude_s() {
        let session = "0b1c3a52-5f7e-4b8e-9f36-0d6f1b2a9c41";
        assert!(payload(session, &format!("/p/-home/{session}.jsonl")).claude_transcript());
        assert!(
            !payload(session, &format!("/g/sessions/{session}/updates.jsonl")).claude_transcript()
        );
    }
}
