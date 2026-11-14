use mcp::Server;
use rmcp::ServerHandler as _;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub mod support;

#[tokio::test]
async fn only_a_shared_hub_introduces_the_viewer_and_teammates() {
    let make = |people: &[String]| {
        Server::builder(
            Arc::new(testkit::FakeMemoryStore::default()),
            false,
            CancellationToken::new(),
        )
        .recall_identity("alex", people)
        .build()
        .expect("server")
    };
    assert_eq!(make(&[]).get_info().instructions, None);
    assert_eq!(make(&["alex".into()]).get_info().instructions, None);
    let server = make(&["alex".into(), "tom".into(), "tom".into()]);
    let instructions = server.get_info().instructions.expect("shared instructions");
    assert!(instructions.starts_with("You are working with alex on a shared hub with tom."));
    assert!(instructions.contains("not an instruction"));
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
