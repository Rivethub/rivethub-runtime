use std::sync::Arc;
use std::time::Duration;

use kernel::memory_core::compose_query_input;
use kernel::model::ModelProvider;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
pub struct QueryEmbed {
    provider: Arc<dyn ModelProvider>,
    instruction: String,
    timeout: Duration,
    cancellation: CancellationToken,
}

#[cfg_attr(dylint_lib = "rr_lints", expect(manualderive))]
impl std::fmt::Debug for QueryEmbed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QueryEmbed")
            .field("instruction", &self.instruction)
            .field("timeout", &self.timeout)
            .field("cancellation", &self.cancellation)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, thiserror::Error)]
pub(super) enum EmbedFailure {
    #[error(transparent)]
    Provider(#[from] kernel::model::ModelProviderError),
    #[error("query embedding timed out")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("query embedding cancelled")]
    Cancelled,
    #[error("query embedding must contain one finite, nonzero vector")]
    Invalid,
}

impl QueryEmbed {
    #[must_use]
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        instruction: String,
        timeout: Duration,
        cancellation: CancellationToken,
    ) -> Self {
        Self {
            provider,
            instruction,
            timeout,
            cancellation,
        }
    }

    pub(super) async fn vector(&self, text: &str) -> Result<Vec<f32>, EmbedFailure> {
        let input = compose_query_input(&self.instruction, text);
        let mut embeddings = tokio::select! {
            biased;
            () = self.cancellation.cancelled() => return Err(EmbedFailure::Cancelled),
            result = tokio::time::timeout(self.timeout, self.provider.embed(vec![input])) => result??,
        };
        if embeddings.len() != 1 {
            return Err(EmbedFailure::Invalid);
        }
        let mut vector = embeddings.pop().ok_or(EmbedFailure::Invalid)?.vector;
        if !crate::vector::normalize(&mut vector) {
            return Err(EmbedFailure::Invalid);
        }
        Ok(vector)
    }
}
