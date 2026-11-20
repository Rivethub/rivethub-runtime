use wire::JsonObject;
use wire::capture::{CaptureBatchRequest, CaptureMessage};
use wire::message::Role;
use wire::session_id::SessionId;

use super::pending::PendingPrompts;
use super::{FALLBACK_TITLE, HookError, ObservedHook, base_batch, observed_hook, transcript_batch};
use crate::event_id::{OccurrenceKind, ToolInput, occurrence_digest, occurrence_event_id};
use crate::payload::HookPayload;
use crate::transcript::Line;

#[derive(serde::Serialize)]
struct ToolMetadata<'a> {
    #[serde(flatten)]
    observed: ObservedHook<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_use_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_jsonl_path: Option<String>,
    #[serde(flatten)]
    position: Option<transcript_batch::ToolPosition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assistant_uuid: Option<&'a str>,
}

pub(super) async fn batch(
    payload: &HookPayload,
    session: &SessionId,
    transcript: &[Line],
    ingest_key: &str,
    pending: &PendingPrompts,
) -> Result<CaptureBatchRequest, HookError> {
    let session_key = session.as_str();
    let mut batch = base_batch(session_key);
    batch.title = Some(FALLBACK_TITLE.to_owned());
    let tool_name = payload
        .tool_name
        .as_ref()
        .ok_or(HookError::Missing("tool_name"))?;
    let input = payload
        .tool_input
        .as_ref()
        .ok_or(HookError::Missing("tool_input"))?;
    let content = format!("[tool call] {tool_name}");
    let tool_use_id = payload.tool_use_id.as_deref().filter(|id| !id.is_empty());
    let event_id = if let Some(id) = payload.tool_use_id.as_deref() {
        format!("{session_key}:tool:{id}")
    } else {
        let digest = occurrence_digest(OccurrenceKind::Tool, &content, tool_name, Some(input));
        occurrence_event_id(
            session_key,
            &digest,
            prior_occurrences(transcript, tool_name, input),
        )
    };
    let anchor = tool_use_id.and_then(|id| transcript_batch::tool_anchor(transcript, id));
    let sequence = if anchor.is_none() {
        match pending.position(session.native(), event_id.clone()).await {
            Ok(position) => Some(position),
            Err(error) => {
                tracing::warn!(%error, "tool hook position was not saved");
                None
            }
        }
    } else {
        None
    };
    let metadata = ToolMetadata {
        observed: observed_hook("PostToolUse", ingest_key, sequence.as_ref()),
        tool_use_id,
        session_jsonl_path: payload
            .transcript_path
            .as_ref()
            .map(|path| path.display().to_string()),
        position: anchor.map(|(position, _)| position),
        assistant_uuid: anchor.and_then(|(_, uuid)| uuid),
    };
    batch.messages.push(CaptureMessage {
        event_id,
        role: Role::Tool,
        content,
        tool_name: Some(tool_name.clone()),
        tool_args: Some(input.arguments().clone()),
        tool_result: tool_result_text(payload)?,
        metadata: Some(JsonObject::from_serialize(&metadata)?),
        created_at: None,
    });
    Ok(batch)
}

fn prior_occurrences(transcript: &[Line], tool_name: &str, input: &ToolInput) -> usize {
    transcript
        .iter()
        .flat_map(Line::tool_uses)
        .filter(|tool_use| {
            tool_use.name == tool_name && tool_use.input.spelling() == input.spelling()
        })
        .count()
}

fn tool_result_text(payload: &HookPayload) -> Result<Option<String>, HookError> {
    if let Some(result) = &payload.tool_result {
        return Ok(Some(result.clone()));
    }
    let Some(raw) = &payload.tool_response else {
        return Ok(None);
    };
    match serde_json::from_str::<String>(raw.get()) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.is_data() => Ok(Some(serde_json::to_string(raw)?)),
        Err(error) => Err(error.into()),
    }
}
