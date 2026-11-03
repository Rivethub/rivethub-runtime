#![cfg(test)]

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use kernel::memory::jobs::{
    ChunkWrite, MemoryJobError, MemoryJobStore, SynthesizeToolCall, ToolCallMessage,
    UnchunkedMessage,
};
use kernel::memory::{EmbedTarget, StoredChunk};
use kernel::model::{
    Embedding, ModelCallFailure, ModelProvider, ModelProviderError, ModelRequest, ModelStreamItem,
};
use kernel::model::{LlmResponse, LlmResponseKind};
use tokio::sync::{Mutex, mpsc};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use wire::JsonObject;
use workers::tool_synthesis::handle_synthesize_tool_call;

struct Store(Mutex<ToolCallMessage>);

impl Store {
    fn new() -> Self {
        Self(Mutex::new(ToolCallMessage {
            id: Uuid::nil(),
            tool_name: Some("read".to_owned()),
            tool_args: JsonObject::default(),
            content: String::new(),
        }))
    }
}

#[async_trait]
impl MemoryJobStore for Store {
    async fn tool_call(&self, id: Uuid) -> Result<Option<ToolCallMessage>, MemoryJobError> {
        let row = self.0.lock().await;
        Ok((row.id == id).then(|| row.clone()))
    }
    async fn write_tool_sentence(
        &self,
        id: Uuid,
        previous_content: &str,
        content: &str,
    ) -> Result<(), MemoryJobError> {
        let mut row = self.0.lock().await;
        if row.id == id && row.content == previous_content {
            content.clone_into(&mut row.content);
        }
        drop(row);
        Ok(())
    }
    async fn unchunked(
        &self,
        _limit: u32,
        _chars_per_chunk: u32,
        _tool_result_cap: u32,
    ) -> Result<Vec<UnchunkedMessage>, MemoryJobError> {
        Ok(Vec::new())
    }
    async fn write_chunks(
        &self,
        _message_id: Uuid,
        _chunks: Vec<StoredChunk>,
    ) -> Result<ChunkWrite, MemoryJobError> {
        Ok(ChunkWrite::Skipped)
    }
    async fn unembedded_chunks(
        &self,
        _message_id: Uuid,
    ) -> Result<Vec<EmbedTarget>, MemoryJobError> {
        Ok(Vec::new())
    }
    async fn revive_dead_wiki(&self, _limit: u32) -> Result<u64, MemoryJobError> {
        Ok(0)
    }
    async fn missing_wiki(
        &self,
        _limit: u32,
    ) -> Result<Vec<kernel::memory::jobs::MissingWiki>, MemoryJobError> {
        Ok(Vec::new())
    }
}

struct Provider {
    calls: AtomicUsize,
    failures: usize,
    status: Option<u16>,
    hang: bool,
    inner: testkit::FakeProvider,
}

impl Provider {
    fn new(failures: usize, status: Option<u16>, hang: bool) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            failures,
            status,
            hang,
            inner: testkit::FakeProvider::new(vec![testkit::Script::Chat(LlmResponse {
                r#type: LlmResponseKind::Text,
                content: Some("Read the service configuration.".to_owned()),
                tool_calls: None,
                usage: None,
                finish_reason: None,
            })]),
        }
    }
}

#[async_trait]
impl ModelProvider for Provider {
    async fn chat(&self, request: ModelRequest) -> Result<LlmResponse, ModelProviderError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.hang {
            return std::future::pending().await;
        }
        if call < self.failures {
            let source = Box::new(std::io::Error::other("fixture failure"));
            let failure = match self.status {
                Some(status) => ModelCallFailure::Http {
                    status,
                    retry_after: None,
                    source,
                },
                None => ModelCallFailure::Transport { source },
            };
            return Err(ModelProviderError::Request {
                message: failure.to_string(),
                source: Box::new(failure),
            });
        }
        self.inner.chat(request).await
    }
    async fn stream(
        &self,
        request: ModelRequest,
    ) -> Result<mpsc::Receiver<ModelStreamItem>, ModelProviderError> {
        self.inner.stream(request).await
    }
    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Embedding>, ModelProviderError> {
        self.inner.embed(texts).await
    }
    async fn list_models(&self) -> Result<Vec<String>, ModelProviderError> {
        self.inner.list_models().await
    }
}

fn payload() -> JsonObject {
    JsonObject::from_serialize(&SynthesizeToolCall {
        message_id: Uuid::nil(),
    })
    .expect("payload")
}

#[tokio::test(start_paused = true)]
async fn transport_and_server_failures_retry_three_times_with_bounded_backoff() {
    for status in [None, Some(500), Some(503)] {
        let provider = Provider::new(3, status, false);
        let start = tokio::time::Instant::now();
        handle_synthesize_tool_call(
            &Store::new(),
            &provider,
            "model",
            &payload(),
            &CancellationToken::new(),
        )
        .await
        .expect("fourth attempt succeeds");
        assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
        assert_eq!(start.elapsed(), Duration::from_secs(12));
    }
}

#[tokio::test(start_paused = true)]
async fn other_http_failures_and_exhausted_retries_fail_the_job() {
    for (status, attempts) in [(Some(400), 1), (Some(429), 1), (Some(500), 4), (None, 4)] {
        let provider = Provider::new(4, status, false);
        let error = handle_synthesize_tool_call(
            &Store::new(),
            &provider,
            "model",
            &payload(),
            &CancellationToken::new(),
        )
        .await
        .expect_err("failed");
        assert_eq!(error.to_string(), "Empty synth response");
        assert_eq!(provider.calls.load(Ordering::SeqCst), attempts);
    }
}

#[tokio::test(start_paused = true)]
async fn timeout_and_cancellation_bound_the_entire_model_call() {
    let provider = Provider::new(0, None, true);
    let start = tokio::time::Instant::now();
    let error = handle_synthesize_tool_call(
        &Store::new(),
        &provider,
        "model",
        &payload(),
        &CancellationToken::new(),
    )
    .await
    .expect_err("timed out");
    assert_eq!(error.to_string(), "Empty synth response");
    assert_eq!(start.elapsed(), Duration::from_secs(120));
    let cancel = CancellationToken::new();
    cancel.cancel();
    let error = handle_synthesize_tool_call(&Store::new(), &provider, "model", &payload(), &cancel)
        .await
        .expect_err("cancelled");
    assert!(matches!(error, workers::WorkerError::Cancelled));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_sentence_ending_in_a_quoted_message_is_written() {
    let quoted = "Sent a message with the content \"Sending it.\"";
    let provider = testkit::FakeProvider::new(vec![testkit::Script::Chat(LlmResponse {
        r#type: LlmResponseKind::Text,
        content: Some(quoted.to_owned()),
        tool_calls: None,
        usage: None,
        finish_reason: None,
    })]);
    let store = Store::new();
    handle_synthesize_tool_call(
        &store,
        &provider,
        "model",
        &payload(),
        &CancellationToken::new(),
    )
    .await
    .expect("synthesized");
    assert_eq!(store.0.lock().await.content, quoted);
}
