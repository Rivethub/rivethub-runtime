#![cfg(test)]

use mcp::Server;
use testkit::translate_recorded_product_names;

use rmcp::{
    ServiceExt,
    model::{CallToolRequestParams, CallToolResult, ClientConfig, ProtocolVersion, Tool},
    service::ServiceError,
};
use serde::Deserialize;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub mod support;

#[derive(Deserialize)]
struct Catalog {
    tools: Vec<Tool>,
}
#[derive(Deserialize)]
struct Metadata {
    server: rmcp::model::ServerPeerInfo,
}
#[derive(Deserialize)]
struct Calls {
    calls: Vec<Call>,
}
#[derive(Deserialize)]
struct Call {
    request: CallToolRequestParams,
    note: String,
    #[serde(flatten)]
    outcome: Outcome,
}
#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Outcome {
    Result { result: CallToolResult },
    ProtocolError { code: i32, message: String },
}

const NOT_SERVED: &[&str] = &[
    "internet_search",
    "web_fetch",
    "echo",
    "skill_list",
    "skill_manage",
    "delegate_task",
    "list_agents",
];

fn recording_server() -> Server {
    Server::builder(
        std::sync::Arc::new(testkit::FakeMemoryStore::default()),
        CancellationToken::new(),
    )
    .build()
    .expect("recording server")
}

fn load<T: serde::de::DeserializeOwned>(name: &str) -> T {
    serde_json::from_str(
        &std::fs::read_to_string(testkit::recording(&format!("mcp/{name}.json")))
            .expect("recording"),
    )
    .expect("typed recording")
}

fn assert_recorded(actual: CallToolResult, expected: CallToolResult, note: &str) {
    assert_eq!(
        support::contract_version(&actual),
        wire::contract::MEMORY_CONTRACT,
        "{note}"
    );
    assert_eq!(
        support::without_contract(actual),
        support::without_contract(expected),
        "{note}"
    );
}

fn translate_schema_descriptions(schema: &mut schemars::Schema) {
    if let Some(description) = schema.get("description").and_then(|value| value.as_str()) {
        let translated = translate_recorded_product_names(description);
        schema.insert("description".into(), translated.into());
    }
    schemars::transform::transform_subschemas(&mut translate_schema_descriptions, schema);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recordings_over_stdio_framing() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let server = recording_server();
        let expected: Catalog = load("tools-list");
        let calls: Calls = load("calls");
        let metadata: Metadata = load("meta");
        let mut tools = Vec::new();
        for mut tool in expected.tools {
            if !NOT_SERVED.contains(&tool.name.as_ref()) {
                tool.description = tool
                    .description
                    .map(|text| translate_recorded_product_names(&text).into());
                let mut schema = serde_json::from_value(
                    serde_json::to_value(&tool.input_schema).expect("schema value"),
                )
                .expect("schema");
                translate_schema_descriptions(&mut schema);
                tool.input_schema = std::sync::Arc::new(
                    serde_json::from_value(serde_json::to_value(schema).expect("schema value"))
                        .expect("schema object"),
                );
                tools.push(tool);
            }
        }
        let (client_io, server_io) = tokio::io::duplex(65536);
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(mcp::transport::serve_io(
            server,
            server_io,
            shutdown.clone(),
        ));
        let mut config = ClientConfig::default();
        config.protocol_version = ProtocolVersion::V_2025_11_25;
        let client = config.serve(client_io).await.expect("initialize");
        let info = client.peer_info().expect("server metadata");
        assert_eq!(info.protocol_version, metadata.server.protocol_version);
        assert_eq!(info.capabilities, metadata.server.capabilities);
        let identity = info.server_info.as_ref().expect("server identity");
        assert_eq!(identity.name, "rivethub-mcp-server");
        assert_eq!(identity.version, env!("CARGO_PKG_VERSION"));
        let actual = client.list_all_tools().await.expect("catalog");
        assert!(actual.iter().any(|tool| tool.name == "memory_team"));
        assert_eq!(
            actual
                .into_iter()
                .filter(|tool| tool.name != "memory_team")
                .collect::<Vec<_>>(),
            tools
        );
        for call in calls.calls {
            if NOT_SERVED.contains(&call.request.name.as_ref()) {
                continue;
            }
            let result = client.call_tool(call.request).await;
            match call.outcome {
                Outcome::Result { result: expected } => {
                    assert_recorded(result.expect("result"), expected, &call.note);
                }
                Outcome::ProtocolError { code, message } => {
                    let ServiceError::McpError(error) = result.expect_err("protocol error") else {
                        panic!("not a protocol error")
                    };
                    assert_eq!(error.code.0, code);
                    assert_eq!(error.message, message);
                }
            }
        }
        client.cancel().await.expect("client closed");
        shutdown.cancel();
        task.await.expect("task").expect("server closed");
    })
    .await
    .expect("replay deadline");
}
