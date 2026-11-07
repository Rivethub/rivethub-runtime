use async_trait::async_trait;
use kernel::model::Embedding;
use kernel::model::LlmResponse;
use kernel::model::ModelProvider;
use kernel::model::ModelProviderError;
use kernel::model::ModelRequest;
use kernel::model::ModelStreamItem;
use thiserror::Error;
use tokio::sync::mpsc::Receiver;

use crate::embed::Embedder;

#[derive(Debug, Error)]
#[error("this provider only serves embeddings")]
struct EmbedOnly;

#[derive(Debug)]
pub struct EmbedProvider {
    embedder: Embedder,
    model: String,
}

impl EmbedProvider {
    #[must_use]
    pub fn new(embedder: Embedder, model: String) -> Self {
        Self { embedder, model }
    }
}

fn chat_refused() -> ModelProviderError {
    ModelProviderError::Request {
        message: EmbedOnly.to_string(),
        source: Box::new(EmbedOnly),
    }
}

#[async_trait]
impl ModelProvider for EmbedProvider {
    async fn chat(&self, _request: ModelRequest) -> Result<LlmResponse, ModelProviderError> {
        Err(chat_refused())
    }

    async fn stream(
        &self,
        _request: ModelRequest,
    ) -> Result<Receiver<ModelStreamItem>, ModelProviderError> {
        Err(chat_refused())
    }

    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Embedding>, ModelProviderError> {
        self.embedder.embed(texts).await.map_err(Into::into)
    }

    async fn list_models(&self) -> Result<Vec<String>, ModelProviderError> {
        Ok(vec![self.model.clone()])
    }
}
