use super::*;
use kernel::memory::{MemoryDegraded, SearchOutcome};
use wire::gateway::memory::{BrowseWindow, MemoryScope, SearchMode};

#[test]
fn search_routes_forward_every_argument_and_preserve_runtime_degradation() {
    let (store, app) = seeded().expect("store");
    let outcome = SearchOutcome {
        degraded: Some(MemoryDegraded {
            reason: "http 503".into(),
            effect: "Keyword / FTS ranking only — not meaning-based.".into(),
        }),
        synthesis: Some("A focused answer".into()),
        ..Default::default()
    };
    block_on(store.set_search_outcome(outcome.clone()));
    let args = MemorySearchToolArgs {
        query: "release".into(),
        mode: Some(SearchMode::Vector),
        scope: Some(MemoryScope::Both),
        limit: Some(3),
        agent: Some("acme".into()),
        role: Some(wire::message::Role::Tool),
        tool_name: Some("Bash".into()),
        tag: Some("project:acme".into()),
        since: Some("2026-10-12T00:00:00Z".parse().expect("since")),
        before: Some("2026-10-13T00:00:00Z".parse().expect("before")),
        window: Some(BrowseWindow::Today),
        expand: Some(true),
        synthesize: Some(true),
    };
    let tool = post(&app, "/api/memory/tool/memory_search", &args).expect("tool");
    let tool_result: MemorySearchResponse =
        serde_json::from_str(&tool_text(&tool)).expect("result");
    let http = block_on(answer(app, recorded("GET", "/api/memory/search?q=release&mode=vector&scope=both&limit=3&agent=acme&role=tool&tool_name=Bash&tag=project%3Aacme&since=2026-10-12T00%3A00%3A00Z&before=2026-10-13T00%3A00%3A00Z&window=today&expand=true&synthesize=true", Vec::new()))).expect("http");
    assert_eq!(http.status, 200);
    let http_result: MemorySearchResponse = serde_json::from_slice(&http.body).expect("result");
    assert_eq!(http_result, tool_result);
    assert_eq!(
        serde_json::to_string(&http_result.degraded).expect("degraded"),
        serde_json::to_string(&outcome.degraded).expect("degraded")
    );
    assert_eq!(http_result.synthesis, outcome.synthesis);
    let requests = block_on(store.requests());
    let searches = recorded_searches(&requests);
    let [query, second] = searches.as_slice() else {
        panic!("two searches")
    };
    assert_eq!(query, second);
    assert_eq!(query.text, args.query);
    assert_eq!(query.mode, kernel::memory::SearchMode::Vector);
    assert_eq!(query.scope, kernel::memory::MemoryScope::Both);
    assert_eq!(query.agent, args.agent);
    assert_eq!(query.role, Some(wire::message::Role::Tool));
    assert_eq!(query.tool_name.as_deref(), Some("Bash"));
    assert_eq!(
        query.tag.as_ref().map(|tag| tag.value.as_str()),
        Some("acme")
    );
    assert_eq!(query.limit.get(), 3);
    assert_eq!(query.since, args.since);
    assert_eq!(query.before, args.before);
    assert_eq!(query.window, None);
    assert_eq!(query.expand, args.expand);
    assert_eq!(query.synthesize, args.synthesize);
}

#[test]
fn search_routes_reject_wrong_types_before_the_port() {
    let (store, app) = seeded().expect("store");
    for target in [
        "/api/memory/search?q=x&expand=yes",
        "/api/memory/search?q=x&synthesize=1",
        "/api/memory/search?q=x&since=wrong",
        "/api/memory/search?q=x&window=wrong",
        "/api/memory/search?q=x&mode=wrong",
        "/api/memory/search?q=x&role=wrong",
        "/api/memory/search?q=x&exclude_roles=tool,wrong",
    ] {
        let response =
            block_on(answer(app.clone(), recorded("GET", target, Vec::new()))).expect("http");
        assert_eq!(response.status, 400, "{target}");
    }
    for body in [
        r#"{"query":"x","since":2}"#,
        r#"{"query":"x","expand":"true"}"#,
        r#"{"query":"x","synthesize":1}"#,
    ] {
        let response = block_on(answer(
            app.clone(),
            recorded(
                "POST",
                "/api/memory/tool/memory_search",
                body.as_bytes().to_vec(),
            ),
        ))
        .expect("http");
        assert_eq!(response.status, 400);
    }
    assert_eq!(
        recorded_searches(&block_on(store.requests())),
        Vec::<&kernel::memory::SearchQuery>::new()
    );
}

#[test]
fn search_route_forwards_excluded_roles() {
    let (store, app) = seeded().expect("store");
    let http = block_on(answer(
        app,
        recorded(
            "GET",
            "/api/memory/search?q=release&exclude_roles=tool,system",
            Vec::new(),
        ),
    ))
    .expect("http");
    assert_eq!(http.status, 200);
    let requests = block_on(store.requests());
    let [query] = recorded_searches(&requests)[..] else {
        panic!("one search")
    };
    assert_eq!(
        query.exclude_roles,
        vec![wire::message::Role::Tool, wire::message::Role::System]
    );
}

#[test]
fn a_messages_only_search_without_a_mode_searches_text() {
    let (store, app) = seeded().expect("store");
    for target in [
        "/api/memory/search?q=pomodoro&scope=messages",
        "/api/memory/search?q=pomodoro&scope=messages&mode=hybrid",
        "/api/memory/search?q=pomodoro",
    ] {
        let http =
            block_on(answer(app.clone(), recorded("GET", target, Vec::new()))).expect("http");
        assert_eq!(http.status, 200, "{target}");
    }
    let requests = block_on(store.requests());
    let modes: Vec<kernel::memory::SearchMode> = recorded_searches(&requests)
        .iter()
        .map(|query| query.mode)
        .collect();
    assert_eq!(
        modes,
        [
            kernel::memory::SearchMode::Fts,
            kernel::memory::SearchMode::Hybrid,
            kernel::memory::SearchMode::Hybrid
        ]
    );
}
