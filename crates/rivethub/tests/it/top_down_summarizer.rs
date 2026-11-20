use async_trait::async_trait;
use jiff::Timestamp;
use kernel::memory::SummaryStore;
use kernel::model::{
    Embedding, LlmResponse, ModelProvider, ModelProviderError, ModelRequest, ModelStreamItem,
};
use kernel::queue::JobQueue;
use serde::Deserialize;
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

fn assert_expected_request(request: &ModelRequest, expected: &wire::JsonObject, step: &str) {
    let actual = providers::body::chat_body(providers::Variant::Vllm, request, None)
        .ok()
        .and_then(|body| wire::JsonObject::from_serialize(&body).ok());
    assert_eq!(actual.as_ref(), Some(expected), "{step} wire request");
}

mod live;
#[cfg(test)]
#[path = "../../../workers/tests/it/summarizer/prompt_parity.rs"]
mod prompt_parity;
