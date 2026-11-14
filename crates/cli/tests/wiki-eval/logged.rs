use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Instant;

use async_trait::async_trait;
use kernel::model::Embedding;
use kernel::model::FinishReason;
use kernel::model::LlmResponse;
use kernel::model::ModelProvider;
use kernel::model::ModelProviderError;
use kernel::model::ModelRequest;
use kernel::model::ModelStreamItem;
use providers::OpenAiCompatible;
use serde::Serialize;
use tokio::sync::Mutex;
use tokio::sync::mpsc::Receiver;
use wire::message::Message;

use crate::failure::redact;

#[derive(Debug)]
pub(crate) struct LoggedProvider {
    inner: OpenAiCompatible,
    dir: PathBuf,
    secrets: Vec<String>,
    calls: AtomicU64,
    prompt_tokens: AtomicU64,
    completion_tokens: AtomicU64,
    log_error: Mutex<Option<String>>,
}

impl LoggedProvider {
    pub(crate) fn new(inner: OpenAiCompatible, dir: PathBuf, secrets: Vec<String>) -> Self {
        Self {
            inner,
            dir,
            secrets,
            calls: AtomicU64::new(0),
            prompt_tokens: AtomicU64::new(0),
            completion_tokens: AtomicU64::new(0),
            log_error: Mutex::new(None),
        }
    }

    pub(crate) fn totals(&self) -> Totals {
        Totals {
            calls: self.calls.load(Ordering::Relaxed),
            prompt_tokens: self.prompt_tokens.load(Ordering::Relaxed),
            completion_tokens: self.completion_tokens.load(Ordering::Relaxed),
        }
    }

    pub(crate) async fn log_error(&self) -> Option<String> {
        self.log_error.lock().await.clone()
    }

    async fn record(&self, request: &ModelRequest, latency_ms: u64, outcome: &CallOutcome<'_>) {
        let index = self.calls.fetch_add(1, Ordering::Relaxed);
        let (prompt_tokens, completion_tokens, reasoning_tokens) = tokens(outcome);
        self.prompt_tokens
            .fetch_add(prompt_tokens, Ordering::Relaxed);
        self.completion_tokens
            .fetch_add(completion_tokens, Ordering::Relaxed);
        let log = CallLog {
            model: &request.model,
            latency_ms,
            prompt_tokens,
            completion_tokens,
            reasoning_tokens,
            finish_reason: outcome.finish.map(finish_name),
            error: outcome
                .error
                .map(|error| redact(&error.to_string(), &self.secrets)),
            messages: &request.messages,
            content: outcome.content,
        };
        let path = self.dir.join(format!("{index:06}.json"));
        let written = match serde_json::to_vec_pretty(&log) {
            Ok(bytes) => tokio::fs::write(&path, bytes).await.err(),
            Err(error) => Some(std::io::Error::other(error)),
        };
        if let Some(error) = written {
            let mut slot = self.log_error.lock().await;
            if slot.is_none() {
                *slot = Some(format!("cannot write {}: {error}", path.display()));
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Totals {
    pub(crate) calls: u64,
    pub(crate) prompt_tokens: u64,
    pub(crate) completion_tokens: u64,
}

struct CallOutcome<'a> {
    content: Option<&'a str>,
    finish: Option<FinishReason>,
    usage_prompt: u64,
    usage_completion: u64,
    usage_reasoning: Option<u64>,
    error: Option<&'a ModelProviderError>,
}

#[derive(Serialize)]
struct CallLog<'a> {
    model: &'a str,
    latency_ms: u64,
    prompt_tokens: u64,
    completion_tokens: u64,
    reasoning_tokens: Option<u64>,
    finish_reason: Option<&'static str>,
    error: Option<String>,
    messages: &'a [Message],
    content: Option<&'a str>,
}

fn tokens(outcome: &CallOutcome<'_>) -> (u64, u64, Option<u64>) {
    (
        outcome.usage_prompt,
        outcome.usage_completion,
        outcome.usage_reasoning,
    )
}

fn finish_name(reason: FinishReason) -> &'static str {
    match reason {
        FinishReason::Stop => "stop",
        FinishReason::Length => "length",
        FinishReason::ToolCalls => "tool_calls",
        FinishReason::ContentFilter => "content_filter",
    }
}

#[async_trait]
impl ModelProvider for LoggedProvider {
    async fn chat(&self, request: ModelRequest) -> Result<LlmResponse, ModelProviderError> {
        let started = Instant::now();
        let outcome = self.inner.chat(request.clone()).await;
        let latency_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
        let recorded = match &outcome {
            Ok(response) => CallOutcome {
                content: response.content.as_deref(),
                finish: response.finish_reason,
                usage_prompt: response
                    .usage
                    .as_ref()
                    .map_or(0, |usage| usage.prompt_tokens),
                usage_completion: response
                    .usage
                    .as_ref()
                    .map_or(0, |usage| usage.completion_tokens),
                usage_reasoning: response
                    .usage
                    .as_ref()
                    .and_then(|usage| usage.reasoning_tokens),
                error: None,
            },
            Err(error) => CallOutcome {
                content: None,
                finish: None,
                usage_prompt: 0,
                usage_completion: 0,
                usage_reasoning: None,
                error: Some(error),
            },
        };
        self.record(&request, latency_ms, &recorded).await;
        outcome
    }

    async fn stream(
        &self,
        request: ModelRequest,
    ) -> Result<Receiver<ModelStreamItem>, ModelProviderError> {
        self.inner.stream(request).await
    }

    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Embedding>, ModelProviderError> {
        self.inner.embed(texts).await
    }

    async fn list_models(&self) -> Result<Vec<String>, ModelProviderError> {
        self.inner.list_models().await
    }
}
