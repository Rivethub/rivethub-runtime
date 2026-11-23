use kernel::memory::TitleAnswer;
use kernel::memory::fit_title;
use kernel::memory::title_schema;
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
const TITLE_INPUT_CHARS: usize = 8_000;
const TITLE_MAX_TOKENS: u32 = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Named {
    Title(String),
    Unfit(String),
}

#[expect(missing_debug_implementations)]
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
                enable_thinking: Some(false),
                json_schema: Some(title_schema()),
                disable_any_whitespace: false,
            },
            model: self.model.to_owned(),
            messages: vec![
                message(Role::System, TITLE_SYSTEM_PROMPT.to_owned()),
                message(Role::User, input(summaries)),
            ],
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

pub(crate) async fn handle_opening_title(
    env: &SummarizeEnv<'_>,
    id: uuid::Uuid,
) -> Result<(), WorkerError> {
    let Some(opening) = env.store.opening_title(id).await? else {
        return Ok(());
    };
    if !opening.armed {
        return Ok(());
    }
    let suggested = if opening.wants_model() {
        let call = TitleCall {
            provider: env.provider,
            model: &env.settings.model,
            cancel: env.cancel,
        };
        match call.name(&[&opening.content]).await? {
            Named::Title(title) => Some(title),
            Named::Unfit(_) => None,
        }
    } else {
        None
    };
    env.store
        .finish_opening_title(&opening, suggested.as_deref())
        .await?;
    Ok(())
}

pub(crate) async fn handle_enqueue_openings(env: &SummarizeEnv<'_>) -> Result<(), WorkerError> {
    let jobs = kernel::tenancy::Scope::Maintenance
        .run(env.store.undelivered_openings())
        .await?;
    for job in jobs {
        super::llm::cancelled(env)?;
        env.queue.enqueue(job).await.map_err(WorkerError::queue)?;
    }
    Ok(())
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
