mod error;
mod turn;

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use config::provider::{CliProvider, ProviderType};
use kernel::harness::{HarnessAdapter, SessionSpec, TurnInput};
use kernel::model::LlmResponse;
use kernel::model::{Embedding, ModelProvider, ModelProviderError, ModelRequest, ModelStreamItem};
use parking_lot::Mutex;
use serde::Serialize;
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use wire::harness::HarnessId;
use wire::levels::Effort;
use wire::session_id::SessionId;
use wire::stream::ThinkingLevel;

pub use error::CliError;

pub struct CliBacked {
    harness: Arc<dyn HarnessAdapter>,
    harness_id: HarnessId,
    config: CliProvider,
    embedder: Option<crate::embed::Embedder>,
    cancel: CancellationToken,
    tasks: Mutex<TaskTracker>,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "PATTERNS MANUALDERIVE: HarnessAdapter is not Debug; this implementation exposes only the harness id"
    )
)]
impl std::fmt::Debug for CliBacked {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CliBacked")
            .field("harness_id", &self.harness_id)
            .finish_non_exhaustive()
    }
}

impl CliBacked {
    #[must_use]
    pub fn with_task_tracker(mut self, tracker: TaskTracker) -> Self {
        self.tasks = Mutex::new(tracker);
        self
    }

    pub fn build(
        kind: ProviderType,
        config: CliProvider,
        harness: Arc<dyn HarnessAdapter>,
        embedder: impl Into<Option<crate::embed::Embedder>>,
        shutdown: &CancellationToken,
    ) -> Result<Self, CliError> {
        if config.timeout_ms == Some(0) {
            return Err(CliError::TimeoutConfig);
        }
        Ok(Self {
            harness,
            harness_id: harness_id(kind)?,
            config,
            embedder: embedder.into(),
            cancel: shutdown.child_token(),
            tasks: Mutex::new(TaskTracker::new()),
        })
    }

    pub async fn shutdown(&self) {
        let tasks = {
            let tasks = self.tasks.lock();
            self.cancel.cancel();
            tasks.close();
            tasks.clone()
        };
        tasks.wait().await;
    }
}

impl Drop for CliBacked {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.tasks.get_mut().close();
    }
}

#[async_trait]
impl ModelProvider for CliBacked {
    async fn chat(&self, request: ModelRequest) -> Result<LlmResponse, ModelProviderError> {
        crate::collect_response(self.stream(request).await?).await
    }

    async fn stream(
        &self,
        request: ModelRequest,
    ) -> Result<Receiver<ModelStreamItem>, ModelProviderError> {
        let prompt = HeadlessInput {
            messages: &request.messages,
            tools: &request.tools,
            system_prompt: self.config.system_prompt.as_deref(),
            max_tokens: request.max_tokens,
        };
        let input = TurnInput::Text(serde_json::to_string(&prompt).map_err(CliError::from)?);
        let spec = SessionSpec {
            session: SessionId::new(self.harness_id, &uuid::Uuid::new_v4().to_string()),
            harness: self.harness_id,
            model: requested_model(request.model),
            effort: request.thinking.or_else(|| {
                self.config
                    .reasoning_effort
                    .or(self.config.effort)
                    .map(thinking)
            }),
            working_dir: self.config.cwd.clone(),
            startup_context: None,
        };
        let (output, receiver) = tokio::sync::mpsc::channel(crate::sse::STREAM_CAPACITY);
        let tasks = self.tasks.lock();
        if self.cancel.is_cancelled() {
            return Err(CliError::Closed.into());
        }
        tasks.spawn(
            turn::Turn {
                harness: Arc::clone(&self.harness),
                spec,
                input,
                timeout: self.config.timeout_ms.map(Duration::from_millis),
                permission_mode: self.config.permission_mode.clone(),
                permission_timeout: self.config.permission_timeout_ms.map(Duration::from_millis),
                cancel: self.cancel.child_token(),
                output,
            }
            .run(),
        );
        drop(tasks);
        Ok(receiver)
    }

    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Embedding>, ModelProviderError> {
        let embedder = self.embedder.as_ref().ok_or(CliError::MissingEmbedder)?;
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(CliError::Closed.into()),
            result = embedder.embed(texts) => result.map_err(Into::into),
        }
    }

    async fn list_models(&self) -> Result<Vec<String>, ModelProviderError> {
        let offerings = tokio::select! {
            biased;
            () = self.cancel.cancelled() => return Err(CliError::Closed.into()),
            result = self.harness.offerings() => result.map_err(CliError::from)?,
        };
        let mut models: BTreeSet<String> = offerings
            .into_iter()
            .filter(|offering| offering.harness_id == self.harness_id && offering.chat)
            .filter_map(|offering| offering.capabilities.models)
            .flatten()
            .map(|model| model.id)
            .collect();
        if let Some(model) = &self.config.model {
            models.insert(model.clone());
        }
        Ok(models.into_iter().collect())
    }
}

#[derive(Serialize)]
struct HeadlessInput<'a> {
    messages: &'a [wire::message::Message],
    tools: &'a [rmcp::model::Tool],
    #[serde(skip_serializing_if = "Option::is_none")]
    system_prompt: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
}

pub const fn harness_id(kind: ProviderType) -> Result<HarnessId, CliError> {
    match kind {
        ProviderType::ClaudeCli => Ok(HarnessId::ClaudeCode),
        ProviderType::CodexCli => Ok(HarnessId::Codex),
        ProviderType::GrokCli => Ok(HarnessId::GrokBuild),
        ProviderType::HermesCli => Ok(HarnessId::Hermes),
        ProviderType::KimiCode => Ok(HarnessId::KimiCode),
        ProviderType::OpencodeCli => Ok(HarnessId::Opencode),
        ProviderType::OpenaiCompatible | ProviderType::Vllm => Err(CliError::ProviderType),
    }
}

const fn thinking(effort: Effort) -> ThinkingLevel {
    match effort {
        Effort::Low => ThinkingLevel::Low,
        Effort::Medium => ThinkingLevel::Medium,
        Effort::High => ThinkingLevel::High,
    }
}

fn requested_model(model: String) -> Option<String> {
    let trimmed = model.trim();
    if trimmed.is_empty() || trimmed == "default" {
        None
    } else {
        Some(model)
    }
}

#[cfg(test)]
mod model_tests {
    use super::requested_model;

    #[test]
    fn the_default_placeholder_leaves_the_harness_model_unpinned() {
        assert_eq!(requested_model("default".into()), None);
        assert_eq!(requested_model(String::new()), None);
        assert_eq!(
            requested_model("gpt-6.1-sol".into()),
            Some("gpt-6.1-sol".into())
        );
    }
}
