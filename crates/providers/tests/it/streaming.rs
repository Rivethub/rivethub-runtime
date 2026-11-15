use crate::common;
use crate::common::fixed_status_server;
use crate::common::repeating_server;
use crate::common::scenario_server;
use crate::common::slow_server;
use kernel::model::ModelChunk;
use kernel::model::ModelProvider;
use kernel::model::ModelProviderError;
use kernel::model::ModelRequest;
use providers::OpenAiCompatible;
use std::sync::Arc;
use std::time::Duration;
use wire::message::Message;
use wire::message::MessageContent;
use wire::message::Role;
use wire::stream::LlmUsage;
const A_TEXT_SSE: &str = concat!(
    "data: {\"id\":\"chatcmpl-golden-vllm-A-1\",\"object\":\"chat.completion.chunk\",\"created\":1791590400,\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"\"},\"logprobs\":null,\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-golden-vllm-A-1\",\"object\":\"chat.completion.chunk\",\"created\":1791590400,\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"Hi\"},\"logprobs\":null,\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-golden-vllm-A-1\",\"object\":\"chat.completion.chunk\",\"created\":1791590400,\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\" there!\"},\"logprobs\":null,\"finish_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-golden-vllm-A-1\",\"object\":\"chat.completion.chunk\",\"created\":1791590400,\"model\":\"test-model\",\"choices\":[{\"index\":0,\"delta\":{},\"logprobs\":null,\"finish_reason\":\"stop\",\"stop_reason\":null}]}\n\n",
    "data: {\"id\":\"chatcmpl-golden-vllm-A-1\",\"object\":\"chat.completion.chunk\",\"created\":1791590400,\"model\":\"test-model\",\"choices\":[],\"usage\":{\"prompt_tokens\":25,\"total_tokens\":30,\"completion_tokens\":5,\"prompt_tokens_details\":{\"cached_tokens\":10}}}\n\n",
    "data: [DONE]\n\n",
);

fn request() -> ModelRequest {
    ModelRequest {
        options: kernel::model::ModelOptions::default(),
        model: "test-model".to_owned(),
        messages: vec![
            Message {
                role: Role::System,
                content: MessageContent::Text("You are a test.".to_owned()),
                reasoning: None,
                tool_calls: None,
                tool_call_id: None,
            },
            Message {
                role: Role::User,
                content: MessageContent::Text("Say hi.".to_owned()),
                reasoning: None,
                tool_calls: None,
                tool_call_id: None,
            },
        ],
        thinking: None,
        max_tokens: None,
    }
}

fn chat_provider(addr: std::net::SocketAddr) -> OpenAiCompatible {
    common::chat_provider(addr, None)
}

#[expect(clippy::expect_used)]
async fn collect(
    mut rx: tokio::sync::mpsc::Receiver<kernel::model::ModelStreamItem>,
) -> (String, Option<LlmUsage>, bool) {
    let mut text = String::new();
    let mut usage = None;
    let mut done = false;
    while let Some(item) = rx.recv().await {
        match item.expect("the stream item is a chunk") {
            ModelChunk::Text { delta } => text.push_str(&delta),
            ModelChunk::Usage { usage: received } => usage = Some(received),
            ModelChunk::Done => done = true,
            ModelChunk::Reasoning { .. } => {}
        }
    }
    (text, usage, done)
}

#[tokio::test]
async fn a_text_stream_parses_into_chunks_usage_and_done() {
    let server = repeating_server(A_TEXT_SSE.to_owned()).await;
    let provider = chat_provider(server.addr);
    let rx = provider.stream(request()).await.expect("the stream starts");
    let (text, usage, done) = collect(rx).await;
    assert_eq!(text, "Hi there!");
    assert_eq!(
        usage,
        Some(LlmUsage {
            prompt_tokens: 25,
            completion_tokens: 5,
            reasoning_tokens: None,
            cached_tokens: Some(10),
            cache_creation_tokens: None,
            cache_read_tokens: None,
        })
    );
    assert!(done);
}

