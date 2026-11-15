#[path = "common/mod.rs"]
#[expect(dead_code)]
mod common;

use common::captured_requests;
use common::chat_provider;
use common::drive;
use common::scenario_record;
use common::scenario_server;
use kernel::model::ModelProvider;
use testkit::Json;
use testkit::ProviderTurn;

#[tokio::test]
async fn a_text_replays_against_the_recording() {
    drive("A-text").await;
}

#[tokio::test]
async fn b_tool_replays_against_the_recording() {
    drive("B-tool").await;
}

#[tokio::test]
async fn c_reasoning_replays_against_the_recording() {
    drive("C-reasoning").await;
}

#[tokio::test]
async fn d_thinking_off_replays_against_the_recording() {
    drive("D-thinking-off").await;
}

#[tokio::test]
async fn e_reasoning_tool_replays_against_the_recording() {
    drive("E-reasoning-tool").await;
}

#[tokio::test]
async fn probe_replays_against_the_recording() {
    let recorded = scenario_record("probe");
    let server = scenario_server(recorded.canned.clone()).await;
    let provider = chat_provider(server.addr, None);
    let models = provider
        .list_models()
        .await
        .expect("the probe lists models");
    assert_eq!(models, vec!["test-model".to_owned()]);
    let turn = ProviderTurn {
        requests: captured_requests(&server.recorder),
        chunks: Vec::new(),
    };
    drop(server);
    common::replay(&recorded, &turn);
}

#[tokio::test]
async fn a_configured_max_tokens_is_sent_as_is() {
    let recorded = scenario_record("A-text");
    let raw_recorded =
        testkit::provider::load_scenario(&testkit::recording("providers/vllm/A-text"))
            .expect("the recorded scenario loads");
    let server = scenario_server(recorded.canned.clone()).await;
    let provider = chat_provider(server.addr, Some(4096));
    let raw = raw_recorded
        .requests
        .first()
        .expect("the recorded request")
        .body
        .clone();
    let body: Json = serde_json::from_str(&raw).expect("body parses");
    let mut rx = common::model_request_to_stream(&provider, "A-text", &body)
        .await
        .expect("the stream starts");
    while rx.recv().await.is_some() {}
    let call = server
        .recorder
        .lock()
        .first()
        .cloned()
        .expect("the chat request is captured");
    let sent: Json = serde_json::from_str(&call.body).expect("sent body parses");
    assert_eq!(
        sent.get("max_tokens")
            .and_then(Json::as_u64)
            .expect("configured max_tokens travels"),
        4096
    );
}
