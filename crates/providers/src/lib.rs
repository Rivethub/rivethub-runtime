pub mod body;
pub mod cli;
pub mod embed;
pub mod error;
mod retry_after;
pub mod sse;
mod token;

use std::sync::Arc;

use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;

use async_trait::async_trait;
use config::provider::OpenaiProvider;
use config::provider::ProviderType;
use kernel::model::Embedding;
use kernel::model::LlmResponse;
use kernel::model::LlmResponseKind;
use kernel::model::ModelChunk;
use kernel::model::ModelProvider;
use kernel::model::ModelProviderError;
use kernel::model::ModelRequest;
use kernel::model::ModelStreamItem;
use kernel::shutdown::Activities;
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;
use wire::message::ToolCall;
use wire::stream::LlmUsage;

pub use crate::error::BuildError;
use crate::error::StatusError;
use crate::error::StreamFault;
use crate::sse::STREAM_CAPACITY;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Variant {
    OpenaiCompatible,
    Vllm,
}

impl Variant {
    #[must_use]
    pub fn of(kind: ProviderType) -> Option<Self> {
        match kind {
            ProviderType::OpenaiCompatible => Some(Self::OpenaiCompatible),
            ProviderType::Vllm => Some(Self::Vllm),
            ProviderType::ClaudeCli
            | ProviderType::CodexCli
            | ProviderType::GrokCli
            | ProviderType::HermesCli
            | ProviderType::KimiCode
            | ProviderType::OpencodeCli => None,
        }
    }
}

#[derive(Debug)]
pub struct OpenAiCompatible {
    variant: Variant,
    model: String,
    auth: Arc<token::Auth>,
    base_url: String,
    api_prefix: String,
    max_tokens: Option<u32>,
    probe_models: bool,
    client: reqwest::Client,
    embedder: embed::Embedder,
    shutdown: CancellationToken,
    activities: Activities,
}

impl OpenAiCompatible {
    #[must_use]
    pub fn with_embedder(mut self, embedder: embed::Embedder) -> Self {
        self.embedder = embedder;
        self
    }

    pub fn build(kind: ProviderType, provider: &OpenaiProvider) -> Result<Self, BuildError> {
        let variant = Variant::of(kind).ok_or(BuildError::NotOpenaiCompatible)?;
        let model = provider.model.clone().ok_or(BuildError::MissingModel)?;
        let raw = provider
            .base_url
            .as_deref()
            .ok_or(BuildError::MissingBaseUrl)?;
        let parsed = url::Url::parse(raw).map_err(|source| BuildError::BaseUrl { source })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(BuildError::BaseUrlScheme);
        }
        let client = reqwest::Client::builder()
            .build()
            .map_err(|source| BuildError::HttpClient { source })?;
        let base_url = raw.trim().trim_end_matches('/').to_owned();
        let api_prefix = body::normalize_api_prefix(provider.api_prefix.as_deref());
        let auth = Arc::new(token::Auth::from_provider(provider)?);
        let embedder = embed::Embedder::sharing(
            &base_url,
            &api_prefix,
            model.clone(),
            Arc::clone(&auth),
            client.clone(),
        );
        Ok(Self {
            variant,
            model,
            auth,
            base_url,
            api_prefix,
            max_tokens: provider.max_tokens,
            probe_models: probe_unless_disabled(provider.probe_models),
            client,
            embedder,
            shutdown: CancellationToken::new(),
            activities: Activities::new(),
        })
    }

    #[must_use]
    pub fn with_shutdown(mut self, shutdown: CancellationToken) -> Self {
        self.shutdown = shutdown;
        self
    }

    #[must_use]
    pub fn with_activities(mut self, activities: Activities) -> Self {
        self.activities = activities;
        self
    }

    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }

    async fn send_chat(
        &self,
        request: &ModelRequest,
        finish: Option<Arc<AtomicU8>>,
    ) -> Result<Receiver<ModelStreamItem>, ModelProviderError> {
        let body = body::chat_body(self.variant, request, self.max_tokens)?;
        let url = body::chat_url(&self.base_url, &self.api_prefix);
        let pending = token::dispatch(&self.auth, || self.client.post(url.clone()).json(&body));
        tokio::pin!(pending);
        let response = tokio::select! {
            biased;
            () = self.shutdown.cancelled() => return Err(cancelled_provider()),
            result = &mut pending => result?,
        };
        let (tx, rx) = tokio::sync::mpsc::channel(STREAM_CAPACITY);
        let pump = tokio::spawn(sse::pump(response, tx, finish));
        let abort = pump.abort_handle();
        let shutdown = self.shutdown.clone();
        let activities = self.activities.clone();
        let watcher = tokio::spawn(async move {
            let _activity = activities.enter("provider chat");
            tokio::select! {
                biased;
                () = shutdown.cancelled() => abort.abort(),
                joined = pump => {
                    if let Err(error) = joined {
                        tracing::debug!("provider stream task ended: {error}");
                    }
                }
            }
        });
        drop(watcher);
        Ok(rx)
    }
}

fn cancelled_provider() -> ModelProviderError {
    ModelProviderError::Request {
        message: "the provider call was cancelled".to_owned(),
        source: Box::new(std::io::Error::new(
            std::io::ErrorKind::Interrupted,
            "the provider call was cancelled",
        )),
    }
}

pub(crate) fn request_error(source: reqwest::Error) -> ModelProviderError {
    let message = format!("the model server request failed: {source}");
    let failure = if source.is_connect() {
        kernel::model::ModelCallFailure::Unreachable {
            source: Box::new(source),
        }
    } else {
        kernel::model::ModelCallFailure::Transport {
            source: Box::new(source),
        }
    };
    ModelProviderError::Request {
        message,
        source: Box::new(failure),
    }
}

