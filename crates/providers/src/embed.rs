use std::sync::Arc;
use std::time::Duration;

use config::provider::OpenaiProvider;
use kernel::model::Embedding;
use kernel::model::ModelProviderError;
use thiserror::Error;
use wire::stream::openai::EmbeddingRequest;
use wire::stream::openai::EmbeddingResponse;
use wire::stream::openai::EmbeddingRow;

use crate::error::BuildError;
use crate::error::StatusError;
use crate::error::TokenError;
use crate::token::Auth;

#[derive(Debug)]
pub struct EmbedConfig {
    pub endpoint: String,
    pub api_prefix: Option<String>,
    pub model: String,
    pub api_key: Option<String>,
    pub expected_dims: Option<u32>,
    pub timeout_ms: Option<u64>,
    pub query_instruction: Option<String>,
}

#[derive(Debug, Error)]
pub enum EmbedError {
    #[error("the embeddings request failed: {source}")]
    Transport {
        #[source]
        source: reqwest::Error,
    },
    #[error(transparent)]
    Status(#[from] StatusError),
    #[error(transparent)]
    Token(#[from] TokenError),
    #[error("the embeddings response is malformed: {source}")]
    Malformed {
        #[source]
        source: serde_json::Error,
    },
    #[error("embedding index {index} is outside the {count} inputs")]
    IndexOutOfRange { index: usize, count: usize },
    #[error("embedding index {index} appears more than once")]
    DuplicateIndex { index: usize },
    #[error("embedding {index} is missing from the model server response")]
    Missing { index: usize },
    #[error(
        "embedding {index} carries {got} dimensions, the configured embed_expected_dims is {expected}"
    )]
    Dimensions {
        index: usize,
        got: usize,
        expected: usize,
    },
}

#[derive(Debug)]
pub struct Embedder {
    url: String,
    model: String,
    auth: Arc<Auth>,
    dimensions: Option<usize>,
    client: reqwest::Client,
}

impl Embedder {
    pub fn build(config: EmbedConfig) -> Result<Self, BuildError> {
        if config.endpoint.is_empty() {
            return Err(BuildError::MissingEmbedEndpoint);
        }
        if config.model.is_empty() {
            return Err(BuildError::MissingEmbedModel);
        }
        let parsed =
            url::Url::parse(&config.endpoint).map_err(|source| BuildError::BaseUrl { source })?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(BuildError::BaseUrlScheme);
        }
        let dimensions = config
            .expected_dims
            .map(usize::try_from)
            .transpose()
            .map_err(|source| BuildError::EmbedDimensions { source })?;
        let mut builder = reqwest::Client::builder();
        if let Some(timeout_ms) = config.timeout_ms {
            builder = builder.timeout(Duration::from_millis(timeout_ms));
        }
        let client = builder
            .build()
            .map_err(|source| BuildError::HttpClient { source })?;
        let url = crate::body::embeddings_url(
            &config.endpoint,
            &crate::body::normalize_api_prefix(config.api_prefix.as_deref()),
        );
        Ok(Self {
            url,
            model: config.model,
            auth: Arc::new(Auth::static_key(config.api_key)),
            dimensions,
            client,
        })
    }

    pub fn with_provider_auth(mut self, provider: &OpenaiProvider) -> Result<Self, BuildError> {
        self.auth = Arc::new(Auth::from_provider(provider)?);
        Ok(self)
    }

    pub(crate) fn sharing(
        endpoint: &str,
        api_prefix: &str,
        model: String,
        auth: Arc<Auth>,
        client: reqwest::Client,
    ) -> Self {
        Self {
            url: crate::body::embeddings_url(endpoint, api_prefix),
            model,
            auth,
            dimensions: None,
            client,
        }
    }

    pub async fn embed(&self, texts: Vec<String>) -> Result<Vec<Embedding>, EmbedError> {
        let count = texts.len();
        let response = crate::token::dispatch(&self.auth, || {
            self.client.post(&self.url).json(&EmbeddingRequest {
                model: self.model.clone(),
                input: texts.clone(),
            })
        })
        .await?;
        let body = response
            .bytes()
            .await
            .map_err(|source| EmbedError::Transport { source })?;
        let response: EmbeddingResponse =
            serde_json::from_slice(&body).map_err(|source| EmbedError::Malformed { source })?;
        place(response.data, count, self.dimensions)
    }
}

impl From<crate::token::CallError> for EmbedError {
    fn from(error: crate::token::CallError) -> Self {
        match error {
            crate::token::CallError::Token(error) => Self::Token(error),
            crate::token::CallError::Status(error) => Self::Status(error),
            crate::token::CallError::Transport { source } => Self::Transport { source },
        }
    }
}

impl From<EmbedError> for ModelProviderError {
    fn from(error: EmbedError) -> Self {
        match error {
            EmbedError::Status(status) => {
                let message = format!("http {}", status.code());
                Self::Request {
                    message,
                    source: Box::new(status.into_failure()),
                }
            }
            EmbedError::Transport { source } => crate::request_error(source),
            EmbedError::Token(error) => error.into(),
            other @ (EmbedError::Malformed { .. }
            | EmbedError::IndexOutOfRange { .. }
            | EmbedError::DuplicateIndex { .. }
            | EmbedError::Missing { .. }
            | EmbedError::Dimensions { .. }) => {
                let message = other.to_string();
                Self::Request {
                    message,
                    source: Box::new(other),
                }
            }
        }
    }
}

fn place(
    rows: Vec<EmbeddingRow>,
    count: usize,
    dimensions: Option<usize>,
) -> Result<Vec<Embedding>, EmbedError> {
    let mut slots: Vec<Option<Vec<f32>>> = vec![None; count];
    for row in rows {
        let index = row.index;
        if let Some(expected) = dimensions
            && row.embedding.len() != expected
        {
            return Err(EmbedError::Dimensions {
                index,
                got: row.embedding.len(),
                expected,
            });
        }
        let slot = slots
            .get_mut(index)
            .ok_or(EmbedError::IndexOutOfRange { index, count })?;
        if slot.replace(row.embedding).is_some() {
            return Err(EmbedError::DuplicateIndex { index });
        }
    }
    slots
        .into_iter()
        .enumerate()
        .map(|(index, slot)| {
            slot.map(|vector| Embedding { vector })
                .ok_or(EmbedError::Missing { index })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use kernel::model::ModelProviderError;
    use reqwest::StatusCode;

    #[test]
    fn a_status_on_the_embed_port_is_http_status() {
        let error = EmbedError::Status(StatusError::Body {
            status: StatusCode::SERVICE_UNAVAILABLE,
            retry_after: None,
            body: "model not loaded".to_owned(),
        });
        let ModelProviderError::Request { message, source } = ModelProviderError::from(error)
        else {
            panic!("a status becomes a request error");
        };
        assert_eq!(message, "http 503");
        let rendered = source.to_string();
        assert!(rendered.contains("model not loaded"), "{rendered}");
        assert!(rendered.contains("model server returned"), "{rendered}");
    }
}
