use kernel::model::ModelProviderError;
use wire::message::ContentPart;
use wire::message::Message;
use wire::message::MessageContent;
use wire::message::Role;
use wire::stream::openai::ChatCompletionRequest;
use wire::stream::openai::ChatTemplateKwargs;
use wire::stream::openai::FunctionDef;
use wire::stream::openai::JsonSchemaFormat;
use wire::stream::openai::MessageToolCall;
use wire::stream::openai::ResponseFormat;
use wire::stream::openai::StreamOptions;
use wire::stream::openai::StructuredOutputs;
use wire::stream::openai::Tool;

use crate::Variant;
use crate::error::Unsupported;

#[must_use]
pub fn normalize_api_prefix(prefix: Option<&str>) -> String {
    let Some(raw) = prefix else {
        return "/v1".to_owned();
    };
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with('/') {
        return trimmed.to_owned();
    }
    format!("/{trimmed}")
}

fn endpoint_url(base_url: &str, prefix: &str, path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let base = base.strip_suffix("/v1").unwrap_or(base);
    format!("{base}{prefix}/{path}")
}

#[must_use]
pub fn chat_url(base_url: &str, prefix: &str) -> String {
    endpoint_url(base_url, prefix, "chat/completions")
}

#[must_use]
pub fn models_url(base_url: &str, prefix: &str) -> String {
    endpoint_url(base_url, prefix, "models")
}

#[must_use]
pub fn embeddings_url(base_url: &str, prefix: &str) -> String {
    endpoint_url(base_url, prefix, "embeddings")
}

fn message_text(message: &Message) -> String {
    match &message.content {
        MessageContent::Text(text) => text.clone(),
        MessageContent::Parts(parts) => parts
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                ContentPart::Image(_) | ContentPart::Video(_) => None,
            })
            .collect(),
    }
}

fn system_notice(text: String) -> Message {
    Message {
        role: Role::User,
        content: MessageContent::Text(text),
        reasoning: None,
        tool_calls: None,
        tool_call_id: None,
    }
}

fn split_and_fold_system(messages: &[Message]) -> (String, Vec<Message>) {
    let mut system = String::new();
    let mut rest = Vec::with_capacity(messages.len());
    let mut leading = true;
    for message in messages {
        if message.role == Role::System {
            let body = message_text(message);
            if leading {
                if !system.is_empty() {
                    system.push_str("\n\n");
                }
                system.push_str(&body);
            } else if body.is_empty() {
                rest.push(system_notice(
                    "[SYSTEM NOTICE]\n(empty system message)".to_owned(),
                ));
            } else {
                rest.push(system_notice(format!("[SYSTEM NOTICE]\n{body}")));
            }
            continue;
        }
        leading = false;
        rest.push(message.clone());
    }
    (system, rest)
}

pub fn chat_body(
    variant: Variant,
    request: &kernel::model::ModelRequest,
    configured_max_tokens: Option<u32>,
) -> Result<ChatCompletionRequest, ModelProviderError> {
    let (system, messages) = split_and_fold_system(&request.messages);
    let mut wire_messages = Vec::with_capacity(messages.len() + usize::from(!system.is_empty()));
    if !system.is_empty() {
        wire_messages.push(wire::stream::openai::Message {
            role: Role::System,
            content: Some(system),
            reasoning_content: None,
            tool_calls: None,
            tool_call_id: None,
        });
    }
    for message in messages {
        wire_messages.push(wire_message(&message)?);
    }
    let tools = tools(&request.tools)?;
    let response_format = request
        .options
        .json_schema
        .as_ref()
        .map(response_format)
        .transpose()?;
    let structured_outputs = response_format.as_ref().and_then(|format| {
        request
            .options
            .disable_any_whitespace
            .then(|| StructuredOutputs {
                json: Some(format.json_schema.schema.clone()),
                disable_any_whitespace: Some(true),
            })
    });
    Ok(ChatCompletionRequest {
        model: request.model.clone(),
        max_tokens: request.max_tokens.or(configured_max_tokens).map(u64::from),
        temperature: request.options.temperature,
        enable_thinking: request.options.enable_thinking,
        messages: wire_messages,
        tools,
        tool_choice: None,
        stream: Some(true),
        stream_options: Some(StreamOptions {
            include_usage: true,
        }),
        chat_template_kwargs: request
            .options
            .enable_thinking
            .map(|enable_thinking| ChatTemplateKwargs { enable_thinking })
            .or(match (variant, request.thinking) {
                (Variant::Vllm, Some(wire::stream::ThinkingLevel::Off)) => {
                    Some(ChatTemplateKwargs {
                        enable_thinking: false,
                    })
                }
                _ => None,
            }),
        think: None,
        reasoning: None,
        response_format,
        structured_outputs,
    })
}

fn response_format(schema: &impl serde::Serialize) -> Result<ResponseFormat, ModelProviderError> {
    let schema =
        wire::JsonObject::from_serialize(schema).map_err(|source| ModelProviderError::Request {
            message: "the response schema does not serialize".to_owned(),
            source: Box::new(source),
        })?;
    Ok(ResponseFormat {
        kind: "json_schema".to_owned(),
        json_schema: JsonSchemaFormat {
            name: "response".to_owned(),
            schema,
            strict: true,
        },
    })
}

