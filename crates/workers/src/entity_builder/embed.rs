use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;

use crate::WorkerError;

const EMBED_WIDTH: usize = 1024;
const EMBED_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Serialize)]
struct EmbedRequest<'a> {
    model: &'a str,
    input: &'a str,
}

#[derive(Deserialize)]
struct EmbedResponse {
    data: Vec<EmbedData>,
}

#[derive(Deserialize)]
struct EmbedData {
    embedding: Vec<f32>,
}

pub(super) fn http_client(timeout: Duration) -> Result<reqwest::Client, WorkerError> {
    reqwest::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|error| WorkerError::Llm {
            message: error.to_string(),
        })
}

pub(super) async fn embed_text(base: &str, model: &str, text: &str) -> Option<Vec<f32>> {
    let root = base.trim().trim_end_matches('/');
    if root.is_empty() || text.trim().is_empty() {
        return None;
    }
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: an embedding client that cannot be built leaves the vector absent"
        )
    )]
    let Ok(client) = http_client(EMBED_TIMEOUT) else {
        tracing::warn!("entity embedding client could not be built");
        return None;
    };
    let response = match client
        .post(format!("{root}/v1/embeddings"))
        .json(&EmbedRequest { model, input: text })
        .send()
        .await
    {
        Ok(response) => response,
        Err(error) => {
            tracing::warn!(%error, "entity embedding request failed");
            return None;
        }
    };
    vector_from(response).await
}

async fn vector_from(response: reqwest::Response) -> Option<Vec<f32>> {
    if !response.status().is_success() {
        tracing::warn!(status = %response.status(), "entity embedding request failed");
        return None;
    }
    let body = match response.json::<EmbedResponse>().await {
        Ok(body) => body,
        Err(error) => {
            tracing::warn!(%error, "entity embedding response was not a vector");
            return None;
        }
    };
    let vector = body.data.into_iter().next().map(|item| item.embedding)?;
    if vector.len() == EMBED_WIDTH {
        Some(vector)
    } else {
        tracing::warn!(len = vector.len(), "entity embedding width is not 1024");
        None
    }
}
