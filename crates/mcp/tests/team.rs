use mcp::Server;
use rmcp::ServerHandler as _;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub mod support;

#[tokio::test]
async fn only_a_shared_hub_introduces_the_viewer_and_teammates() {
    let server = Server::builder(
        Arc::new(testkit::FakeMemoryStore::default()),
        false,
        CancellationToken::new(),
    )
    .store_identity("alex")
    .await
    .expect("identity")
    .build()
    .expect("server");
    assert_eq!(server.get_info().instructions, None);
    let reply = support::call(&server, "memory_team", "{}").await;
    assert_eq!(reply.is_error, None);
    assert!(
        serde_json::to_string(&reply)
            .expect("reply")
            .contains("No recent visible sessions from teammates.")
    );
    assert_eq!(
        support::call(&server, "memory_team", r#"{"limit":0}"#)
            .await
            .is_error,
        Some(true)
    );
}
