use std::sync::Arc;
use std::sync::atomic::AtomicU8;
use std::sync::atomic::Ordering;

use eventsource_stream::EventStreamError;
use eventsource_stream::Eventsource;
use futures::StreamExt;
use kernel::model::FinishReason;
use kernel::model::ModelChunk;
use kernel::model::ModelProviderError;
use kernel::model::ModelStreamItem;
use wire::stream::openai::ChatCompletionChunk;
use wire::stream::openai::FinishReason as ChunkFinish;
use wire::stream::openai::StreamedToolCall;

use crate::error::StreamFault;

pub const STREAM_CAPACITY: usize = 64;

const FINISH_STOP: u8 = 1;
const FINISH_LENGTH: u8 = 2;
const FINISH_TOOL_CALLS: u8 = 3;
const FINISH_CONTENT_FILTER: u8 = 4;

pub async fn pump(
    response: reqwest::Response,
    tx: tokio::sync::mpsc::Sender<ModelStreamItem>,
    finish: Option<Arc<AtomicU8>>,
) {
    let mut events = response.bytes_stream().eventsource();
    let mut complete = false;
    loop {
        tokio::select! {
            biased;
            () = tx.closed() => return,
            next = events.next() => {
                match next {
                    None => break,
                    Some(Ok(event)) => {
                        if event.data.trim() == "[DONE]" {
                            complete = true;
                            break;
                        }
                        if !forward(&tx, &event.data, finish.as_deref()).await {
                            return;
                        }
                    }
                    Some(Err(source)) => {
                        send_failure(&tx, stream_event_error(source)).await;
                        return;
                    }
                }
            }
        }
    }
    if complete {
        #[expect(
            clippy::let_underscore_must_use,
            reason = "the receiver is already gone"
        )]
        #[expect(
            clippy::let_underscore_untyped,
            reason = "the receiver is already gone"
        )]
        let _ = tx.send(Ok(ModelChunk::Done)).await;
    }
}

async fn forward(
    tx: &tokio::sync::mpsc::Sender<ModelStreamItem>,
    data: &str,
    finish: Option<&AtomicU8>,
) -> bool {
    let chunk: ChatCompletionChunk = match serde_json::from_str(data) {
        Ok(chunk) => chunk,
        Err(source) => {
            send_failure(tx, undecodable(data, source)).await;
            return false;
        }
    };
    let mut translated = Vec::new();
    if let Err(fault) = translate(&chunk, &mut translated, finish) {
        send_failure(tx, fault.into()).await;
        return false;
    }
    for chunk in translated {
        if tx.send(Ok(chunk)).await.is_err() {
            return false;
        }
    }
    true
}

fn undecodable(data: &str, source: serde_json::Error) -> ModelProviderError {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            untyped,
            reason = "tool arguments and model JSON are arbitrary objects"
        )
    )]
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a stream error that is not JSON is reported as the decode error"
        )
    )]
    let reported = serde_json::from_str::<serde_json::Value>(data)
        .ok()
        .and_then(|value| value.get("error").cloned());
    if let Some(error) = reported {
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                untyped,
                reason = "tool arguments and model JSON are arbitrary objects"
            )
        )]
        let message = error
            .get("message")
            .and_then(serde_json::Value::as_str)
            .map_or_else(|| error.to_string(), str::to_owned);
        let code = error
            .get("code")
            .filter(|code| !code.is_null())
            .map(|code| format!(" (code {code})"))
            .unwrap_or_default();
        return stream_error(
            format!("the model server reported an error mid-stream: {message}{code}"),
            source,
        );
    }
    let quoted: String = data.chars().take(300).collect();
    stream_error(
        format!("the model server sent a malformed stream event: {source}; event: {quoted}"),
        source,
    )
}

async fn send_failure(tx: &tokio::sync::mpsc::Sender<ModelStreamItem>, error: ModelProviderError) {
    #[expect(
        clippy::let_underscore_must_use,
        reason = "the receiver is already gone"
    )]
    #[expect(
        clippy::let_underscore_untyped,
        reason = "the receiver is already gone"
    )]
    let _ = tx.send(Err(error)).await;
}

fn stream_event_error(source: EventStreamError<reqwest::Error>) -> ModelProviderError {
    let malformed = matches!(
        &source,
        EventStreamError::Parser(_) | EventStreamError::Utf8(_)
    );
    let message = if malformed {
        format!("the model server sent a malformed stream event: {source}")
    } else {
        format!("the model server stream failed: {source}")
    };
    if malformed {
        stream_error(message, source)
    } else {
        stream_error(
            message,
            kernel::model::ModelCallFailure::Transport {
                source: Box::new(source),
            },
        )
    }
}

