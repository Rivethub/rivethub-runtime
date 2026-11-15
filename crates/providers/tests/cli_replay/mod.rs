mod capabilities;
mod lifecycle;
use std::collections::BTreeMap;
use std::sync::Arc;

use config::provider::CliProvider;
use kernel::harness::{SessionEvent, ToolCallInfo};
use kernel::model::{ModelChunk, ModelProvider};
use serde::Deserialize;
use tokio_util::sync::CancellationToken;
use wire::stream::LlmChunk;
use wire::stream::LlmChunkKind;

use crate::cli_support::{Harness, Plan, TestResult, items, provider, request};

const SCENARIOS: &[&str] = &[
    "A-text",
    "B-tool",
    "C-reasoning",
    "D-thinking-off",
    "E-reasoning-tool",
];

#[derive(Debug)]
enum Expected {
    Chunk(ModelChunk),
    Arguments {
        index: u32,
        arguments: wire::JsonObject,
    },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BasicChunk {
    Text { text: String },
    Done,
}

#[tokio::test]
async fn basic_provider_recording_replays_through_harness() -> TestResult {
    let chunks: Vec<BasicChunk> = serde_json::from_reader(std::fs::File::open(
        testkit::recording("provider/A-text/chunks.json"),
    )?)?;
    let mut events = Vec::new();
    let mut expected = Vec::new();
    for chunk in chunks {
        match chunk {
            BasicChunk::Text { text } => {
                events.push(SessionEvent::AgentMessageChunk { text: text.clone() });
                expected.push(ModelChunk::Text { delta: text });
            }
            BasicChunk::Done => expected.push(ModelChunk::Done),
        }
    }
    let (harness, actions) = Harness::new(vec![Plan {
        events,
        finish: true,
    }]);
    let provider = provider(
        Arc::new(harness),
        CliProvider::default(),
        &CancellationToken::new(),
    );
    let actual = items(provider.stream(request()).await?)
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(actual, expected);
    provider.shutdown().await;
    drop(actions);
    Ok(())
}

#[tokio::test]
async fn shared_recordings_replay_text_reasoning_and_complete_tool_arguments() -> TestResult {
    for scenario in SCENARIOS {
        let chunks: Vec<LlmChunk> = serde_json::from_reader(std::fs::File::open(
            testkit::recording(&format!("providers/vllm/{scenario}/chunks.json")),
        )?)?;
        let (events, expected) = replay(chunks)?;
        let (harness, actions) = Harness::new(vec![Plan {
            events,
            finish: true,
        }]);
        let provider = provider(
            Arc::new(harness),
            CliProvider::default(),
            &CancellationToken::new(),
        );
        let actual = items(provider.stream(request()).await?)
            .await
            .into_iter()
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(actual.len(), expected.len(), "{scenario}");
        for (actual, expected) in actual.into_iter().zip(expected) {
            match expected {
                Expected::Chunk(expected) => assert_eq!(actual, expected, "{scenario}"),
                Expected::Arguments { index, arguments } => {
                    let ModelChunk::ToolCallDelta {
                        index: actual_index,
                        arguments_delta,
                    } = actual
                    else {
                        panic!("expected tool arguments for {scenario}");
                    };
                    assert_eq!(actual_index, index);
                    assert_eq!(
                        serde_json::from_str::<wire::JsonObject>(&arguments_delta)?,
                        arguments
                    );
                }
            }
        }
        provider.shutdown().await;
        drop(actions);
    }
    Ok(())
}

fn replay(
    chunks: Vec<LlmChunk>,
) -> Result<(Vec<SessionEvent>, Vec<Expected>), Box<dyn std::error::Error + Send + Sync>> {
    let mut events = Vec::new();
    let mut expected = Vec::new();
    let mut calls = BTreeMap::new();
    for chunk in chunks {
        match chunk.r#type {
            LlmChunkKind::Text => {
                let delta = chunk.delta.expect("text delta");
                events.push(SessionEvent::AgentMessageChunk {
                    text: delta.clone(),
                });
                expected.push(Expected::Chunk(ModelChunk::Text { delta }));
            }
            LlmChunkKind::Reasoning => {
                let delta = chunk.delta.expect("reasoning delta");
                events.push(SessionEvent::AgentThoughtChunk {
                    text: delta.clone(),
                });
                expected.push(Expected::Chunk(ModelChunk::Reasoning { delta }));
            }
            LlmChunkKind::ToolCallStart => {
                let call = chunk.tool_call.expect("call start");
                calls.insert(
                    call.index.expect("index"),
                    (
                        call.id.expect("id"),
                        call.name.expect("name"),
                        String::new(),
                    ),
                );
            }
            LlmChunkKind::ToolCallDelta => {
                let index = chunk.tool_call.expect("delta call").index.expect("index");
                calls
                    .get_mut(&index)
                    .expect("started call")
                    .2
                    .push_str(&chunk.delta.expect("arguments delta"));
            }
            LlmChunkKind::ToolCallDone => {
                let index = chunk.tool_call.expect("done call").index.expect("index");
                let (id, name, arguments) = calls.remove(&index).expect("started call");
                let input: wire::JsonObject = serde_json::from_str(&arguments)?;
                events.push(SessionEvent::ToolCall(ToolCallInfo {
                    tool_call_id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                }));
                expected.push(Expected::Chunk(ModelChunk::ToolCallStart {
                    index,
                    id,
                    name,
                }));
                expected.push(Expected::Arguments {
                    index,
                    arguments: input,
                });
            }
            other @ (LlmChunkKind::Status | LlmChunkKind::Done | LlmChunkKind::Error) => {
                panic!("unexpected fixture chunk {other:?}")
            }
        }
    }
    assert!(calls.is_empty());
    expected.push(Expected::Chunk(ModelChunk::Done));
    Ok((events, expected))
}
