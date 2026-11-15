mod capabilities;
mod lifecycle;
use std::sync::Arc;

use config::provider::CliProvider;
use kernel::harness::SessionEvent;
use kernel::model::{ModelChunk, ModelProvider};
use tokio_util::sync::CancellationToken;

use crate::cli_support::{Harness, Plan, TestResult, items, provider, request};

const SCENARIOS: &[&str] = &["A-text", "C-reasoning", "D-thinking-off"];

#[derive(serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Recorded {
    Text { delta: String },
    Reasoning { delta: String },
}

#[tokio::test]
async fn shared_recordings_replay_text_and_reasoning() -> TestResult {
    for scenario in SCENARIOS {
        let recorded: Vec<Recorded> = serde_json::from_reader(std::fs::File::open(
            testkit::recording(&format!("providers/vllm/{scenario}/chunks.json")),
        )?)?;
        let (events, mut expected): (Vec<_>, Vec<_>) = recorded
            .into_iter()
            .map(|chunk| match chunk {
                Recorded::Text { delta } => (
                    SessionEvent::AgentMessageChunk {
                        text: delta.clone(),
                    },
                    ModelChunk::Text { delta },
                ),
                Recorded::Reasoning { delta } => (
                    SessionEvent::AgentThoughtChunk {
                        text: delta.clone(),
                    },
                    ModelChunk::Reasoning { delta },
                ),
            })
            .unzip();
        expected.push(ModelChunk::Done);
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
        assert_eq!(actual, expected, "{scenario}");
        provider.shutdown().await;
        drop(actions);
    }
    Ok(())
}
