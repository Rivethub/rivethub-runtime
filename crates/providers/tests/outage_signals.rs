#[path = "common/mod.rs"]
#[expect(
    dead_code,
    reason = "each test binary uses a subset of the HTTP harness"
)]
mod common;

use std::time::Duration;

use axum::http::StatusCode;
use axum::http::header::RETRY_AFTER;
use axum::response::IntoResponse as _;
use config::provider::OpenaiProvider;
use config::provider::ProviderType;
use kernel::model::ModelCallFailure;
use kernel::model::ModelProvider;
use kernel::model::ModelProviderError;
use providers::OpenAiCompatible;

#[derive(Clone, Copy)]
struct Answer {
    status: StatusCode,
    retry_after: Option<&'static str>,
}

async fn answer(
    axum::extract::State(answer): axum::extract::State<Answer>,
) -> axum::response::Response {
    let mut response = (answer.status, "busy").into_response();
    if let Some(value) = answer.retry_after {
        response
            .headers_mut()
            .insert(RETRY_AFTER, axum::http::HeaderValue::from_static(value));
    }
    response
}

async fn server(answer_with: Answer) -> testkit::Stub {
    let routes = axum::Router::new().fallback(answer).with_state(answer_with);
    common::demand(testkit::stub_server(routes).await, "stub server binds")
}

fn provider(stub: &testkit::Stub, token_command: Option<Vec<String>>) -> OpenAiCompatible {
    let config = OpenaiProvider {
        model: Some("test-model".to_owned()),
        base_url: Some(format!("http://{}", stub.addr)),
        api_key: Some("static-key".to_owned()),
        token_command,
        ..OpenaiProvider::default()
    };
    common::demand(
        OpenAiCompatible::build(ProviderType::Vllm, &config),
        "the provider builds",
    )
}

fn request() -> kernel::model::ModelRequest {
    kernel::model::ModelRequest {
        options: kernel::model::ModelOptions::default(),
        model: "test-model".to_owned(),
        messages: Vec::new(),
        tools: Vec::new(),
        thinking: None,
        max_tokens: None,
    }
}

fn http_failure(error: &ModelProviderError) -> (u16, Option<Duration>) {
    match error.failure() {
        Some(ModelCallFailure::Http {
            status,
            retry_after,
            ..
        }) => (*status, *retry_after),
        other => common::halt(&format!("an HTTP failure, got {other:?} from {error}")),
    }
}

#[tokio::test]
async fn a_throttled_chat_carries_the_status_and_the_retry_after_hint() {
    let stub = server(Answer {
        status: StatusCode::TOO_MANY_REQUESTS,
        retry_after: Some("7"),
    })
    .await;
    let error = provider(&stub, None)
        .chat(request())
        .await
        .expect_err("the server throttles");
    assert_eq!(http_failure(&error), (429, Some(Duration::from_secs(7))));
}

#[tokio::test]
async fn an_embedding_status_is_a_typed_http_failure() {
    let stub = server(Answer {
        status: StatusCode::SERVICE_UNAVAILABLE,
        retry_after: Some("Wed, 21 Oct 2015 07:28:00 GMT"),
    })
    .await;
    let error = provider(&stub, None)
        .embed(vec!["q".to_owned()])
        .await
        .expect_err("the server is down");
    assert_eq!(http_failure(&error), (503, Some(Duration::ZERO)));
    let unauthorized = server(Answer {
        status: StatusCode::UNAUTHORIZED,
        retry_after: None,
    })
    .await;
    let error = provider(&unauthorized, None)
        .embed(vec!["q".to_owned()])
        .await
        .expect_err("the key is refused");
    assert_eq!(http_failure(&error), (401, None));
}

#[tokio::test]
async fn a_server_that_cannot_be_reached_is_an_unreachable_failure() {
    let closed = common::demand(
        std::net::TcpListener::bind("127.0.0.1:0"),
        "a free port binds",
    );
    let addr = common::demand(closed.local_addr(), "the port has an address");
    drop(closed);
    let config = OpenaiProvider {
        model: Some("test-model".to_owned()),
        base_url: Some(format!("http://{addr}")),
        api_key: Some("static-key".to_owned()),
        ..OpenaiProvider::default()
    };
    let built = common::demand(
        OpenAiCompatible::build(ProviderType::Vllm, &config),
        "the provider builds",
    );
    for error in [
        built.embed(vec!["q".to_owned()]).await.expect_err("embed"),
        built.chat(request()).await.expect_err("chat"),
    ] {
        assert!(
            matches!(error.failure(), Some(ModelCallFailure::Unreachable { .. })),
            "{error}"
        );
    }
}

#[tokio::test]
async fn a_failing_token_command_is_a_credentials_failure() {
    let stub = server(Answer {
        status: StatusCode::OK,
        retry_after: None,
    })
    .await;
    let command = vec!["/bin/sh".to_owned(), "-c".to_owned(), "exit 3".to_owned()];
    let built = provider(&stub, Some(command));
    for error in [
        built.embed(vec!["q".to_owned()]).await.expect_err("embed"),
        built.chat(request()).await.expect_err("chat"),
    ] {
        assert!(
            matches!(error.failure(), Some(ModelCallFailure::Credentials { .. })),
            "{error}"
        );
        assert!(
            error.to_string().contains("token_command exited 3"),
            "{error}"
        );
    }
}
