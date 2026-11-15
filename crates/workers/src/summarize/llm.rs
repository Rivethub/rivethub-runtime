use kernel::model::LlmResponse;
use kernel::model::ModelProviderError;
use kernel::model::ModelRequest;
use wire::message::Message;
use wire::message::MessageContent;
use wire::message::Role;

use super::SummarizeEnv;
use crate::error::WorkerError;

pub(super) async fn chat(
    env: &SummarizeEnv<'_>,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> Result<LlmResponse, WorkerError> {
    cancelled(env)?;
    let request = ModelRequest {
        options: kernel::model::ModelOptions::default(),
        model: env.settings.model.clone(),
        messages: vec![
            text_message(Role::System, system),
            text_message(Role::User, user),
        ],
        thinking: None,
        max_tokens: Some(max_tokens),
    };
    tokio::select! {
        biased;
        () = env.cancel.cancelled() => Err(WorkerError::Cancelled),
        result = env.provider.chat(request) => result.map_err(llm_error),
    }
}

pub(super) fn require_text(response: &LlmResponse) -> Result<String, WorkerError> {
    match response.content.as_deref() {
        Some(text) if !text.is_empty() => Ok(text.to_owned()),
        Some(_) | None => Err(WorkerError::EmptySummary),
    }
}

pub(super) fn cancelled(env: &SummarizeEnv<'_>) -> Result<(), WorkerError> {
    if env.cancel.is_cancelled() {
        Err(WorkerError::Cancelled)
    } else {
        Ok(())
    }
}

fn text_message(role: Role, text: &str) -> Message {
    Message {
        role,
        content: MessageContent::Text(text.to_owned()),
        reasoning: None,
        tool_calls: None,
        tool_call_id: None,
    }
}

pub(crate) fn llm_error(error: ModelProviderError) -> WorkerError {
    if let Some(outage) = crate::outage::provider_outage(&error) {
        return outage;
    }
    let text = error.to_string();
    let _owned = error;
    match http_status(&text) {
        Some(status) if permanent(status) => WorkerError::Llm {
            message: format!("LLM HTTP {status} (not retrying)"),
        },
        Some(status) => WorkerError::Llm {
            message: format!("LLM HTTP {status}"),
        },
        None => WorkerError::Llm { message: text },
    }
}

fn permanent(status: u16) -> bool {
    (400..500).contains(&status) && status != 408 && status != 429
}

fn http_status(text: &str) -> Option<u16> {
    const MARKER: &str = "model server returned ";
    let index = text.find(MARKER)?;
    let rest = text.get(index.saturating_add(MARKER.len())..)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    parse_status(&digits)
}

fn parse_status(digits: &str) -> Option<u16> {
    if digits.is_empty() || digits.len() > 3 {
        return None;
    }
    let mut value: u16 = 0;
    for ch in digits.chars() {
        let digit = decimal(ch)?;
        let scaled = value.checked_mul(10)?;
        value = scaled.checked_add(digit)?;
    }
    Some(value)
}

fn decimal(ch: char) -> Option<u16> {
    match ch {
        '0' => Some(0),
        '1' => Some(1),
        '2' => Some(2),
        '3' => Some(3),
        '4' => Some(4),
        '5' => Some(5),
        '6' => Some(6),
        '7' => Some(7),
        '8' => Some(8),
        '9' => Some(9),
        _ => None,
    }
}
