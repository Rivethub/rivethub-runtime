use jiff::Timestamp;
use std::sync::Arc;

use async_trait::async_trait;
use kernel::memory::ConversationHead;
use kernel::memory::ConversationWindow;
use kernel::memory::NewSummary;
use kernel::memory::PendingConversation;
use kernel::memory::SummaryStore;
use kernel::memory::SummaryStoreError;
use kernel::memory::SummaryWrite;
use kernel::memory::summary_opening::OpeningTitle;
use kernel::model::Embedding;
use kernel::model::FinishReason;
use kernel::model::LlmResponse;
use kernel::model::LlmResponseKind;
use kernel::model::ModelProvider;
use kernel::model::ModelProviderError;
use kernel::model::ModelRequest;
use kernel::model::ModelStreamItem;
use kernel::queue::Job;
use kernel::queue::JobQueue;
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use wire::message::MessageContent;
use wire::message::Role;
use workers::TITLE_SYSTEM_PROMPT;

const TITLE_ANSWER: &str = r#"{"title": "Plan blue-green AcmeKit deploys"}"#;

#[derive(Debug)]
struct Captured {
    kind: String,
    user: String,
}

#[derive(Default)]
struct Scripted {
    calls: Mutex<Vec<Captured>>,
    live_output: bool,
    fail_part_once: std::sync::atomic::AtomicBool,
    empty: std::sync::atomic::AtomicBool,
    truncated: std::sync::atomic::AtomicBool,
    fail_embeddings: std::sync::atomic::AtomicBool,
    large_parts: std::sync::atomic::AtomicBool,
    part_calls: std::sync::atomic::AtomicUsize,
    concurrent: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
    part_pause: Option<Arc<tokio::sync::Barrier>>,
}

struct MemStore {
    openings: Mutex<Vec<OpeningTitle>>,
}

fn cancel() -> CancellationToken {
    CancellationToken::new()
}

mod live;
mod prompt_parity;
mod support;
#[path = "summarizer/opening.rs"]
mod top_down_opening;
