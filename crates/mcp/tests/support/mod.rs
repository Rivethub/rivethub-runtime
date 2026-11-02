#![cfg(test)]

use mcp::Server;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub fn server() -> Server {
    Server::new(
        Arc::new(testkit::FakeMemoryStore::default()),
        false,
        CancellationToken::new(),
    )
    .expect("server")
}

pub async fn call(server: &Server, name: &str, arguments: &str) -> CallToolResult {
    let mut request = CallToolRequestParams::new(name.to_owned());
    request.arguments = Some(serde_json::from_str(arguments).expect("arguments"));
    server.call(request).await.expect("tool protocol result")
}

#[derive(serde::Deserialize)]
struct ContractStamp {
    #[serde(rename = "contractVersion")]
    contract_version: u32,
}

pub fn contract_version(result: &CallToolResult) -> u32 {
    let meta = result.meta.as_ref().expect("contract meta");
    let stamp: ContractStamp = wire::JsonObject::from_serialize(meta)
        .expect("contract object")
        .deserialize()
        .expect("contract stamp");
    stamp.contract_version
}

pub fn without_contract(mut result: CallToolResult) -> CallToolResult {
    result.meta = None;
    result
}