fn translate(
    chunk: &ChatCompletionChunk,
    out: &mut Vec<ModelChunk>,
    finish: Option<&AtomicU8>,
) -> Result<(), StreamFault> {
    for choice in chunk.choices.iter().take(1) {
        if let Some(reason) = choice.finish_reason {
            note_finish(finish, reason);
        }
        if let Some(text) = non_empty(choice.delta.content.as_deref()) {
            out.push(ModelChunk::Text {
                delta: text.to_owned(),
            });
        }
        if let Some(reasoning) = non_empty(choice.delta.reasoning_content.as_deref()) {
            out.push(ModelChunk::Reasoning {
                delta: reasoning.to_owned(),
            });
        }
        for call in choice.delta.tool_calls.iter().flatten() {
            let call = match call {
                StreamedToolCall::Function(tagged) => match tagged {
                    wire::stream::openai::FunctionToolCallTag::Function(plain) => plain,
                },
                StreamedToolCall::Plain(plain) => plain,
            };
            let index = call.index.ok_or(StreamFault::ToolCallWithoutIndex)?;
            if let Some(name) = non_empty(call.function.name.as_deref()) {
                let id = call
                    .id
                    .as_deref()
                    .filter(|id| !id.is_empty())
                    .ok_or(StreamFault::ToolCallWithoutId)?;
                out.push(ModelChunk::ToolCallStart {
                    index,
                    id: id.to_owned(),
                    name: name.to_owned(),
                });
            }
            if let Some(arguments) = non_empty(call.function.arguments.as_deref()) {
                out.push(ModelChunk::ToolCallDelta {
                    index,
                    arguments_delta: arguments.to_owned(),
                });
            }
        }
    }
    if let Some(usage) = &chunk.usage {
        out.push(ModelChunk::Usage {
            usage: wire::stream::LlmUsage {
                prompt_tokens: usage.prompt_tokens,
                completion_tokens: usage.completion_tokens,
                reasoning_tokens: usage
                    .completion_tokens_details
                    .as_ref()
                    .map(|d| d.reasoning_tokens),
                cached_tokens: usage
                    .prompt_tokens_details
                    .as_ref()
                    .map(|d| d.cached_tokens),
                cache_creation_tokens: None,
                cache_read_tokens: None,
            },
        });
    }
    Ok(())
}

fn note_finish(slot: Option<&AtomicU8>, reason: ChunkFinish) {
    let Some(slot) = slot else {
        return;
    };
    slot.store(encode_finish(reason), Ordering::Relaxed);
}

fn encode_finish(reason: ChunkFinish) -> u8 {
    match reason {
        ChunkFinish::Stop => FINISH_STOP,
        ChunkFinish::Length => FINISH_LENGTH,
        ChunkFinish::ToolCalls => FINISH_TOOL_CALLS,
        ChunkFinish::ContentFilter => FINISH_CONTENT_FILTER,
    }
}

pub(crate) fn decode_finish(code: u8) -> Option<FinishReason> {
    match code {
        FINISH_STOP => Some(FinishReason::Stop),
        FINISH_LENGTH => Some(FinishReason::Length),
        FINISH_TOOL_CALLS => Some(FinishReason::ToolCalls),
        FINISH_CONTENT_FILTER => Some(FinishReason::ContentFilter),
        _ => None,
    }
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    let value = value?;
    if value.is_empty() {
        return None;
    }
    Some(value)
}

pub(crate) fn stream_error(
    message: impl Into<String>,
    source: impl Into<kernel::BoxError>,
) -> ModelProviderError {
    ModelProviderError::Stream {
        message: message.into(),
        source: source.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU8;
    use std::sync::atomic::Ordering;

    use super::decode_finish;
    use super::translate;
    use super::undecodable;
    use kernel::model::FinishReason;
    use kernel::model::ModelChunk;
    use wire::stream::openai::ChatCompletionChunk;

    #[test]
    fn a_length_finish_is_recorded_for_chat() {
        let raw = r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"delta":{"content":"hi"},"finish_reason":"length"}]}"#;
        let chunk: ChatCompletionChunk = serde_json::from_str(raw).expect("chunk");
        let slot = AtomicU8::new(0);
        let mut out = Vec::new();
        translate(&chunk, &mut out, Some(&slot)).expect("translate");
        assert_eq!(
            decode_finish(slot.load(Ordering::Relaxed)),
            Some(FinishReason::Length)
        );
        assert!(matches!(out.as_slice(), [ModelChunk::Text { delta }] if delta == "hi"));
    }

    #[test]
    fn a_stream_without_a_slot_drops_the_finish_reason() {
        let raw = r#"{"id":"c","object":"chat.completion.chunk","created":1,"model":"m","choices":[{"delta":{},"finish_reason":"stop"}]}"#;
        let chunk: ChatCompletionChunk = serde_json::from_str(raw).expect("chunk");
        let mut out = Vec::new();
        translate(&chunk, &mut out, None).expect("translate");
        assert_eq!(out, Vec::<ModelChunk>::new());
    }

    fn failure_text(data: &str) -> String {
        let source = serde_json::from_str::<ChatCompletionChunk>(data).expect_err("not a chunk");
        undecodable(data, source).to_string()
    }

    #[test]
    fn a_mid_stream_error_object_reports_the_server_message() {
        let text = failure_text(
            r#"{"error":{"object":"error","message":"engine dead: KV transfer aborted","type":"InternalServerError","param":null,"code":500}}"#,
        );
        assert!(text.contains("reported an error mid-stream"), "{text}");
        assert!(text.contains("engine dead: KV transfer aborted"), "{text}");
        assert!(text.contains("(code 500)"), "{text}");
    }

    #[test]
    fn an_unknown_event_is_quoted_in_the_failure() {
        let text = failure_text(r#"{"weird":true}"#);
        assert!(text.contains("malformed stream event"), "{text}");
        assert!(text.contains(r#"{"weird":true}"#), "{text}");
    }
}
