use std::sync::Arc;
use std::time::Duration;

use chrono::Utc;
use kernel::changes::ChangeFeed;
use kernel::changes::DIGEST_SYSTEM_PROMPT;
use kernel::changes::DailyDigest;
use kernel::changes::DigestRequest;
use kernel::changes::DigestStore;
use kernel::changes::clean_digest;
use kernel::changes::digest_prompt;
use kernel::model::ModelOptions;
use kernel::model::ModelProvider;
use kernel::model::ModelRequest;
use tokio_util::sync::CancellationToken;
use wire::JsonObject;
use wire::message::Role;

use crate::WorkerError;

const DIGEST_TIMEOUT: Duration = Duration::from_secs(180);

#[expect(
    missing_debug_implementations,
    reason = "the digest ports are trait objects without Debug"
)]
#[derive(Clone)]
pub struct DailyDigests {
    pub changes: Arc<dyn ChangeFeed>,
    pub digests: Arc<dyn DigestStore>,
}

#[expect(
    missing_debug_implementations,
    reason = "the model provider is a trait object without Debug"
)]
pub struct DigestEnv<'a> {
    pub deps: &'a DailyDigests,
    pub provider: &'a dyn ModelProvider,
    pub model: &'a str,
    pub cancel: &'a CancellationToken,
}

pub async fn handle_digest_day(
    env: &DigestEnv<'_>,
    request: &DigestRequest,
) -> Result<(), WorkerError> {
    let owner = request.owner()?;
    let changes = env.deps.changes.changes(&request.window()).await?;
    if changes.is_empty() {
        return Ok(());
    }
    let model_request = ModelRequest {
        options: ModelOptions {
            temperature: Some(0.3),
            enable_thinking: Some(false),
            ..ModelOptions::default()
        },
        model: env.model.to_owned(),
        messages: vec![
            crate::tool_synthesis::message_text(Role::System, DIGEST_SYSTEM_PROMPT.to_owned()),
            crate::tool_synthesis::message_text(Role::User, digest_prompt(request.day, &changes)),
        ],
        tools: Vec::new(),
        thinking: None,
        max_tokens: Some(400),
    };
    let answer = tokio::select! {
        biased;
        () = env.cancel.cancelled() => return Err(WorkerError::Cancelled),
        answer = tokio::time::timeout(DIGEST_TIMEOUT, env.provider.chat(model_request)) => answer,
    };
    let response = answer
        .map_err(|_| WorkerError::ModelTimeout {
            seconds: DIGEST_TIMEOUT.as_secs(),
        })?
        .map_err(crate::summarize::llm_error)?;
    let summary = response
        .content
        .as_deref()
        .and_then(clean_digest)
        .ok_or(WorkerError::EmptyDigest)?;
    env.deps
        .digests
        .keep_digest(&DailyDigest {
            day: request.day,
            owner,
            summary,
            changes: u32::try_from(changes.len()).map_err(|_| WorkerError::Count)?,
            written_at: Utc::now(),
        })
        .await?;
    tracing::info!(day = %request.day, changes = changes.len(), "daily digest written");
    Ok(())
}

pub fn digest_request(payload: &JsonObject) -> Result<DigestRequest, WorkerError> {
    DigestRequest::from_payload(payload).map_err(WorkerError::from)
}