#[tokio::test]
async fn chat_assembles_the_llm_response() {
    let server = repeating_server(A_TEXT_SSE.to_owned()).await;
    let provider = chat_provider(server.addr);
    let response = provider.chat(request()).await.expect("the chat completes");
    assert_eq!(response.r#type, kernel::model::LlmResponseKind::Text);
    assert_eq!(response.content.as_deref(), Some("Hi there!"));
    assert_eq!(response.usage.map(|usage| usage.prompt_tokens), Some(25));
    assert!(response.tool_calls.is_none());
}

#[tokio::test]
async fn media_message_parts_are_rejected_before_the_wire() {
    let server = repeating_server(A_TEXT_SSE.to_owned()).await;
    let provider = chat_provider(server.addr);
    let mut request = request();
    request.messages.push(Message {
        role: Role::User,
        content: MessageContent::Parts(vec![wire::message::ContentPart::Image(
            wire::message::MediaFields {
                url: Some("https://example.com/a.png".to_owned()),
                ..wire::message::MediaFields::default()
            },
        )]),
        reasoning: None,
        tool_calls: None,
        tool_call_id: None,
    });
    let error = provider
        .stream(request)
        .await
        .expect_err("media parts fail the request");
    match error {
        ModelProviderError::Request { message, .. } => {
            assert!(
                message.contains("content parts are not spoken"),
                "the rejection is named: {message}"
            );
        }
        ModelProviderError::Stream { .. } => panic!("an input rejection is a request error"),
    }
}

#[tokio::test]
async fn probe_models_false_skips_the_models_get() {
    let server = repeating_server(A_TEXT_SSE.to_owned()).await;
    let provider = config::provider::OpenaiProvider {
        model: Some("test-model".to_owned()),
        base_url: Some(format!("http://{}", server.addr)),
        probe_models: Some(false),
        ..config::provider::OpenaiProvider::default()
    };
    let built = OpenAiCompatible::build(config::provider::ProviderType::Vllm, &provider)
        .expect("the provider builds");
    let models = built.list_models().await.expect("no probe is made");
    assert_eq!(models, vec!["test-model".to_owned()]);
    assert!(server.recorder.lock().is_empty());
}

#[tokio::test]
async fn a_stream_that_closes_without_done_is_not_a_completed_response() {
    let unfinished = A_TEXT_SSE.replace(
        concat!(
            "data: {\"id\":\"chatcmpl-golden-vllm-A-1\",\"object\":\"chat.completion.chunk\",",
            "\"created\":1791590400,\"model\":\"test-model\",\"choices\":[],\"usage\":",
            "{\"prompt_tokens\":25,\"total_tokens\":30,\"completion_tokens\":5,",
            "\"prompt_tokens_details\":{\"cached_tokens\":10}}}\n\ndata: [DONE]\n\n",
        ),
        "",
    );
    let server = scenario_server(vec![
        testkit::Canned {
            status: 200,
            body: unfinished.clone(),
        },
        testkit::Canned {
            status: 200,
            body: unfinished,
        },
    ])
    .await;
    let provider = chat_provider(server.addr);
    let rx = provider.stream(request()).await.expect("the stream starts");
    let (text, usage, done) = collect(rx).await;
    assert_eq!(text, "Hi there!");
    assert_eq!(usage, None);
    assert!(
        !done,
        "a clean close without the done marker is not completion"
    );
    let error = provider
        .chat(request())
        .await
        .expect_err("chat reports the short stream");
    match error {
        ModelProviderError::Stream { message, .. } => {
            assert!(
                message.contains("ended before Done"),
                "the failure is named: {message}"
            );
        }
        ModelProviderError::Request { .. } => panic!("a mid-stream failure is a stream error"),
    }
}

#[tokio::test]
async fn dropping_the_stream_receiver_aborts_the_request() {
    let one_event = concat!(
        "data: {\"id\":\"chatcmpl-golden-vllm-A-1\",\"object\":\"chat.completion.chunk\",",
        "\"created\":1791590400,\"model\":\"test-model\",\"choices\":[{\"index\":0,",
        "\"delta\":{\"content\":\"Hi\"},\"logprobs\":null,\"finish_reason\":null}]}\n\n",
    );
    let server = slow_server(one_event.to_owned()).await;
    let provider = chat_provider(server.addr);
    let mut rx = provider.stream(request()).await.expect("the stream starts");
    let first = rx
        .recv()
        .await
        .expect("the first chunk arrives")
        .expect("the first item is a chunk");
    assert!(matches!(first, ModelChunk::Text { .. }));
    drop(rx);
    tokio::time::timeout(Duration::from_secs(5), server.disconnect.wait())
        .await
        .expect("the server observes the client disconnect");
}

#[tokio::test]
async fn a_non_2xx_status_is_a_typed_request_error() {
    let server = fixed_status_server(
        500,
        "{\"error\":{\"message\":\"model not loaded\"}}".to_owned(),
    )
    .await;
    let provider = chat_provider(server.addr);
    let error = provider
        .stream(request())
        .await
        .expect_err("the non-2xx status fails the stream");
    match error {
        ModelProviderError::Request { message, .. } => {
            assert!(message.contains("500"), "the status is named: {message}");
            assert!(
                message.contains("model not loaded"),
                "the server body is carried: {message}"
            );
        }
        ModelProviderError::Stream { .. } => {
            panic!("a status failure is a request error, not a stream error")
        }
    }
}

#[tokio::test]
async fn a_malformed_event_is_a_typed_stream_error() {
    let server = fixed_status_server(200, "data: {\"oops\": \n\ndata: [DONE]\n\n".to_owned()).await;
    let provider = chat_provider(server.addr);
    let mut rx = provider.stream(request()).await.expect("the stream starts");
    let item = rx
        .recv()
        .await
        .expect("the stream reports the broken event");
    match &item {
        Err(ModelProviderError::Stream { message, .. }) => {
            assert!(
                message.contains("malformed"),
                "the failure is named: {message}"
            );
        }
        Err(ModelProviderError::Request { .. }) | Ok(_) => {
            panic!("a malformed event is a stream error, got {item:?}")
        }
    }
    assert!(rx.recv().await.is_none(), "the error is terminal");
    let error = provider
        .chat(request())
        .await
        .expect_err("chat reports the broken stream");
    match error {
        ModelProviderError::Stream { message, .. } => {
            assert!(
                message.contains("malformed"),
                "the failure is named: {message}"
            );
            assert!(
                !message.contains("ended before Done"),
                "a broken event is not a clean short body: {message}"
            );
        }
        ModelProviderError::Request { .. } => panic!("a mid-stream failure is a stream error"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sixty_four_concurrent_streams_complete_on_one_provider() {
    let server = repeating_server(A_TEXT_SSE.to_owned()).await;
    let provider = Arc::new(chat_provider(server.addr));
    let mut handles = Vec::new();
    for _ in 0..64 {
        let provider = provider.clone();
        handles.push(tokio::spawn(async move {
            let rx = provider.stream(request()).await.expect("the stream starts");
            collect(rx).await
        }));
    }
    for handle in handles {
        let (text, usage, done) = handle.await.expect("the task joins");
        assert_eq!(text, "Hi there!");
        assert_eq!(usage.map(|usage| usage.prompt_tokens), Some(25));
        assert!(done);
    }
}
