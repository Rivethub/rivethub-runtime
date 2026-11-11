use std::net::SocketAddr;
use std::time::Duration;

use super::TestResult;
use wire::stream::openai::{
    ChatCompletionChunk, ChatCompletionRequest, Choice, Delta, FinishReason,
};

#[derive(Debug)]
pub struct Provider {
    pub address: SocketAddr,
    cancel: tokio_util::sync::CancellationToken,
    task: tokio::task::JoinHandle<std::io::Result<()>>,
}

impl Provider {
    pub async fn start() -> TestResult<Self> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let body = event_stream("hello back", Some(FinishReason::Stop))?;
        let router = axum::Router::new().route(
            "/v1/chat/completions",
            axum::routing::post(
                move |axum::Json(request): axum::Json<ChatCompletionRequest>| {
                    let body = body.clone();
                    async move {
                        assert_eq!(request.model, "test");
                        (
                            [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
                            body,
                        )
                    }
                },
            ),
        );
        let cancel = tokio_util::sync::CancellationToken::new();
        let stopped = cancel.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(stopped.cancelled_owned())
                .await
        });
        Ok(Self {
            address,
            cancel,
            task,
        })
    }

    pub async fn close(self) -> TestResult {
        self.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), self.task).await???;
        Ok(())
    }
}

pub fn event_stream(
    content: &str,
    finish_reason: Option<FinishReason>,
) -> Result<String, serde_json::Error> {
    let chunk = ChatCompletionChunk {
        id: "test-completion".into(),
        object: "chat.completion.chunk".into(),
        created: 0,
        model: "test".into(),
        system_fingerprint: None,
        choices: vec![Choice {
            index: Some(0),
            delta: Delta {
                role: None,
                content: Some(content.into()),
                reasoning_content: None,
                tool_calls: None,
            },
            logprobs: None,
            finish_reason,
            stop_reason: None,
        }],
        usage: None,
        timings: None,
    };
    Ok(format!(
        "data: {}\n\ndata: [DONE]\n\n",
        serde_json::to_string(&chunk)?
    ))
}
