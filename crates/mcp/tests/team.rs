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

#[tokio::test]
async fn teammate_sessions_carry_a_page_only_when_the_origin_and_the_key_are_known() {
    let store = Arc::new(testkit::FakeMemoryStore::default());
    let when = wire::time::parse_rfc3339("2026-11-14T01:37:50Z").expect("when");
    store
        .script_team_sessions(vec![
            kernel::memory::TeamSession {
                owner: "ines".into(),
                session_id: "claude-code:field-gulls".into(),
                title: "Field notes".into(),
                when,
                topic: vec!["Work".into()],
                excerpt: Some("shared notes".into()),
            },
            kernel::memory::TeamSession {
                owner: "tom".into(),
                session_id: "a title".into(),
                title: "a title".into(),
                when,
                topic: Vec::new(),
                excerpt: None,
            },
        ])
        .await;
    let plain = Server::new(store.clone(), CancellationToken::new()).expect("server");
    let linked = Server::builder(store, CancellationToken::new())
        .page_origin(url::Url::parse("https://localhost:5174").expect("origin"))
        .build()
        .expect("server");
    let before =
        serde_json::to_string(&support::call(&plain, "memory_team", "{}").await).expect("json");
    let after =
        serde_json::to_string(&support::call(&linked, "memory_team", "{}").await).expect("json");
    let url = "https://localhost:5174/sessions/claude-code%3Afield-gulls";
    assert!(!before.contains("https://"));
    assert_eq!(after.matches(url).count(), 1, "{after}");
    assert!(after.contains("shared notes"), "{after}");
    assert!(after.contains("a title"), "{after}");
}
