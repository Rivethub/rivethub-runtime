use kernel::harness::HarnessAdapterError;
use kernel::model::ModelProviderError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CliError {
    #[error(
        "CLI provider embeddings require embed_endpoint and embed_model on the selected memory backend"
    )]
    MissingEmbedder,
    #[error("provider type does not select a CLI harness")]
    ProviderType,
    #[error("CLI provider timeout_ms must be positive")]
    TimeoutConfig,
    #[error("CLI provider is shutting down")]
    Closed,
    #[error("CLI provider turn was cancelled")]
    Cancelled,
    #[error("CLI provider turn timed out")]
    Timeout,
    #[error("CLI provider stream consumer disconnected")]
    Disconnected,
    #[error("harness event stream ended before the turn completed")]
    Incomplete,
    #[error("harness exited before the turn completed: {code:?}")]
    Exited { code: Option<i32> },
    #[error(transparent)]
    Harness(#[from] HarnessAdapterError),
    #[error("could not serialize the headless prompt: {0}")]
    Json(#[from] serde_json::Error),
    #[error("headless turn failed: {turn}; session cleanup also failed: {cleanup}")]
    Cleanup { turn: Box<Self>, cleanup: Box<Self> },
}

impl From<CliError> for ModelProviderError {
    fn from(source: CliError) -> Self {
        Self::Stream {
            message: source.to_string(),
            source: Box::new(source),
        }
    }
}
