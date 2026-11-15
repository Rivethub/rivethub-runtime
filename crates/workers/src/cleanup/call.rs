use std::time::Duration;

use kernel::model::ModelOptions;
use kernel::model::ModelProvider;
use kernel::model::ModelRequest;
use tokio_util::sync::CancellationToken;
use wire::message::Message;
use wire::message::MessageContent;
use wire::message::Role;

use crate::error::WorkerError;
use crate::summarize::llm_error;

#[expect(missing_debug_implementations)]
pub struct ModelCall<'a> {
    pub provider: &'a dyn ModelProvider,
    pub model: &'a str,
    pub timeout: Duration,
    pub cancel: &'a CancellationToken,
}

impl ModelCall<'_> {
    pub async fn ask(
        &self,
        system: &str,
        user: &str,
        max_tokens: u32,
    ) -> Result<String, WorkerError> {
        let request = ModelRequest {
            options: ModelOptions::default(),
            model: self.model.to_owned(),
            messages: vec![message(Role::System, system), message(Role::User, user)],
            tools: Vec::new(),
            thinking: None,
            max_tokens: Some(max_tokens),
        };
        let answered = tokio::select! {
            biased;
            () = self.cancel.cancelled() => return Err(WorkerError::Cancelled),
            answered = tokio::time::timeout(self.timeout, self.provider.chat(request)) => answered,
        };
        let response = answered
            .map_err(|_| WorkerError::ModelTimeout {
                seconds: self.timeout.as_secs(),
            })?
            .map_err(llm_error)?;
        match response.content {
            Some(text) if !text.trim().is_empty() => Ok(text),
            Some(_) | None => Err(WorkerError::Cleanup {
                message: "the model returned no answer".to_owned(),
            }),
        }
    }
}

fn message(role: Role, text: &str) -> Message {
    Message {
        role,
        content: MessageContent::Text(text.to_owned()),
        reasoning: None,
        tool_calls: None,
        tool_call_id: None,
    }
}
