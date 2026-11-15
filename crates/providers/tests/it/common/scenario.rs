#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum RecordedChunk {
    #[serde(rename = "text")]
    Text { delta: String },
    #[serde(rename = "reasoning")]
    Reasoning { delta: String },
    #[serde(rename = "tool_call_start")]
    ToolCallStart {
        #[serde(rename = "toolCall")]
        tool_call: RecordedStart,
    },
    #[serde(rename = "tool_call_delta")]
    ToolCallDelta {
        delta: String,
        #[serde(rename = "toolCall")]
        tool_call: RecordedIndex,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordedStart {
    index: u32,
    id: String,
    name: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordedIndex {
    index: u32,
}

fn recorded_chunk(chunk: &ModelChunk) -> Json {
    let shaped = match chunk {
        ModelChunk::Text { delta } => RecordedChunk::Text {
            delta: delta.clone(),
        },
        ModelChunk::Reasoning { delta } => RecordedChunk::Reasoning {
            delta: delta.clone(),
        },
        ModelChunk::ToolCallStart { index, id, name } => RecordedChunk::ToolCallStart {
            tool_call: RecordedStart {
                index: *index,
                id: id.clone(),
                name: name.clone(),
            },
        },
        ModelChunk::ToolCallDelta {
            index,
            arguments_delta,
        } => RecordedChunk::ToolCallDelta {
            delta: arguments_delta.clone(),
            tool_call: RecordedIndex { index: *index },
        },
        ModelChunk::Usage { .. } | ModelChunk::Done => {
            halt("usage and done chunks are filtered before the comparison")
        }
    };
    demand(
        serde_json::to_value(&shaped),
        "the recorded chunk shape serializes",
    )
}

pub(crate) fn model_request(scenario: &str, body: &Json) -> ModelRequest {
    ModelRequest {
        options: kernel::model::ModelOptions::default(),
        model: demand_some(body.get("model").and_then(Json::as_str), "recorded model").to_owned(),
        messages: decode_messages(body.get("messages")),
        tools: decode_tools(body.get("tools")),
        thinking: Some(thinking(scenario)),
        max_tokens: None,
    }
}

fn thinking(scenario: &str) -> ThinkingLevel {
    if scenario == "D-thinking-off" {
        return ThinkingLevel::Off;
    }
    ThinkingLevel::Medium
}

fn decode_messages(value: Option<&Json>) -> Vec<Message> {
    let Some(Json::Array(rows)) = value else {
        return Vec::new();
    };
    rows.iter()
        .map(|row| Message {
            role: demand(
                serde_json::from_value(demand_some(row.get("role").cloned(), "recorded role")),
                "recorded role parses",
            ),
            content: decode_content(row.get("content")),
            reasoning: row
                .get("reasoning_content")
                .and_then(Json::as_str)
                .map(str::to_owned),
            tool_calls: row.get("tool_calls").map(|calls| {
                demand(
                    serde_json::from_value::<Vec<RecordedToolCall>>(calls.clone()),
                    "recorded tool calls parse",
                )
                .into_iter()
                .map(RecordedToolCall::into_kernel)
                .collect()
            }),
            tool_call_id: row
                .get("tool_call_id")
                .and_then(Json::as_str)
                .map(str::to_owned),
        })
        .collect()
}

fn decode_content(value: Option<&Json>) -> MessageContent {
    match value {
        Some(Json::String(text)) => MessageContent::Text(text.clone()),
        None | Some(Json::Null) => MessageContent::Text(String::new()),
        Some(parts) => MessageContent::Parts(demand(
            serde_json::from_value::<Vec<ContentPart>>(parts.clone()),
            "recorded content parts parse",
        )),
    }
}

#[derive(Deserialize)]
struct RecordedToolCall {
    id: String,
    function: RecordedFunction,
}

#[derive(Deserialize)]
struct RecordedFunction {
    name: String,
    arguments: String,
}

impl RecordedToolCall {
    fn into_kernel(self) -> ToolCall {
        ToolCall {
            id: self.id,
            name: self.function.name,
            arguments: demand(
                serde_json::from_str(&self.function.arguments),
                "recorded arguments parse",
            ),
            thought_signature: None,
        }
    }
}

fn decode_tools(value: Option<&Json>) -> Vec<rmcp::model::Tool> {
    let Some(Json::Array(rows)) = value else {
        return Vec::new();
    };
    rows.iter()
        .map(|row| {
            let function = demand_some(row.get("function"), "recorded tool function");
            let parameters: wire::JsonObject = demand(
                serde_json::from_value(demand_some(
                    function.get("parameters").cloned(),
                    "recorded tool parameters",
                )),
                "recorded tool parameters parse",
            );
            rmcp::model::Tool::new(
                demand_some(
                    function.get("name").and_then(Json::as_str),
                    "recorded tool name",
                )
                .to_owned(),
                demand_some(
                    function.get("description").and_then(Json::as_str),
                    "recorded tool description",
                )
                .to_owned(),
                parameters.as_map().clone(),
            )
        })
        .collect()
}

fn expected_usage(scenario: &str) -> Vec<LlmUsage> {
    let built =
        |prompt: u64, completion: u64, reasoning: Option<u64>, cached: Option<u64>| LlmUsage {
            prompt_tokens: prompt,
            completion_tokens: completion,
            reasoning_tokens: reasoning,
            cached_tokens: cached,
            cache_creation_tokens: None,
            cache_read_tokens: None,
        };
    match scenario {
        "A-text" | "D-thinking-off" => vec![built(25, 5, None, Some(10))],
        "C-reasoning" => vec![built(25, 30, Some(22), None)],
        "B-tool" => vec![built(60, 18, None, None), built(90, 9, None, None)],
        "E-reasoning-tool" => vec![built(60, 40, Some(20), None), built(110, 9, None, None)],
        other => halt(&format!("no recorded usage table for {other}")),
    }
}

pub(crate) async fn drive(scenario: &'static str) {
    let recorded = scenario_record(scenario);
    let server = scenario_server(recorded.canned.clone()).await;
    let provider = chat_provider(server.addr, None);
    let bodies: Vec<Json> = recorded
        .requests
        .iter()
        .map(|request| demand(serde_json::from_str(&request.body), "stripped body parses"))
        .collect();
    let mut chunks: Vec<Json> = Vec::new();
    let mut usage: Vec<LlmUsage> = Vec::new();
    for body in &bodies {
        let mut rx = demand(
            provider.stream(model_request(scenario, body)).await,
            "the stream starts",
        );
        let mut done = false;
        while let Some(item) = rx.recv().await {
            match demand(item, "the stream item is a chunk") {
                ModelChunk::Done => done = true,
                ModelChunk::Usage { usage: received } => usage.push(received),
                other @ (ModelChunk::Text { .. }
                | ModelChunk::Reasoning { .. }
                | ModelChunk::ToolCallStart { .. }
                | ModelChunk::ToolCallDelta { .. }) => chunks.push(recorded_chunk(&other)),
            }
        }
        assert!(done, "scenario {scenario}: the stream ended without Done");
    }
    assert_eq!(usage, expected_usage(scenario), "scenario {scenario} usage");
    let turn = ProviderTurn {
        requests: captured_requests(&server.recorder),
        chunks,
    };
    drop(server);
    replay(&recorded, &turn);
}

pub(crate) async fn model_request_to_stream(
    provider: &OpenAiCompatible,
    scenario: &str,
    body: &Json,
) -> Result<tokio::sync::mpsc::Receiver<ModelStreamItem>, kernel::model::ModelProviderError> {
    provider.stream(model_request(scenario, body)).await
}
