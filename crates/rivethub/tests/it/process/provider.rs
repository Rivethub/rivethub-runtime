use wire::stream::openai::{ChatCompletionChunk, Choice, Delta, FinishReason};

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
