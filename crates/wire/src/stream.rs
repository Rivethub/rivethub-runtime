pub use values::{LlmUsage, ThinkingLevel};

pub mod openai {
    use crate::JsonObject;
    use crate::message::Role;
    use serde::{Deserialize, Serialize};

    #[serde_with::skip_serializing_none]
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct FunctionDelta {
        pub name: Option<String>,
        pub arguments: Option<String>,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    #[serde(tag = "type", rename_all = "camelCase")]
    pub enum MessageToolCall {
        #[serde(rename = "function")]
        Function(StreamedPlainToolCall),
    }

    #[serde_with::skip_serializing_none]
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    #[schemars(rename = "OpenAiMessage")]
    pub struct Message {
        pub role: Role,
        #[serialize_always]
        pub content: Option<String>,
        pub reasoning_content: Option<String>,
        pub tool_calls: Option<Vec<MessageToolCall>>,
        pub tool_call_id: Option<String>,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct StreamOptions {
        pub include_usage: bool,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct ChatTemplateKwargs {
        pub enable_thinking: bool,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct Reasoning {
        pub enabled: bool,
    }

    #[serde_with::skip_serializing_none]
    #[derive(
        Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default,
    )]
    #[serde(default)]
    pub struct StructuredOutputs {
        pub json: Option<crate::JsonObject>,
        pub disable_any_whitespace: Option<bool>,
    }

    #[serde_with::skip_serializing_none]
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct ChatCompletionRequest {
        pub temperature: Option<f64>,
        pub enable_thinking: Option<bool>,
        pub model: String,
        pub max_tokens: Option<u64>,
        pub messages: Vec<Message>,
        pub stream: Option<bool>,
        pub stream_options: Option<StreamOptions>,
        pub chat_template_kwargs: Option<ChatTemplateKwargs>,
        pub think: Option<bool>,

        #[serde(default)]
        pub reasoning: Option<Reasoning>,
        pub response_format: Option<ResponseFormat>,
        #[serde(default)]
        pub structured_outputs: Option<StructuredOutputs>,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct ResponseFormat {
        #[serde(rename = "type")]
        pub kind: String,
        pub json_schema: JsonSchemaFormat,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct JsonSchemaFormat {
        pub name: String,
        pub schema: crate::JsonObject,
        pub strict: bool,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    #[serde(rename_all = "snake_case")]
    pub enum FinishReason {
        Stop,
        Length,
        ToolCalls,
        ContentFilter,
    }

    #[serde_with::skip_serializing_none]
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    #[serde(rename_all = "camelCase")]
    pub struct StreamedPlainToolCall {
        pub index: Option<u32>,
        pub id: Option<String>,
        pub function: FunctionDelta,
    }

    #[serde_with::skip_serializing_none]
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct Delta {
        pub role: Option<Role>,
        pub content: Option<String>,
        pub reasoning_content: Option<String>,
    }

    #[serde_with::skip_serializing_none]
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct Choice {
        pub index: Option<u64>,
        pub delta: Delta,
        pub logprobs: Option<JsonObject>,
        pub finish_reason: Option<FinishReason>,
        pub stop_reason: Option<String>,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct PromptTokensDetails {
        pub cached_tokens: u64,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct CompletionTokensDetails {
        pub reasoning_tokens: u64,
    }

    #[serde_with::skip_serializing_none]
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct Usage {
        pub prompt_tokens: u64,
        pub completion_tokens: u64,
        pub total_tokens: u64,
        pub prompt_tokens_details: Option<PromptTokensDetails>,
        pub completion_tokens_details: Option<CompletionTokensDetails>,
    }

    #[serde_with::skip_serializing_none]
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct Timings {
        pub cache_n: u64,
        pub prompt_n: u64,
        pub prompt_ms: f64,
        pub prompt_per_token_ms: f64,
        pub prompt_per_second: f64,
        pub predicted_n: u64,
        pub predicted_ms: f64,
        pub predicted_per_token_ms: f64,
        pub predicted_per_second: f64,
    }

    #[serde_with::skip_serializing_none]
    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct ChatCompletionChunk {
        pub id: String,
        pub object: String,
        pub created: i64,
        pub model: String,
        pub system_fingerprint: Option<String>,
        pub choices: Vec<Choice>,
        pub usage: Option<Usage>,
        pub timings: Option<Timings>,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct ModelList {
        pub object: String,
        pub data: Vec<Model>,
    }

    #[serde_with::skip_serializing_none]
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct Model {
        pub id: String,
        pub object: String,
        pub created: i64,
        pub owned_by: String,
        pub root: Option<String>,
        pub parent: Option<String>,
        pub max_model_len: Option<u64>,
        pub meta: Option<JsonObject>,
        pub permission: Option<Vec<JsonObject>>,
    }

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct EmbeddingRequest {
        pub model: String,
        pub input: Vec<String>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct EmbeddingResponse {
        pub data: Vec<EmbeddingRow>,
    }

    #[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
    pub struct EmbeddingRow {
        pub embedding: Vec<f32>,
        pub index: usize,
    }
}
