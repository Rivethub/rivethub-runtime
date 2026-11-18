use async_trait::async_trait;
use kernel::model::{
    Embedding, FinishReason, LlmResponse, LlmResponseKind, ModelProvider, ModelProviderError,
    ModelRequest, ModelStreamItem,
};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use tokio::sync::{Mutex, mpsc::Receiver};
use wire::message::MessageContent;
use workers::TITLE_SYSTEM_PROMPT;

const TITLE_ANSWER: &str = r#"{"title": "Plan blue-green AcmeKit deploys"}"#;

#[derive(Debug)]
pub(super) struct Captured {
    pub(super) kind: String,
    pub(super) user: String,
}

#[derive(Default)]
pub(super) struct Scripted {
    pub(super) calls: Mutex<Vec<Captured>>,
    pub(super) call_arrived: tokio::sync::Notify,
    pub(super) live_output: bool,
    pub(super) fail_part_once: std::sync::atomic::AtomicBool,
    pub(super) empty: std::sync::atomic::AtomicBool,
    pub(super) truncated: std::sync::atomic::AtomicBool,
    pub(super) fail_embeddings: std::sync::atomic::AtomicBool,
    pub(super) large_parts: std::sync::atomic::AtomicBool,
    pub(super) part_calls: std::sync::atomic::AtomicUsize,
    pub(super) concurrent: std::sync::atomic::AtomicUsize,
    pub(super) peak: std::sync::atomic::AtomicUsize,
    pub(super) part_pause: Option<Arc<tokio::sync::Barrier>>,
}

struct Active<'a>(&'a std::sync::atomic::AtomicUsize);

impl Drop for Active<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl ModelProvider for Scripted {
    async fn chat(&self, request: ModelRequest) -> Result<LlmResponse, ModelProviderError> {
        let kind = kind_name(system_text(&request)?);
        let user = user_text(&request)?;
        self.calls.lock().await.push(Captured {
            kind: kind.to_owned(),
            user: user.to_owned(),
        });
        self.call_arrived.notify_one();
        let active = self.concurrent.fetch_add(1, Ordering::SeqCst) + 1;
        let _active = Active(&self.concurrent);
        self.peak.fetch_max(active, Ordering::SeqCst);
        if kind == "TOP_DOWN_PART" {
            let position = self.part_calls.fetch_add(1, Ordering::SeqCst);
            if let Some(barrier) = self.part_pause.as_ref()
                && position < 3
            {
                barrier.wait().await;
            } else {
                tokio::task::yield_now().await;
            }
            if user.starts_with(
                "# Transcript of part 2
",
            ) && self.fail_part_once.swap(false, Ordering::SeqCst)
            {
                return Err(plain_error("scripted part failure"));
            }
        }
        if self.empty.load(Ordering::SeqCst) {
            return Ok(response(String::new(), Some(FinishReason::Stop)));
        }
        let text = match kind {
            "TITLE_SYSTEM_PROMPT" => TITLE_ANSWER.to_owned(),
            "TOP_DOWN_PART" if self.large_parts.load(Ordering::SeqCst) => "parsnip ".repeat(4400),
            "TOP_DOWN_PART" => "Résultat du jardin".to_owned(),
            _ if self.live_output => "A conserved ledger.

## Decisions [m00001-m00001]
The reconciliation uses saffron markers."
                .to_owned(),
            _ => "Réduit.".to_owned(),
        };
        let finish = if self.truncated.load(Ordering::SeqCst) {
            FinishReason::Length
        } else {
            FinishReason::Stop
        };
        let mut response = response(text, Some(finish));
        if kind == "TOP_DOWN_PART" && self.large_parts.load(Ordering::SeqCst) {
            response.usage = Some(wire::stream::LlmUsage {
                prompt_tokens: 11_000,
                completion_tokens: 12_000,
                reasoning_tokens: None,
                cached_tokens: None,
                cache_creation_tokens: None,
                cache_read_tokens: None,
            });
        }
        Ok(response)
    }

    async fn stream(
        &self,
        _: ModelRequest,
    ) -> Result<Receiver<ModelStreamItem>, ModelProviderError> {
        Err(plain_error("the summarizer script has no stream"))
    }
    async fn embed(&self, texts: Vec<String>) -> Result<Vec<Embedding>, ModelProviderError> {
        if self.fail_embeddings.load(Ordering::SeqCst) {
            return Err(plain_error("scripted embedding failure"));
        }
        Ok(texts
            .into_iter()
            .map(|_| Embedding {
                vector: vec![0.25; 1024],
            })
            .collect())
    }
    async fn list_models(&self) -> Result<Vec<String>, ModelProviderError> {
        Ok(Vec::new())
    }
}

pub(super) fn system_text(request: &ModelRequest) -> Result<&str, ModelProviderError> {
    let message = request
        .messages
        .first()
        .ok_or_else(|| plain_error("missing system message"))?;
    match &message.content {
        MessageContent::Text(text) => Ok(text),
        MessageContent::Parts(_) => Err(plain_error("system content is not text")),
    }
}

pub(super) fn user_text(request: &ModelRequest) -> Result<&str, ModelProviderError> {
    let message = request
        .messages
        .get(1)
        .ok_or_else(|| plain_error("missing user message"))?;
    match &message.content {
        MessageContent::Text(text) => Ok(text),
        MessageContent::Parts(_) => Err(plain_error("user content is not text")),
    }
}

fn kind_name(system: &str) -> &'static str {
    if system == TITLE_SYSTEM_PROMPT {
        "TITLE_SYSTEM_PROMPT"
    } else if system.starts_with("You write the memory record of one working session")
        && system.contains("You are given the transcript of part ")
    {
        "TOP_DOWN_PART"
    } else if system.starts_with("You write the memory record of one working session") {
        "TOP_DOWN_SESSION"
    } else if system.starts_with("You are given the records of the first ") {
        "TOP_DOWN_CARRY"
    } else if system.starts_with("You are given the records of all ") {
        "TOP_DOWN_FINAL"
    } else {
        "OTHER"
    }
}

fn response(content: String, finish: Option<FinishReason>) -> LlmResponse {
    LlmResponse {
        r#type: LlmResponseKind::Text,
        content: if content.is_empty() {
            None
        } else {
            Some(content)
        },
        tool_calls: None,
        usage: None,
        finish_reason: finish,
    }
}

fn plain_error(message: &str) -> ModelProviderError {
    ModelProviderError::Request {
        message: message.to_owned(),
        source: Box::new(std::io::Error::other(message.to_owned())),
    }
}