pub(crate) async fn check_status(
    response: reqwest::Response,
) -> Result<reqwest::Response, StatusError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let retry_after = retry_after::hint(response.headers(), chrono::Utc::now());
    match response.text().await {
        Ok(body) => Err(StatusError::Body {
            status,
            retry_after,
            body,
        }),
        Err(source) => Err(StatusError::Unreadable {
            status,
            retry_after,
            source,
        }),
    }
}

#[async_trait]
impl ModelProvider for OpenAiCompatible {
    async fn chat(&self, request: ModelRequest) -> Result<LlmResponse, ModelProviderError> {
        let finish = Arc::new(AtomicU8::new(0));
        let rx = self.send_chat(&request, Some(Arc::clone(&finish))).await?;
        let collect = collect_response(rx);
        tokio::pin!(collect);
        let mut response = tokio::select! {
            biased;
            () = self.shutdown.cancelled() => return Err(cancelled_provider()),
            result = &mut collect => result?,
        };
        response.finish_reason = sse::decode_finish(finish.load(Ordering::Relaxed));
        Ok(response)
    }

    async fn stream(
        &self,
        request: ModelRequest,
    ) -> Result<Receiver<ModelStreamItem>, ModelProviderError> {
        self.send_chat(&request, None).await
    }

    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Embedding>, ModelProviderError> {
        let _activity = self.activities.enter("provider embed");
        let call = self.embedder.embed(texts);
        tokio::pin!(call);
        tokio::select! {
            biased;
            () = self.shutdown.cancelled() => Err(cancelled_provider()),
            result = &mut call => result.map_err(Into::into),
        }
    }

    async fn list_models(&self) -> Result<Vec<String>, ModelProviderError> {
        if !self.probe_models {
            return Ok(vec![self.model.clone()]);
        }
        let url = body::models_url(&self.base_url, &self.api_prefix);
        let pending = token::dispatch(&self.auth, || self.client.get(url.clone()));
        tokio::pin!(pending);
        let response = tokio::select! {
            biased;
            () = self.shutdown.cancelled() => return Err(cancelled_provider()),
            result = &mut pending => result?,
        };
        let list: wire::stream::openai::ModelList = response.json().await.map_err(request_error)?;
        Ok(list.data.into_iter().map(|model| model.id).collect())
    }
}

pub(crate) async fn collect_response(
    mut rx: Receiver<ModelStreamItem>,
) -> Result<LlmResponse, ModelProviderError> {
    let mut content = String::new();
    let mut calls: Vec<PartialToolCall> = Vec::new();
    let mut usage: Option<LlmUsage> = None;
    let mut done = false;
    while let Some(item) = rx.recv().await {
        match item? {
            ModelChunk::Text { delta } => content.push_str(&delta),
            ModelChunk::Reasoning { delta: _ } => {}
            ModelChunk::ToolCallStart { index, id, name } => {
                if calls.iter().any(|call| call.index == index) {
                    return Err(StreamFault::RestartedToolCall { index }.into());
                }
                calls.push(PartialToolCall {
                    index,
                    id,
                    name,
                    arguments: String::new(),
                });
            }
            ModelChunk::ToolCallDelta {
                index,
                arguments_delta,
            } => {
                let Some(call) = calls.iter_mut().find(|call| call.index == index) else {
                    return Err(StreamFault::UnknownToolCall { index }.into());
                };
                call.arguments.push_str(&arguments_delta);
            }
            ModelChunk::Usage { usage: received } => usage = Some(received),
            ModelChunk::Done => done = true,
        }
    }
    if !done {
        return Err(StreamFault::EndedBeforeDone.into());
    }
    let tool_calls = assemble_tool_calls(&calls)?;
    let r#type = if tool_calls.is_some() {
        LlmResponseKind::ToolCalls
    } else {
        LlmResponseKind::Text
    };
    Ok(LlmResponse {
        r#type,
        content: if content.is_empty() {
            None
        } else {
            Some(content)
        },
        tool_calls,
        usage,
        finish_reason: None,
    })
}

struct PartialToolCall {
    index: u32,
    id: String,
    name: String,
    arguments: String,
}

fn probe_unless_disabled(probe_models: Option<bool>) -> bool {
    probe_models != Some(false)
}

fn assemble_tool_calls(
    calls: &[PartialToolCall],
) -> Result<Option<Vec<ToolCall>>, ModelProviderError> {
    if calls.is_empty() {
        return Ok(None);
    }
    let mut out = Vec::with_capacity(calls.len());
    for call in calls {
        let arguments: wire::JsonObject =
            serde_json::from_str(&call.arguments).map_err(|source| {
                sse::stream_error(
                    format!(
                        "tool call {} carried malformed arguments: {source}",
                        call.index
                    ),
                    source,
                )
            })?;
        out.push(ToolCall {
            id: call.id.clone(),
            name: call.name.clone(),
            arguments,
            thought_signature: None,
        });
    }
    Ok(Some(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_models_probe_runs_unless_disabled() {
        assert!(probe_unless_disabled(None));
        assert!(probe_unless_disabled(Some(true)));
        assert!(!probe_unless_disabled(Some(false)));
    }

    #[test]
    fn the_variant_accepts_only_the_openai_family() {
        assert_eq!(Variant::of(ProviderType::Vllm), Some(Variant::Vllm));
        assert_eq!(
            Variant::of(ProviderType::OpenaiCompatible),
            Some(Variant::OpenaiCompatible)
        );
        for kind in [
            ProviderType::ClaudeCli,
            ProviderType::CodexCli,
            ProviderType::GrokCli,
            ProviderType::HermesCli,
            ProviderType::KimiCode,
            ProviderType::OpencodeCli,
        ] {
            assert_eq!(Variant::of(kind), None);
        }
    }
}
