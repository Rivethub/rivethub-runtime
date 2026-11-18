use async_trait::async_trait;
use jiff::Timestamp;
use kernel::memory::SummaryStore;
use kernel::model::{
    Embedding, LlmResponse, ModelProvider, ModelProviderError, ModelRequest, ModelStreamItem,
};
use kernel::queue::JobQueue;
use std::sync::Arc;
use tokio::sync::mpsc::Receiver;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use wire::message::Role;

#[path = "../../../workers/tests/it/summarizer/script.rs"]
mod script;

use self::script::Scripted;

impl Scripted {
    fn live() -> Self {
        Self {
            live_output: true,
            ..Self::default()
        }
    }
}

mod live;
