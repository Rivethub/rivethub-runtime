use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use kernel::plugins::{
    PluginTool, PluginToolCall, PluginToolEntry, PluginToolError, PluginToolHint, PluginToolList,
    PluginToolResult, PluginTools,
};
use mcp::Server;
use mcp::client::Client;
use mcp::transport::{Bearer, serve_tcp};
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use tokio_util::sync::CancellationToken;
use wire::plugin::{PluginId, ToolName};

pub mod support;

struct FixturePlugin {
    list: PluginToolList,
}

#[async_trait]
impl PluginTools for FixturePlugin {
    async fn tools(&self) -> Result<PluginToolList, PluginToolError> {
        Ok(self.list.clone())
    }

    async fn call(
        &self,
        plugin: &PluginId,
        tool: &ToolName,
        call: PluginToolCall,
    ) -> Result<PluginToolResult, PluginToolError> {
        let arguments =
            serde_json::to_string(&call.arguments).map_err(|error| PluginToolError::Failed {
                plugin: plugin.as_ref().to_owned(),
                tool: tool.as_ref().to_owned(),
                detail: error.to_string(),
            })?;
        Ok(PluginToolResult {
            text: format!("{}/{} {arguments}", plugin.as_ref(), tool.as_ref()),
            is_error: false,
        })
    }

    async fn changed(&self, _seen: u64) -> Result<u64, PluginToolError> {
        std::future::pending().await
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn plugin_tools_follow_the_nine_and_answer_calls() {
    let tool = PluginTool {
        name: ToolName::try_from("echo".to_owned()).expect("name"),
        title: Some("Echo".to_owned()),
        description: "Echo the arguments.".to_owned(),
        input_schema: serde_json::from_str(r#"{"type": "object"}"#).expect("schema"),
        hints: vec![PluginToolHint::ReadOnly],
    };
    let list = PluginToolList {
        version: 1,
        tools: vec![PluginToolEntry {
            plugin: PluginId::try_from("fixture-kit".to_owned()).expect("id"),
            tool,
        }],
    };
    let server = Server::builder(
        Arc::new(testkit::FakeMemoryStore::default()),
        false,
        CancellationToken::new(),
    )
    .plugins(Arc::new(FixturePlugin { list }))
    .build()
    .expect("server");
    let answered = support::call(&server, "fixture-kit-echo", r#"{"say": "hi"}"#).await;
    let [block] = answered.content.as_slice() else {
        panic!("one result block")
    };
    assert_eq!(
        block.as_text().expect("text").text,
        r#"fixture-kit/echo {"say":"hi"}"#
    );
    assert_eq!(answered.is_error, Some(false));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let url = format!("http://{}/mcp", listener.local_addr().expect("address"));
    let shutdown = CancellationToken::new();
    let task = tokio::spawn(serve_tcp(
        listener,
        server,
        Bearer::new("plugin-secret").expect("bearer"),
        shutdown.clone(),
    ));
    let config = StreamableHttpClientTransportConfig::with_uri(url).auth_header("plugin-secret");
    let client = tokio::time::timeout(
        Duration::from_secs(10),
        Client::connect(
            StreamableHttpClientTransport::with_client(reqwest::Client::new(), config),
            shutdown.child_token(),
        ),
    )
    .await
    .expect("deadline")
    .expect("connect");
    let names: Vec<String> = client
        .tools()
        .await
        .expect("tools")
        .iter()
        .map(|tool| tool.definition().name.to_string())
        .collect();
    assert_eq!(names.last().map(String::as_str), Some("fixture-kit-echo"));
    assert!(names.contains(&"memory_search".to_owned()));
    let listed = client.tools().await.expect("tools");
    let echo = listed
        .iter()
        .find(|tool| tool.definition().name == "fixture-kit-echo")
        .expect("echo")
        .definition();
    assert_eq!(
        echo.annotations.and_then(|hints| hints.read_only_hint),
        Some(true)
    );
    client.close().await.expect("close");
    shutdown.cancel();
    task.await.expect("task").expect("serve");
}
