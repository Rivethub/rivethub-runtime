use async_trait::async_trait;
use kernel::model::LlmResponse;
use kernel::model::{
    Embedding, ModelChunk, ModelProvider, ModelProviderError, ModelRequest, ModelStreamItem,
};
use tokio::sync::{Mutex, mpsc::Receiver};
#[derive(Debug, Clone)]
pub enum Script {
    Chat(LlmResponse),
    Stream(Vec<ModelChunk>),
    Embed(Vec<Embedding>),
    EmbedHttpStatus(u16),
    EmbedNetwork,
    EmbedHang,
    Models(Vec<String>),
}

#[derive(Debug, Default)]
pub struct FakeProvider {
    script: Mutex<Vec<Script>>,
    fnv_dims: Mutex<Option<usize>>,
    embed_requests: Mutex<Vec<Vec<String>>>,
    chat_requests: Mutex<Vec<ModelRequest>>,
    http_status: Mutex<Option<u16>>,
}

impl FakeProvider {
    pub fn new(script: Vec<Script>) -> Self {
        Self {
            script: Mutex::new(script),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn fnv_embeddings(width: usize) -> Self {
        Self {
            fnv_dims: Mutex::new(Some(width)),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn http_errors(status: u16) -> Self {
        Self {
            http_status: Mutex::new(Some(status)),
            ..Self::default()
        }
    }

    pub async fn chat_requests(&self) -> Vec<ModelRequest> {
        self.chat_requests.lock().await.clone()
    }

    pub async fn embed_requests(&self) -> Vec<Vec<String>> {
        self.embed_requests.lock().await.clone()
    }

    async fn scripted<T>(&self, keep: fn(&Script) -> Option<T>) -> Option<T> {
        let mut script = self.script.lock().await;
        let index = script.iter().position(|s| keep(s).is_some())?;
        let entry = script.get(index).cloned()?;
        let chosen = keep(&entry);
        script.remove(index);
        drop(script);
        chosen
    }
}

fn fnv1a32(bytes: &[u8]) -> u32 {
    let mut hash = 0x811c_9dc5;
    for byte in bytes {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

fn fnv_token_vector(text: &str, dims: usize) -> Embedding {
    let mut vector = vec![0.0_f32; dims];
    for token in text
        .to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
    {
        let index = fnv1a32(token.as_bytes()) as usize % dims;
        #[expect(clippy::indexing_slicing)]
        {
            vector[index] += 1.0;
        }
    }
    Embedding { vector }
}

fn embed_http(status: u16) -> ModelProviderError {
    ModelProviderError::Request {
        message: format!("http {status}"),
        source: Box::new(std::io::Error::other(format!(
            "embed endpoint answered http {status}"
        ))),
    }
}

fn take_embed(script: &Script) -> Option<Vec<Embedding>> {
    match script {
        Script::Embed(vectors) => Some(vectors.clone()),
        Script::Chat(_)
        | Script::Stream(_)
        | Script::EmbedHttpStatus(_)
        | Script::EmbedNetwork
        | Script::EmbedHang
        | Script::Models(_) => None,
    }
}

fn take_http_status(script: &Script) -> Option<u16> {
    match script {
        Script::EmbedHttpStatus(status) => Some(*status),
        Script::Chat(_)
        | Script::Stream(_)
        | Script::Embed(_)
        | Script::EmbedNetwork
        | Script::EmbedHang
        | Script::Models(_) => None,
    }
}

fn take_network(script: &Script) -> Option<()> {
    match script {
        Script::EmbedNetwork => Some(()),
        Script::Chat(_)
        | Script::Stream(_)
        | Script::Embed(_)
        | Script::EmbedHttpStatus(_)
        | Script::EmbedHang
        | Script::Models(_) => None,
    }
}

fn take_hang(script: &Script) -> Option<()> {
    match script {
        Script::EmbedHang => Some(()),
        Script::Chat(_)
        | Script::Stream(_)
        | Script::Embed(_)
        | Script::EmbedHttpStatus(_)
        | Script::EmbedNetwork
        | Script::Models(_) => None,
    }
}

#[derive(Debug, thiserror::Error)]
#[error("fake has no scripted {0} response")]
struct MissingScript(&'static str);

fn missing_script(operation: &'static str) -> ModelProviderError {
    ModelProviderError::Request {
        message: format!("no scripted {operation} response"),
        source: Box::new(MissingScript(operation)),
    }
}

#[async_trait]
impl ModelProvider for FakeProvider {
    async fn chat(&self, request: ModelRequest) -> Result<LlmResponse, ModelProviderError> {
        self.chat_requests.lock().await.push(request);
        self.scripted(|s| match s {
            Script::Chat(response) => Some(response.clone()),
            Script::Stream(_)
            | Script::Embed(_)
            | Script::EmbedHttpStatus(_)
            | Script::EmbedNetwork
            | Script::EmbedHang
            | Script::Models(_) => None,
        })
        .await
        .ok_or_else(|| missing_script("chat"))
    }

    async fn stream(
        &self,
        request: ModelRequest,
    ) -> Result<Receiver<ModelStreamItem>, ModelProviderError> {
        let chunks = self
            .scripted(|s| match s {
                Script::Stream(chunks) => Some(chunks.clone()),
                Script::Chat(_)
                | Script::Embed(_)
                | Script::EmbedHttpStatus(_)
                | Script::EmbedNetwork
                | Script::EmbedHang
                | Script::Models(_) => None,
            })
            .await
            .ok_or_else(|| missing_script("stream"))?;
        self.chat_requests.lock().await.push(request);
        let (tx, rx) = tokio::sync::mpsc::channel(chunks.len().max(1));
        for chunk in chunks {
            tx.try_send(Ok(chunk))
                .map_err(|source| ModelProviderError::Stream {
                    message: "scripted stream delivery failed".to_owned(),
                    source: Box::new(source),
                })?;
        }
        Ok(rx)
    }

    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Embedding>, ModelProviderError> {
        self.embed_requests.lock().await.push(texts.clone());
        let sticky = *self.http_status.lock().await;
        if let Some(status) = sticky {
            return Err(embed_http(status));
        }
        if let Some(chosen) = self.scripted(take_embed).await {
            return Ok(chosen);
        }
        if let Some(status) = self.scripted(take_http_status).await {
            return Err(embed_http(status));
        }
        if self.scripted(take_network).await.is_some() {
            return Err(ModelProviderError::Request {
                message: "connection refused".to_owned(),
                source: Box::new(std::io::Error::other("connection refused")),
            });
        }
        if self.scripted(take_hang).await.is_some() {
            std::future::pending::<()>().await;
        }
        let dims = self
            .fnv_dims
            .lock()
            .await
            .ok_or_else(|| missing_script("embedding"))?;
        if dims == 0 {
            return Err(missing_script("nonzero embedding width"));
        }
        Ok(texts
            .into_iter()
            .map(|text| fnv_token_vector(&text, dims))
            .collect())
    }

    async fn list_models(&self) -> Result<Vec<String>, ModelProviderError> {
        self.scripted(|s| match s {
            Script::Models(models) => Some(models.clone()),
            Script::Chat(_)
            | Script::Stream(_)
            | Script::Embed(_)
            | Script::EmbedHttpStatus(_)
            | Script::EmbedNetwork
            | Script::EmbedHang => None,
        })
        .await
        .ok_or_else(|| missing_script("models"))
    }
}
