use kernel::memory::ConversationHead;
use kernel::memory::TitleAnswer;
use kernel::memory::fit_title;
use kernel::memory::names_session;
use kernel::memory::summary_title;
use kernel::memory::title_schema;
use kernel::memory::wants_model_title;
use kernel::model::ModelOptions;
use kernel::model::ModelProvider;
use kernel::model::ModelRequest;
use tokio_util::sync::CancellationToken;
use wire::message::Message;
use wire::message::MessageContent;
use wire::message::Role;

use super::SummarizeEnv;
use super::llm::llm_error;
use crate::error::WorkerError;

pub const TITLE_SYSTEM_PROMPT: &str = "You title a person's work sessions. Read the session's summary and answer with a short title, 3 to 7 words and never more than 8, that names the work: the task done, the thing changed or the question answered. Use the summary's own names for projects, PRs, hosts and tools. Never describe the session itself: no harness, agent or channel names, ids, dates or times, and no words like session, batch, conversation or summary. Sentence case, no final period. Answer with JSON: {\"title\": \"<the title>\"}.";
pub const TITLE_LEAVES: i64 = 3;
const TITLE_INPUT_CHARS: usize = 8_000;
const TITLE_MAX_TOKENS: u32 = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Named {
    Title(String),
    Unfit(String),
}

#[expect(
    missing_debug_implementations,
    reason = "the model provider is a trait object"
)]
pub struct TitleCall<'a> {
    pub provider: &'a dyn ModelProvider,
    pub model: &'a str,
    pub cancel: &'a CancellationToken,
}

impl TitleCall<'_> {
    pub async fn name(&self, summaries: &[&str]) -> Result<Named, WorkerError> {
        let request = ModelRequest {
            options: ModelOptions {
                temperature: Some(0.0),
                enable_thinking: None,
                json_schema: Some(title_schema()),
                disable_any_whitespace: false,
            },
            model: self.model.to_owned(),
            messages: vec![
                message(Role::System, TITLE_SYSTEM_PROMPT.to_owned()),
                message(Role::User, input(summaries)),
            ],
            tools: Vec::new(),
            thinking: None,
            max_tokens: Some(TITLE_MAX_TOKENS),
        };
        let response = tokio::select! {
            biased;
            () = self.cancel.cancelled() => return Err(WorkerError::Cancelled),
            result = self.provider.chat(request) => result.map_err(llm_error)?,
        };
        let text = response.content.ok_or(WorkerError::EmptyTitle)?;
        let answer: TitleAnswer = serde_json::from_str(&text).map_err(WorkerError::TitleJson)?;
        Ok(match fit_title(&answer.title) {
            Some(title) => Named::Title(title),
            None => Named::Unfit(answer.title),
        })
    }
}

pub(super) async fn title_first_leaf(
    env: &SummarizeEnv<'_>,
    head: &ConversationHead,
    leaf: &str,
) -> Result<(), WorkerError> {
    let after = ConversationHead {
        title: after_leaf(head.title.as_deref(), leaf),
        ..head.clone()
    };
    if !wants_model_title(&after) {
        return Ok(());
    }
    let call = TitleCall {
        provider: env.provider,
        model: &env.settings.model,
        cancel: env.cancel,
    };
    match call.name(&[leaf]).await {
        Ok(Named::Title(title)) => {
            if env.store.retitle(head.id, &title).await? {
                tracing::info!("titled {}: {title}", head.id);
            }
        }
        Ok(Named::Unfit(answer)) => {
            tracing::info!("kept the title of {}: unfit answer {answer:?}", head.id);
        }
        Err(WorkerError::Cancelled) => return Err(WorkerError::Cancelled),
        Err(error) => tracing::warn!("could not title {}: {error}", head.id),
    }
    Ok(())
}

fn after_leaf(current: Option<&str>, leaf: &str) -> Option<String> {
    match current {
        Some(title) if names_session(title) => Some(title.to_owned()),
        _ => summary_title(leaf).or_else(|| current.map(str::to_owned)),
    }
}

fn input(summaries: &[&str]) -> String {
    summaries
        .join("\n\n")
        .chars()
        .take(TITLE_INPUT_CHARS)
        .collect()
}

fn message(role: Role, text: String) -> Message {
    Message {
        role,
        content: MessageContent::Text(text),
        reasoning: None,
        tool_calls: None,
        tool_call_id: None,
    }
}
