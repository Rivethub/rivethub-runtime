#![cfg(test)]

use kernel::{ToolContext, tool::Tool};
use mcp::{
    client::Client,
    transport::{Bearer, bind_unix, serve_tcp, serve_unix},
};
use rmcp::transport::{
    StreamableHttpClientTransport, streamable_http_client::StreamableHttpClientTransportConfig,
};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

pub mod support;

async fn remote(
    http: reqwest::Client,
    url: String,
    token: Option<&str>,
    shutdown: CancellationToken,
) -> Client {
    let config = StreamableHttpClientTransportConfig::with_uri(url);
    let config = match token {
        Some(token) => config.auth_header(token),
        None => config,
    };
    Client::connect(
        StreamableHttpClientTransport::with_client(http, config),
        shutdown,
    )
    .await
    .expect("connect")
}

async fn search(tools: &[Arc<dyn Tool>]) {
    let search = tools
        .iter()
        .find(|tool| tool.definition().name == "memory_search")
        .expect("memory_search");
    let context = ToolContext {
        agent_id: None,
        working_dir: None,
        session: None,
    };
    let result = search
        .call(
            Some(serde_json::from_str(r#"{"query":"remote ✓"}"#).expect("arguments")),
            context,
        )
        .await
        .expect("remote call");
    let [block] = result.content.as_slice() else {
        panic!("one result block")
    };
    let text = block.as_text().expect("text outcome");
    assert!(
        text.text
            .starts_with("No results found for query \"remote ✓\".")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tcp_auth_client_bridge_and_shutdown() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let server = support::server();
        let expected = server.tools().to_vec();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let url = format!("http://{}/mcp", listener.local_addr().expect("address"));
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(serve_tcp(
            listener,
            server,
            Bearer::new("test-secret").expect("bearer"),
            shutdown.clone(),
        ));
        let http = reqwest::Client::new();
        for token in [
            None,
            Some("Bearer incorrect"),
            Some("Basic test-secret"),
            Some("Bearer test-secret-extra"),
        ] {
            let request = http.post(&url);
            let request = match token {
                Some(token) => request.header("authorization", token),
                None => request,
            };
            assert_eq!(
                request.send().await.expect("auth response").status(),
                reqwest::StatusCode::UNAUTHORIZED
            );
        }
        let client = remote(http, url, Some("test-secret"), shutdown.child_token()).await;
        let tools = client.tools().await.expect("tools");
        assert_eq!(
            tools
                .iter()
                .map(|tool| tool.definition())
                .collect::<Vec<_>>(),
            expected
        );
        tokio::join!(search(&tools), search(&tools));
        client.close().await.expect("close");
        shutdown.cancel();
        task.await.expect("task").expect("serve");
        assert!(
            tools
                .first()
                .expect("tool")
                .call(
                    None,
                    ToolContext {
                        agent_id: None,
                        working_dir: None,
                        session: None
                    }
                )
                .await
                .is_err()
        );
    })
    .await
    .expect("deadline");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unix_socket_serves_with_and_without_a_bearer() {
    tokio::time::timeout(Duration::from_secs(15), async {
        for token in [None, Some("unix-secret")] {
            let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("directory");
            let path = directory.path().join("mcp.sock");
            let listener = bind_unix(&path).await.expect("bind");
            let shutdown = CancellationToken::new();
            let bearer = token.map(|token| Bearer::new(token).expect("bearer"));
            let task = tokio::spawn(serve_unix(
                listener,
                support::server(),
                bearer,
                shutdown.clone(),
            ));
            let http = reqwest::Client::builder()
                .unix_socket(path)
                .build()
                .expect("unix client");
            if token.is_some() {
                assert_eq!(
                    http.post("http://localhost/mcp")
                        .send()
                        .await
                        .expect("response")
                        .status(),
                    reqwest::StatusCode::UNAUTHORIZED
                );
            }
            let client = remote(
                http,
                "http://localhost/mcp".into(),
                token,
                shutdown.child_token(),
            )
            .await;
            search(&client.tools().await.expect("tools")).await;
            client.close().await.expect("close");
            shutdown.cancel();
            task.await.expect("task").expect("serve");
        }
    })
    .await
    .expect("deadline");
}

#[test]
fn bearer_refuses_empty_and_redacts_debug() {
    assert!(Bearer::new("").is_err());
    assert!(Bearer::new("a b").is_err());
    assert_eq!(
        format!("{:?}", Bearer::new("secret").expect("bearer")),
        "Bearer([redacted])"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_origin_duplicate_credentials_and_open_connection_shutdown() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let address = listener.local_addr().expect("address");
        let url = format!("http://{address}/mcp");
        let shutdown = CancellationToken::new();
        let config = rmcp::transport::streamable_http_server::StreamableHttpServerConfig::default()
            .with_allowed_hosts(["example.com"]);
        let task = tokio::spawn(mcp::transport::serve_tcp_with_config(
            listener,
            support::server(),
            Bearer::new("policy-secret").expect("token"),
            config,
            shutdown.clone(),
        ));
        let http = reqwest::Client::new();
        assert_eq!(
            http.post(&url)
                .bearer_auth("policy-secret")
                .send()
                .await
                .expect("host response")
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );
        assert_eq!(
            http.post(&url)
                .header("host", "example.com")
                .header("origin", "https://evil.example")
                .bearer_auth("policy-secret")
                .send()
                .await
                .expect("origin response")
                .status(),
            reqwest::StatusCode::FORBIDDEN
        );
        assert_eq!(
            http.post(&url)
                .bearer_auth("policy-secret")
                .header("authorization", "Bearer policy-secret")
                .send()
                .await
                .expect("duplicate response")
                .status(),
            reqwest::StatusCode::UNAUTHORIZED
        );
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            reqwest::header::HOST,
            reqwest::header::HeaderValue::from_static("example.com"),
        );
        let http = reqwest::Client::builder()
            .default_headers(headers)
            .build()
            .expect("client");
        let client = remote(http, url, Some("policy-secret"), shutdown.child_token()).await;
        search(&client.tools().await.expect("tools")).await;
        let idle = tokio::net::TcpStream::connect(address)
            .await
            .expect("idle connection");
        shutdown.cancel();
        task.await.expect("task").expect("shutdown");
        client.close().await.expect("close");
        drop(idle);
    })
    .await
    .expect("shutdown deadline");
}