fn wire_message(message: &Message) -> Result<wire::stream::openai::Message, ModelProviderError> {
    let content = match &message.content {
        MessageContent::Text(text) if text.is_empty() => None,
        MessageContent::Text(text) => Some(text.clone()),
        MessageContent::Parts(_) => {
            return Err(Unsupported::ContentParts.into());
        }
    };
    let tool_calls = message
        .tool_calls
        .as_ref()
        .map(|calls| calls.iter().map(wire_tool_call).collect())
        .transpose()?;
    Ok(wire::stream::openai::Message {
        role: message.role,
        content,
        reasoning_content: message.reasoning.clone(),
        tool_calls,
        tool_call_id: message.tool_call_id.clone(),
    })
}

fn wire_tool_call(call: &wire::message::ToolCall) -> Result<MessageToolCall, ModelProviderError> {
    let arguments =
        serde_json::to_string(&call.arguments).map_err(|source| ModelProviderError::Request {
            message: format!("tool call arguments for \"{}\" do not serialize", call.name),
            source: Box::new(source),
        })?;
    Ok(MessageToolCall::Function(
        wire::stream::openai::StreamedPlainToolCall {
            index: None,
            id: Some(call.id.clone()),
            function: wire::stream::openai::FunctionDelta {
                name: Some(call.name.clone()),
                arguments: Some(arguments),
            },
        },
    ))
}

fn tools(tools: &[rmcp::model::Tool]) -> Result<Option<Vec<Tool>>, ModelProviderError> {
    if tools.is_empty() {
        return Ok(None);
    }
    let mut converted = Vec::with_capacity(tools.len());
    for tool in tools {
        let schema = serde_json::to_value(tool.input_schema.as_ref()).map_err(|source| {
            ModelProviderError::Request {
                message: format!("tool \"{}\" has an unreadable input schema", tool.name),
                source: Box::new(source),
            }
        })?;
        let parameters: wire::JsonObject =
            serde_json::from_value(schema).map_err(|source| ModelProviderError::Request {
                message: format!("tool \"{}\" has an unreadable input schema", tool.name),
                source: Box::new(source),
            })?;
        converted.push(Tool::Function {
            function: FunctionDef {
                name: tool.name.to_string(),
                description: tool.description.as_deref().map(str::to_owned),
                parameters,
            },
        });
    }
    Ok(Some(converted))
}

#[cfg(test)]
mod url_tests {
    use super::{chat_url, embeddings_url, models_url, normalize_api_prefix};

    #[test]
    fn a_trailing_v1_on_the_base_is_stripped_and_re_appended_like_the_den() {
        let prefix = normalize_api_prefix(None);
        for base in [
            "https://api.deepseek.com/v1",
            "https://api.deepseek.com/v1/",
            "https://api.deepseek.com",
        ] {
            assert_eq!(
                chat_url(base, &prefix),
                "https://api.deepseek.com/v1/chat/completions"
            );
            assert_eq!(
                models_url(base, &prefix),
                "https://api.deepseek.com/v1/models"
            );
            assert_eq!(
                embeddings_url(base, &prefix),
                "https://api.deepseek.com/v1/embeddings"
            );
        }
        assert_eq!(
            chat_url("http://host:8000/v1", &normalize_api_prefix(Some(""))),
            "http://host:8000/chat/completions"
        );
    }

    #[derive(schemars::JsonSchema)]
    struct Probe {
        #[expect(dead_code, reason = "the field exists so the schema is an object")]
        ok: bool,
    }

    #[test]
    fn thinking_off_and_compact_json_are_sent_only_when_the_caller_asks() {
        let request = kernel::model::ModelRequest {
            options: kernel::model::ModelOptions {
                enable_thinking: Some(false),
                json_schema: Some(schemars::schema_for!(Probe)),
                disable_any_whitespace: true,
                ..kernel::model::ModelOptions::default()
            },
            model: "thinkingcap-flash-next".to_owned(),
            messages: Vec::new(),
            tools: Vec::new(),
            thinking: None,
            max_tokens: Some(4_000),
        };
        let body = super::chat_body(crate::Variant::Vllm, &request, None).expect("body");
        assert_eq!(
            body.chat_template_kwargs,
            Some(wire::stream::openai::ChatTemplateKwargs {
                enable_thinking: false
            })
        );
        let compact = body.structured_outputs.expect("compact json");
        assert_eq!(compact.disable_any_whitespace, Some(true));
        assert!(compact.json.is_some());
        let plain = kernel::model::ModelRequest {
            options: kernel::model::ModelOptions::default(),
            ..request
        };
        let plain = super::chat_body(crate::Variant::OpenaiCompatible, &plain, None).expect("body");
        assert_eq!(plain.chat_template_kwargs, None);
        assert_eq!(plain.structured_outputs, None);
    }
}
