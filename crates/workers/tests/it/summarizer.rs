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

mod prompt_parity;
mod script;
mod support;
#[path = "summarizer/opening.rs"]
mod top_down_opening;
