use crate::common;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::response::IntoResponse as _;
use kernel::model::{ModelChunk, ModelProvider, ModelProviderError};
use tokio::sync::Notify;

const TEXT: &str = "data: {\"id\":\"c\",\"object\":\"chat.completion.chunk\",\"created\":1,\"model\":\"m\",\"choices\":[{\"delta\":{\"content\":\"hello\"}}]}\n\n";

fn request() -> kernel::model::ModelRequest {
    kernel::model::ModelRequest {
        options: kernel::model::ModelOptions::default(),
        model: "m".to_owned(),
        messages: Vec::new(),
        thinking: None,
        max_tokens: None,
    }
}

#[tokio::test]
async fn transport_failure_preserves_the_source_and_terminates_the_stream() {
    let fail = Arc::new(Notify::new());
    let routes = axum::Router::new()
        .fallback(broken_body)
        .with_state(Arc::clone(&fail));
    let server = testkit::stub_server(routes).await.expect("server");
    let provider = common::chat_provider(server.addr, None);
    let mut stream = provider.stream(request()).await.expect("stream");
    let first = tokio::time::timeout(Duration::from_secs(5), stream.recv())
        .await
        .expect("text arrives")
        .expect("text item")
        .expect("text");
    assert_eq!(
        first,
        ModelChunk::Text {
            delta: "hello".to_owned()
        }
    );
    fail.notify_one();
    let error = tokio::time::timeout(Duration::from_secs(5), stream.recv())
        .await
        .expect("error arrives")
        .expect("error item")
        .expect_err("broken body");
    assert!(error.retryable_transport());
    let ModelProviderError::Stream { source, message } = error else {
        panic!("body transport is a stream error");
    };
    let Some(kernel::model::ModelCallFailure::Transport { source }) =
        source.downcast_ref::<kernel::model::ModelCallFailure>()
    else {
        panic!("transport classification retains its source");
    };
    assert!(matches!(
        source.downcast_ref::<eventsource_stream::EventStreamError<reqwest::Error>>(),
        Some(eventsource_stream::EventStreamError::Transport(_))
    ));
    assert!(message.starts_with("the model server stream failed:"));
    assert!(
        tokio::time::timeout(Duration::from_secs(5), stream.recv())
            .await
            .expect("stream closes")
            .is_none()
    );
}

async fn broken_body(
    axum::extract::State(fail): axum::extract::State<Arc<Notify>>,
) -> axum::response::Response {
    let stream = futures::stream::unfold((false, fail), |(sent, fail)| async move {
        if !sent {
            return Some((Ok(Bytes::from_static(TEXT.as_bytes())), (true, fail)));
        }
        fail.notified().await;
        Some((
            Err(std::io::Error::other("transport interrupted")),
            (true, fail),
        ))
    });
    (
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        Body::from_stream(stream),
    )
        .into_response()
}

#[tokio::test]
async fn token_spawn_failures_share_the_original_os_error() {
    let directory = tempfile::tempdir().expect("isolated missing command");
    let missing = directory.path().join("missing-command");
    let config = config::provider::OpenaiProvider {
        model: Some("model".to_owned()),
        base_url: Some("http://127.0.0.1:1".to_owned()),
        token_command: Some(vec![missing.to_str().expect("test path").to_owned()]),
        ..config::provider::OpenaiProvider::default()
    };
    let provider =
        providers::OpenAiCompatible::build(config::provider::ProviderType::Vllm, &config)
            .expect("provider");
    let error = provider.list_models().await.expect_err("spawn failure");
    let ModelProviderError::Request { source, .. } = error else {
        panic!("token failure is a request error");
    };
    let Some(kernel::model::ModelCallFailure::Credentials { source }) =
        source.downcast_ref::<kernel::model::ModelCallFailure>()
    else {
        panic!("token failure is a credentials failure");
    };
    let error = source
        .downcast_ref::<providers::error::TokenError>()
        .expect("typed token error");
    let providers::error::TokenError::Spawn { source } = error else {
        panic!("missing executable is a spawn failure");
    };
    assert_eq!(source.kind(), std::io::ErrorKind::NotFound);
    assert!(source.raw_os_error().is_some());
    let providers::error::TokenError::Spawn { source: shared } = error.clone() else {
        panic!("cloned spawn error");
    };
    assert!(Arc::ptr_eq(source, &shared));
}
