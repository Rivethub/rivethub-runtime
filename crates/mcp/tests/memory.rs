#![cfg(test)]

use kernel::memory::{
    BrowseFilter, Limit, SearchMode, SearchQuery, TagCountsQuery, TagListQuery, TagLookup,
    TagTaxonomyQuery,
};
use kernel::memory::{BrowseWindow, MemoryBrowseOrder, MemoryScope, TagState};
use mcp::Server;
use std::sync::Arc;
use testkit::{FakeMemoryStore, MemoryRequest};
use tokio_util::sync::CancellationToken;
use wire::message::Role;

pub mod support;

fn setup(write: bool) -> (Server, Arc<FakeMemoryStore>) {
    let store = Arc::new(FakeMemoryStore::default());
    let server = Server::new(store.clone(), write, CancellationToken::new()).expect("server");
    (server, store)
}

#[tokio::test]
async fn memory_requests_reach_kernel_with_resolved_windows() {
    let (server, store) = setup(false);
    let today = kernel::memory_core::resolve_window(
        BrowseWindow::Today,
        chrono::Utc::now(),
        &chrono::Local,
    );
    for (name, args) in [
        (
            "memory_search",
            r#"{"query":"日本語","mode":"trigram","scope":"messages","role":"tool","tool_name":"Bash","tag":"project:acme","limit":7}"#,
        ),
        (
            "memory_browse",
            r#"{"window":"today","include_tools":true,"order":"asc"}"#,
        ),
        ("memory_stats", "{}"),
        ("memory_get_full", r#"{"id":"row-1"}"#),
        ("memory_tags", "{}"),
        ("memory_tags", r#"{"action":"list","state":"accepted"}"#),
        ("memory_tags", r#"{"action":"counts","key":"project"}"#),
        ("memory_tags", r#"{"action":"lookup","session_keys":["s"]}"#),
        ("memory_tags", r#"{"action":"taxonomy","key":"project"}"#),
    ] {
        assert_ne!(
            support::call(&server, name, args).await.is_error,
            Some(true),
            "{name}"
        );
    }
    assert_eq!(store.requests().await, expected_requests(today));
}

#[tokio::test]
async fn an_unknown_search_role_is_refused_before_the_store() {
    let (server, store) = setup(false);
    let result = support::call(
        &server,
        "memory_search",
        r#"{"query":"disk","role":"robot"}"#,
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(store.requests().await, []);
}

fn expected_requests(today: kernel::memory_core::TimeRange) -> Vec<MemoryRequest> {
    vec![
        MemoryRequest::Search(SearchQuery {
            since: None,
            before: None,
            window: None,
            expand: None,
            synthesize: None,
            text: "日本語".into(),
            mode: SearchMode::Trigram,
            scope: MemoryScope::Messages,
            agent: None,
            role: Some(Role::Tool),
            exclude_roles: Vec::new(),
            tool_name: Some("Bash".into()),
            tag: Some("project:acme".parse().expect("tag")),
            limit: Limit::clamped(Some(7), 10, 1..=50),
        }),
        MemoryRequest::Browse(BrowseFilter {
            conversation_id: None,
            agent: None,
            role: None,
            tool_name: None,
            tag: None,
            window: None,
            since: today.since,
            before: today.before,
            include_tools: true,
            order: MemoryBrowseOrder::Asc,
            limit: Limit::clamped(None, 50, 1..=200),
        }),
        MemoryRequest::Stats,
        MemoryRequest::GetFull("row-1".into()),
        MemoryRequest::PendingTags(Limit::clamped(None, 100, 1..=1000)),
        MemoryRequest::ListTags(TagListQuery {
            entity_type: None,
            entity_id: None,
            key: None,
            value: None,
            states: vec![TagState::Accepted],
            limit: Limit::clamped(None, 100, 1..=1000),
        }),
        MemoryRequest::TagCounts(TagCountsQuery {
            key: Some("project".into()),
            limit: Limit::clamped(None, 100, 1..=1000),
        }),
        MemoryRequest::LookupTags(TagLookup {
            session_keys: vec!["s".into()],
            states: vec![],
        }),
        MemoryRequest::TagTaxonomy(TagTaxonomyQuery {
            key: Some("project".into()),
            state: None,
            limit: Limit::clamped(None, 100, 1..=1000),
        }),
    ]
}

#[tokio::test]
async fn invalid_and_unsupported_options_never_reach_store() {
    let (server, store) = setup(false);
    for (name, args) in [
        ("memory_search", r#"{"query":"x","unknown":true}"#),
        ("memory_search", r#"{"query":"x","limit":0}"#),
        ("memory_search", r#"{"query":"x","limit":2.5}"#),
        ("memory_search", r#"{"query":"x","mode":"bogus"}"#),
        ("memory_browse", r#"{"since":"yesterday"}"#),
        ("memory_browse", r#"{"conversation_id":"bogus"}"#),
        ("memory_browse", r#"{"tag":"missing-colon"}"#),
        ("memory_stats", r#"{"agent":"acme"}"#),
        ("memory_get_full", r#"{"id":""}"#),
        ("memory_tags", r#"{"action":"lookup","session_keys":[]}"#),
    ] {
        assert_eq!(
            support::call(&server, name, args).await.is_error,
            Some(true),
            "{name}: {args}"
        );
    }
    for action in [
        "decide",
        "add",
        "taxonomy_upsert",
        "taxonomy_decide",
        "taxonomy_merge",
    ] {
        assert_eq!(
            support::call(
                &server,
                "memory_tags",
                &format!(r#"{{"action":"{action}"}}"#)
            )
            .await
            .is_error,
            None
        );
    }
    assert_eq!(store.requests().await, Vec::new());
}

#[tokio::test]
async fn explicitly_authorized_tag_writes_reach_port() {
    let (server, store) = setup(true);
    store
        .script_session_entity(
            "s".to_owned(),
            "00000000-0000-4000-8000-000000000002"
                .parse()
                .expect("entity UUID"),
        )
        .await;
    for args in [
        r#"{"action":"decide","ids":["00000000-0000-4000-8000-000000000001"],"state":"accepted"}"#,
        r#"{"action":"add","entity_type":"conversation","session_key":"s","tag":"project:acme"}"#,
        r#"{"action":"taxonomy_upsert","key":"project","value":"acme","aliases":[]}"#,
        r#"{"action":"taxonomy_decide","entries":[{"key":"project","value":"acme"}],"state":"rejected"}"#,
        r#"{"action":"taxonomy_merge","key":"project","from":"old","into":"acme"}"#,
    ] {
        assert_ne!(
            support::call(&server, "memory_tags", args).await.is_error,
            Some(true)
        );
    }
    assert_eq!(store.requests().await.len(), 5);
}

#[tokio::test]
async fn memory_results_preserve_content_and_enabled_write_annotations() {
    use kernel::memory::MemoryEntry;
    use kernel::memory::MemoryStore;
    use wire::message::Role;
    let (server, store) = setup(true);
    let entry = MemoryEntry {
        id: Some("row-1".into()),
        session_id: "s".into(),
        agent: "acme".into(),
        channel: "test".into(),
        role: Role::Assistant,
        content: "日本語 😀".into(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: Some("2026-10-13T12:00:00Z".parse().expect("timestamp")),
    };
    store
        .append(&testkit::owner().expect("owner"), vec![entry.clone()])
        .await
        .expect("seed");
    let result = support::call(&server, "memory_get_full", r#"{"id":"row-1"}"#).await;
    assert_eq!(result.content.len(), 1);
    let content = result
        .content
        .first()
        .and_then(|block| block.as_text())
        .expect("text content");
    assert_eq!(
        content.text,
        "(row was not truncated — stored payload is complete)\n\n日本語 😀"
    );
    let tags = server
        .tools()
        .iter()
        .find(|tool| tool.name == "memory_tags")
        .expect("tags");
    let hints = tags.annotations.as_ref().expect("annotations");
    assert_eq!(hints.read_only_hint, Some(false));
    assert_eq!(hints.destructive_hint, Some(true));
}

#[tokio::test]
async fn browse_refuses_window_with_explicit_time_bounds_by_name() {
    let (server, store) = setup(false);
    for args in [
        r#"{"window":"today","since":"2026-10-12T00:00:00Z"}"#,
        r#"{"window":"today","before":"2026-10-13T00:00:00Z"}"#,
        r#"{"window":"today","since":"2026-10-12T00:00:00Z","before":"2026-10-13T00:00:00Z"}"#,
    ] {
        let result = support::call(&server, "memory_browse", args).await;
        assert_eq!(
            support::contract_version(&result),
            wire::contract::MEMORY_CONTRACT
        );
        assert_eq!(
            support::without_contract(result),
            rmcp::model::CallToolResult::error(vec![rmcp::model::ContentBlock::text(
                "window cannot be combined with since or before"
            )])
        );
    }
    assert_eq!(store.requests().await, Vec::new());
}

#[tokio::test]
async fn browse_explicit_time_bounds_reach_kernel_unchanged() {
    let (server, store) = setup(false);
    let result = support::call(
        &server,
        "memory_browse",
        r#"{"since":"2026-10-12T00:00:00Z","before":"2026-10-13T00:00:00Z"}"#,
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    assert_eq!(
        store.requests().await,
        vec![MemoryRequest::Browse(BrowseFilter {
            conversation_id: None,
            agent: None,
            role: None,
            tool_name: None,
            tag: None,
            window: None,
            since: Some("2026-10-12T00:00:00Z".parse().expect("since")),
            before: Some("2026-10-13T00:00:00Z".parse().expect("before")),
            include_tools: false,
            order: MemoryBrowseOrder::Desc,
            limit: Limit::clamped(None, 50, 1..=200),
        })]
    );
}

#[tokio::test]
async fn search_forwards_every_argument_and_preserves_the_outcome() {
    let (server, store) = setup(false);
    let outcome = kernel::memory::SearchOutcome {
        degraded: Some(kernel::memory::MemoryDegraded {
            reason: "http 503".into(),
            effect: "Keyword / FTS ranking only — not meaning-based.".into(),
        }),
        synthesis: Some("The release is ready.".into()),
        ..Default::default()
    };
    store.set_search_outcome(outcome.clone()).await;
    let reply = support::call(&server, "memory_search", r#"{"query":"release","mode":"vector","scope":"summaries","agent":"acme","tag":"project:acme","limit":3,"since":"2026-10-12T00:00:00Z","before":"2026-10-13T00:00:00Z","window":"today","expand":true,"synthesize":true}"#).await;
    assert_ne!(reply.is_error, Some(true));
    let text = reply
        .content
        .first()
        .and_then(|block| block.as_text())
        .expect("text");
    assert!(text.text.contains("http 503"));
    assert!(
        text.text
            .contains("Keyword / FTS ranking only — not meaning-based.")
    );
    assert!(text.text.contains("The release is ready."));
    assert_eq!(
        store.requests().await,
        vec![MemoryRequest::Search(SearchQuery {
            text: "release".into(),
            mode: SearchMode::Vector,
            scope: MemoryScope::Summaries,
            agent: Some("acme".into()),
            role: None,
            exclude_roles: Vec::new(),
            tool_name: None,
            tag: Some("project:acme".parse().expect("tag")),
            limit: Limit::clamped(Some(3), 10, 1..=50),
            since: Some("2026-10-12T00:00:00Z".parse().expect("since")),
            before: Some("2026-10-13T00:00:00Z".parse().expect("before")),
            window: None,
            expand: Some(true),
            synthesize: Some(true),
        })]
    );
}

#[tokio::test]
async fn search_accepts_windows_and_false_flags_but_rejects_wrong_types() {
    let (server, store) = setup(false);
    let started = chrono::Utc::now();
    let reply = support::call(
        &server,
        "memory_search",
        r#"{"query":"release","window":"last_7d","expand":false,"synthesize":false}"#,
    )
    .await;
    assert_ne!(reply.is_error, Some(true));
    let requests = store.requests().await;
    let [MemoryRequest::Search(query)] = requests.as_slice() else {
        panic!("search request")
    };
    assert_eq!(query.window, None);
    let since = query.since.expect("resolved window start");
    assert!(since >= started - chrono::Duration::days(7));
    assert!(since <= chrono::Utc::now() - chrono::Duration::days(7));
    assert_eq!(query.before, None);
    assert_eq!(query.expand, Some(false));
    assert_eq!(query.synthesize, Some(false));
    for args in [
        r#"{"query":"x","since":123}"#,
        r#"{"query":"x","before":"yesterday"}"#,
        r#"{"query":"x","window":true}"#,
        r#"{"query":"x","expand":"true"}"#,
        r#"{"query":"x","synthesize":1}"#,
    ] {
        assert_eq!(
            support::call(&server, "memory_search", args).await.is_error,
            Some(true)
        );
    }
    assert_eq!(store.requests().await, requests);
}

#[tokio::test]
async fn tool_previews_count_characters_and_reject_malformed_capture_metadata() {
    use kernel::memory::MemoryEntry;
    use kernel::memory::MemoryStore;
    let (server, store) = setup(false);
    let mut entry = MemoryEntry {
        id: Some("unicode-tool".into()),
        session_id: "unicode-session".into(),
        agent: "worker".into(),
        channel: "test".into(),
        role: wire::message::Role::Tool,
        content: "[tool] inspect".into(),
        tool_name: Some("inspect".into()),
        tool_args: None,
        tool_result: Some("🦀".repeat(801)),
        metadata: None,
        created_at: Some("2026-09-02T00:00:00Z".parse().expect("date")),
    };
    store
        .append(&testkit::owner().expect("owner"), vec![entry.clone()])
        .await
        .expect("seed");
    let result = support::call(&server, "memory_browse", r#"{"include_tools":true}"#).await;
    assert_eq!(result.is_error, None);
    let text = &result
        .content
        .first()
        .expect("content")
        .as_text()
        .expect("text")
        .text;
    assert!(text.contains("[tool_result (inspect) 801 chars]"));
    assert!(text.contains(&format!(
        "{}…\n…[display-truncated tool_result → memory_get_full id=unicode-tool]",
        "🦀".repeat(800)
    )));
    entry.id = Some("malformed".into());
    entry.metadata = Some(serde_json::from_str(r#"{"truncated":"true"}"#).expect("metadata"));
    store
        .append(&testkit::owner().expect("owner"), vec![entry])
        .await
        .expect("seed malformed metadata");
    let result = support::call(&server, "memory_get_full", r#"{"id":"malformed"}"#).await;
    assert_eq!(result.is_error, Some(true));
    assert!(
        result
            .content
            .first()
            .expect("content")
            .as_text()
            .expect("text")
            .text
            .contains("expected a boolean")
    );
}

fn headings(result: &rmcp::model::CallToolResult) -> Vec<(String, String)> {
    assert_eq!(result.is_error, None, "{result:?}");
    let text = &result
        .content
        .first()
        .expect("content")
        .as_text()
        .expect("text")
        .text;
    text.lines()
        .zip(text.lines().skip(1))
        .filter(|(heading, _)| heading.starts_with("- ["))
        .map(|(heading, body)| (body.to_owned(), heading.to_owned()))
        .collect()
}

fn heading_of(headings: &[(String, String)], body: &str) -> String {
    headings.iter().find(|(text, _)| text == body).map_or_else(
        || panic!("no hit for {body} in {headings:?}"),
        |(_, heading)| heading.clone(),
    )
}

fn bundled(
    author: &str,
    node: &str,
    conversation: &str,
    created: Option<&str>,
) -> wire::capture::BundledSession {
    let at = |text: &str| text.parse().expect("date");
    wire::capture::BundledSession {
        origin: wire::capture::SessionOrigin {
            author: author.into(),
            node: node.into(),
            conversation_id: conversation.into(),
            session_key: format!("claude-code:{conversation}"),
            agent: "rivet".into(),
            created_at: created.map(at),
            updated_at: created.map(at),
            identity: None,
        },
        batch: wire::capture::CaptureBatchRequest {
            session_key: format!("claude-code:{conversation}"),
            agent: "rivet".into(),
            channel: Some("claude-code".into()),
            title: None,
            settings: None,
            task_id: None,
            finalize: None,
            created_at: None,
            updated_at: None,
            messages: vec![wire::capture::CaptureMessage {
                event_id: format!("{conversation}-1"),
                role: Role::User,
                content: format!("bundle digest work in {conversation}"),
                tool_name: None,
                tool_args: None,
                tool_result: None,
                metadata: None,
                created_at: Some(at("2026-11-08T10:00:00Z")),
            }],
        },
    }
}

fn native(id: &str, metadata: Option<&str>) -> kernel::memory::MemoryEntry {
    kernel::memory::MemoryEntry {
        id: Some(id.into()),
        session_id: format!("claude-code:{id}"),
        agent: "rivet".into(),
        channel: "claude-code".into(),
        role: Role::User,
        content: format!("bundle digest work in {id}"),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: metadata.map(|text| serde_json::from_str(text).expect("metadata")),
        created_at: Some("2026-11-08T10:00:00Z".parse().expect("date")),
    }
}

#[tokio::test]
async fn an_imported_hit_names_its_origin_author() {
    use kernel::memory::MemoryStore;
    let (server, store) = setup(false);
    let owner = testkit::owner().expect("owner");
    let mut broken = bundled("phil", "desk", "broken", Some("2026-10-14T09:00:00Z"))
        .import_batch()
        .expect("batch");
    broken
        .settings
        .get_or_insert_default()
        .insert("origin", "slack")
        .expect("origin");
    for batch in [
        bundled("phil", "desk", "dated", Some("2026-10-14T09:00:00Z"))
            .import_batch()
            .expect("batch"),
        bundled("unknown", "unknown", "unknown", None)
            .import_batch()
            .expect("batch"),
        bundled("alex", "desk", "undated", None)
            .import_batch()
            .expect("batch"),
        broken,
    ] {
        let posted = serde_json::to_value(&batch).expect("posted");
        let received = serde_json::from_value(posted).expect("received");
        store.capture(received, &owner, None).await.expect("import");
    }
    store
        .append(&owner, vec![native("native", None)])
        .await
        .expect("seed");

    let found =
        headings(&support::call(&server, "memory_search", r#"{"query":"bundle digest"}"#).await);

    assert!(
        heading_of(&found, "bundle digest work in dated").ends_with(" from phil · 2026-10-14"),
        "{found:?}"
    );
    assert!(
        heading_of(&found, "bundle digest work in undated").ends_with(" from alex"),
        "{found:?}"
    );
    for untagged in ["unknown", "broken", "native"] {
        let heading = heading_of(&found, &format!("bundle digest work in {untagged}"));
        assert!(!heading.contains(" from "), "{found:?}");
    }
}

#[tokio::test]
async fn message_metadata_never_tags_or_breaks_a_search() {
    use kernel::memory::MemoryStore;
    let (server, store) = setup(false);
    let spoof = r#"{"origin":{"author":"phil","node":"desk","conversation_id":"c1","session_key":"claude-code:one","agent":"rivet","created_at":"2026-10-14T09:00:00Z","updated_at":null}}"#;
    store
        .append(
            &testkit::owner().expect("owner"),
            vec![
                native("spoof", Some(spoof)),
                native("slack", Some(r#"{"origin":"slack"}"#)),
            ],
        )
        .await
        .expect("seed");

    let found =
        headings(&support::call(&server, "memory_search", r#"{"query":"bundle digest"}"#).await);

    assert_eq!(found.len(), 2, "{found:?}");
    for (_, heading) in &found {
        assert!(!heading.contains(" from "), "{found:?}");
    }
}
