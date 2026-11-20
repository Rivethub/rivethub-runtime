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
use kernel::model::ModelRequest;
use kernel::queue::Job;
use kernel::queue::JobQueue;
use serde::Deserialize;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use wire::message::Role;

use self::script::Scripted;

struct MemStore {
    openings: Mutex<Vec<OpeningTitle>>,
}

fn cancel() -> CancellationToken {
    CancellationToken::new()
}

fn assert_expected_request(request: &ModelRequest, expected: &wire::JsonObject, step: &str) {
    let expected: wire::stream::openai::ChatCompletionRequest =
        expected.deserialize().expect("reference request");
    let system = expected.messages.first().expect("reference system message");
    let user = expected.messages.get(1).expect("reference user message");
    assert_eq!(expected.messages.len(), 2);
    assert_eq!(
        script::system_text(request).ok(),
        system.content.as_deref(),
        "{step} system"
    );
    assert_eq!(
        script::user_text(request).ok(),
        user.content.as_deref(),
        "{step} user"
    );
    assert!(
        request
            .messages
            .iter()
            .map(|message| message.role)
            .eq([system.role, user.role])
    );
    assert_eq!(request.model, expected.model);
    assert_eq!(request.max_tokens.map(u64::from), expected.max_tokens);
    assert_eq!(request.options.temperature, expected.temperature);
    assert_eq!(request.options.enable_thinking, expected.enable_thinking);
}

mod prompt_parity;
mod script;
mod support;
#[path = "summarizer/opening.rs"]
mod top_down_opening;
