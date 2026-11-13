use super::{ProviderScenario, ProviderTurn, demand};
use wire::stream::openai::{ChatCompletionRequest, Message, MessageToolCall};

pub(crate) fn replay(recorded: &ProviderScenario, actual: &ProviderTurn) {
    assert_eq!(recorded.requests.len(), actual.requests.len());
    for (expected, actual) in recorded.requests.iter().zip(&actual.requests) {
        assert_eq!(expected.method, actual.method);
        assert_eq!(expected.target, actual.target);
        assert_eq!(
            expected.header("authorization"),
            actual.header("authorization")
        );
        if expected.body.is_empty() {
            assert_eq!(actual.body, "");
        } else {
            let expected: ChatCompletionRequest =
                demand(serde_json::from_str(&expected.body), "recorded request");
            let actual: ChatCompletionRequest =
                demand(serde_json::from_str(&actual.body), "actual request");
            assert_eq!(expected.model, actual.model);
            assert_eq!(expected.max_tokens, actual.max_tokens);
            assert_eq!(expected.tools, actual.tools);
            assert_eq!(expected.tool_choice, actual.tool_choice);
            assert_eq!(expected.stream, actual.stream);
            assert_eq!(expected.stream_options, actual.stream_options);
            assert_eq!(expected.chat_template_kwargs, actual.chat_template_kwargs);
            assert_eq!(expected.think, actual.think);
            assert_eq!(expected.structured_outputs, actual.structured_outputs);
            assert_eq!(expected.messages.len(), actual.messages.len());
            for (expected, actual) in expected.messages.iter().zip(&actual.messages) {
                message(expected, actual);
            }
        }
    }
    if let Some(expected) = &recorded.chunks {
        assert_eq!(expected.len(), actual.chunks.len());
        for (expected, actual) in expected.iter().zip(&actual.chunks) {
            assert_eq!(expected, actual);
        }
    }
}

fn message(expected: &Message, actual: &Message) {
    assert_eq!(expected.role, actual.role);
    assert_eq!(expected.content, actual.content);
    assert_eq!(expected.reasoning_content, actual.reasoning_content);
    assert_eq!(expected.tool_call_id, actual.tool_call_id);
    match (&expected.tool_calls, &actual.tool_calls) {
        (Some(expected), Some(actual)) => {
            assert_eq!(expected.len(), actual.len());
            for (expected, actual) in expected.iter().zip(actual) {
                let MessageToolCall::Function(expected) = expected;
                let MessageToolCall::Function(actual) = actual;
                assert_eq!(expected.id, actual.id);
                assert_eq!(expected.index, actual.index);
                assert_eq!(expected.function.name, actual.function.name);
                let expected = expected.function.arguments.as_deref().map(arguments);
                let actual = actual.function.arguments.as_deref().map(arguments);
                assert_eq!(expected, actual);
            }
        }
        (None, None) => {}
        _ => super::halt("tool calls differ"),
    }
}

fn arguments(text: &str) -> wire::JsonObject {
    demand(serde_json::from_str(text), "tool arguments")
}
