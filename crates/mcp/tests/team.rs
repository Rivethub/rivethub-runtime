use mcp::Server;
use rmcp::ServerHandler as _;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub mod support;

#[tokio::test]
async fn only_a_shared_hub_introduces_the_viewer_and_teammates() {
    let server = Server::builder(
        Arc::new(testkit::FakeMemoryStore::default()),
        CancellationToken::new(),
    )
    .store_identity("ines")
    .await
    .expect("identity")
    .build()
    .expect("server");
    assert_eq!(
        server.get_info().instructions.as_deref(),
        Some(concat!(
            "For memory_search: ",
            "Use a few specific keywords instead of a full sentence. Include names, identifiers, error text, or the subject, such as \"build cache timeout\". If nothing matches, try a second set of keywords."
        ))
    );
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
