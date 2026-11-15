use super::*;

#[test]
fn search_requires_q_and_maps_hits() {
    let (_store, app) = seeded().expect("seed store");
    search_rejections(&app).expect("search rejections hold");

    let response = block_on(answer(
        app,
        recorded("GET", "/api/memory/search?q=hello", Vec::new()),
    ))
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let parsed: MemorySearchResponse =
        serde_json::from_slice(&response.body).expect("search response");
    assert_eq!(parsed.query, "hello");
    assert_eq!(parsed.scope, wire::gateway::memory::MemoryScope::Both);
    assert_eq!(parsed.results.len(), 1);
    let hit = parsed.results.first().expect("hit present");
    assert_eq!(hit.id, "m1");
    assert_eq!(hit.source, wire::gateway::memory::MemoryHitSource::Message);
    assert_eq!(hit.content, "hello world");
    assert_eq!(hit.role, Some(Role::User));
    assert_eq!(hit.agent.as_deref(), Some("rivet-claude"));
}

#[test]
fn browse_returns_messages() {
    let (store, app) = seeded().expect("seed store");
    let response = block_on(answer(
        app,
        recorded("GET", "/api/memory/browse", Vec::new()),
    ))
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let parsed: MemoryBrowseResponse =
        serde_json::from_slice(&response.body).expect("browse response");
    assert_eq!(parsed.messages.len(), 2);
    let message = parsed.messages.first().expect("message present");
    assert_eq!(message.id, "m1");
    assert_eq!(message.role, Role::User);
    assert_eq!(message.agent.as_deref(), Some("rivet-claude"));
    assert_eq!(message.session_id.as_deref(), Some(SESSION));
    let requests = block_on(store.requests());
    assert!(
        requests
            .iter()
            .any(|request| { *request == testkit::MemoryRequest::Browse(full_browse_filter()) })
    );
}

#[test]
fn browse_rejects_bad_filters() {
    let (store, app) = seeded().expect("seed store");
    let call = |target: &str| block_on(answer(app.clone(), recorded("GET", target, Vec::new())));

    let response = call("/api/memory/browse?tag=x").expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "tag must be key:value");

    let response = call("/api/memory/browse?window=fortnight").expect("in-process answer");
    assert_eq!(response.status, 400);
    let text = error_of(&response).error;
    assert!(text.contains("unknown window \"fortnight\""), "{text}");
    assert!(text.contains("last_7d"), "{text}");

    let response = call("/api/memory/browse?window=Last%207d").expect("in-process answer");
    assert_eq!(response.status, 200);

    let response =
        call("/api/memory/browse?role=user&tool_name=Bash&tag=project:acme&window=today&limit=11")
            .expect("in-process answer");
    assert_eq!(response.status, 200);
    let requests = block_on(store.requests());
    let filter = recorded_browses(&requests)
        .into_iter()
        .next_back()
        .expect("browse reached the port");
    assert_eq!(filter.role, Some(Role::User));
    assert_eq!(filter.tool_name.as_deref(), Some("Bash"));
    assert_eq!(
        filter.tag,
        Some(kernel::memory::TagLiteral {
            key: "project".to_owned(),
            value: "acme".to_owned(),
        })
    );
    assert_eq!(filter.window, None);
    let today = kernel::memory_core::resolve_window(
        kernel::memory::BrowseWindow::Today,
        chrono::Utc::now(),
        &chrono::Local,
    );
    assert_eq!(filter.since, today.since);
    assert_eq!(filter.before, today.before);
    assert_eq!(filter.limit, Limit::clamped(Some(11), 50, 1..=200));
}

#[test]
fn stats_passthrough_the_store_shape() {
    let (_store, app) = seeded().expect("seed store");
    let response = block_on(answer(
        app,
        recorded("GET", "/api/memory/stats", Vec::new()),
    ))
    .expect("answer");
    assert_eq!(response.status, 200);
    let parsed: wire::gateway::memory::MemoryStatsResponse =
        serde_json::from_slice(&response.body).expect("stats response");
    assert_eq!(parsed.conversations, 1);
    assert_eq!(parsed.messages, 2);
    assert_eq!(parsed.tool_calls, 0);
    assert_eq!(parsed.embed_queue_depth, 0);
    assert_eq!(parsed.top_tools, Vec::new());
    assert_eq!(parsed.recent_sessions, Vec::new());
}

#[test]
fn get_full_tool_reports_the_documented_strings() {
    let (_store, app) = seeded().expect("seed store");
    let response = post(
        &app,
        "/api/memory/tool/memory_get_full",
        &MemoryGetFullToolArgs { id: None },
    )
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    assert_eq!(
        tool_text(&response),
        "memory_get_full: id is required (string)."
    );

    let response = post(
        &app,
        "/api/memory/tool/memory_get_full",
        &MemoryGetFullToolArgs {
            id: Some("zz".to_owned()),
        },
    )
    .expect("in-process answer");
    assert_eq!(tool_text(&response), "No message with id zz.");

    let response = post(
        &app,
        "/api/memory/tool/memory_get_full",
        &MemoryGetFullToolArgs {
            id: Some("m1".to_owned()),
        },
    )
    .expect("in-process answer");
    let text = tool_text(&response);
    assert!(
        text.starts_with("(row was not truncated — stored payload is complete)"),
        "{text}"
    );
    assert!(text.contains("hello world"), "{text}");
}

#[test]
fn search_tool_answers_json_text_and_reports_failures() {
    let (_store, app) = seeded().expect("seed store");
    let response = post(
        &app,
        "/api/memory/tool/memory_search",
        &MemorySearchToolArgs {
            since: None,
            before: None,
            window: None,
            expand: None,
            synthesize: None,
            query: "hello".to_owned(),
            mode: None,
            scope: None,
            limit: Some(5),
            agent: None,
            role: None,
            tool_name: None,
            tag: None,
        },
    )
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let inner: MemorySearchResponse =
        serde_json::from_str(&tool_text(&response)).expect("tool result is json text");
    assert_eq!(inner.query, "hello");
    assert_eq!(inner.results.len(), 1);

    let response = block_on(answer(
        app.clone(),
        recorded(
            "POST",
            "/api/memory/tool/memory_search",
            br#"{"query":"hello","mode":"bogus"}"#.to_vec(),
        ),
    ))
    .expect("in-process answer");
    assert_eq!(response.status, 400);
    assert!(error_of(&response).error.contains("bogus"), "{response:?}");

    let response = post(
        &app,
        "/api/memory/tool/memory_search",
        &MemorySearchToolArgs {
            since: None,
            before: None,
            window: None,
            expand: None,
            synthesize: None,
            query: "hello".to_owned(),
            mode: None,
            scope: None,
            limit: None,
            agent: None,
            role: None,
            tool_name: None,
            tag: Some("nokey".to_owned()),
        },
    )
    .expect("in-process answer");
    assert_eq!(
        tool_text(&response),
        "Search failed: tag must be key:value (got \"nokey\")"
    );
}

#[test]
fn stats_tool_answers_json_text() {
    let (_store, app) = seeded().expect("seed store");
    let empty = serde_json::value::RawValue::from_string("{}".to_owned()).expect("raw");
    let response = post(&app, "/api/memory/tool/memory_stats", &empty).expect("in-process answer");
    assert_eq!(response.status, 200);
    let inner: wire::gateway::memory::MemoryStatsResponse =
        serde_json::from_str(&tool_text(&response)).expect("stats tool result is json text");
    assert_eq!(inner.messages, 2);
}

#[test]
fn browse_tool_validates_args() {
    let (_store, app) = seeded().expect("seed store");
    let response = block_on(answer(
        app.clone(),
        recorded(
            "POST",
            "/api/memory/tool/memory_browse",
            br#"{"window":"fortnight"}"#.to_vec(),
        ),
    ))
    .expect("in-process answer");
    assert_eq!(response.status, 400);
    assert!(
        error_of(&response).error.contains("fortnight"),
        "{response:?}"
    );

    let response = post(
        &app,
        "/api/memory/tool/memory_browse",
        &MemoryBrowseToolArgs {
            conversation_id: None,
            include_tools: None,
            since: None,
            before: None,
            window: None,
            agent: None,
            tag: Some("nokey".to_owned()),
            limit: None,
            order: None,
        },
    )
    .expect("in-process answer");
    assert_eq!(
        tool_text(&response),
        "Browse failed: tag must be key:value (got \"nokey\")"
    );

    let response = post(
        &app,
        "/api/memory/tool/memory_browse",
        &MemoryBrowseToolArgs {
            conversation_id: Some("zz".to_owned()),
            include_tools: None,
            since: None,
            before: None,
            window: None,
            agent: None,
            tag: None,
            limit: None,
            order: None,
        },
    )
    .expect("in-process answer");
    assert_eq!(
        tool_text(&response),
        "Browse failed: conversation_id must be a uuid"
    );
}

#[test]
fn browse_tool_carries_filters_to_the_port() {
    let started = chrono::Utc::now();
    let (store, app) = seeded().expect("seed store");
    let response = post(
        &app,
        "/api/memory/tool/memory_browse",
        &MemoryBrowseToolArgs {
            conversation_id: Some("00000000-0000-4000-8000-000000000009".to_owned()),
            include_tools: Some(true),
            since: None,
            before: None,
            window: Some(wire::gateway::memory::BrowseWindow::Last7d),
            agent: Some("rivet-claude".to_owned()),
            tag: Some("project:acmekit".to_owned()),
            limit: Some(3),
            order: Some(wire::gateway::memory::MemoryBrowseOrder::Asc),
        },
    )
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let parsed: MemoryBrowseResponse =
        serde_json::from_str(&tool_text(&response)).expect("browse json");
    assert_eq!(parsed.messages.len(), 2);
    let requests = block_on(store.requests());
    let filter = recorded_browses(&requests)
        .into_iter()
        .next_back()
        .expect("browse reached the port");
    assert_eq!(
        filter.tag,
        Some(kernel::memory::TagLiteral {
            key: "project".to_owned(),
            value: "acmekit".to_owned(),
        })
    );
    assert_eq!(filter.window, None);
    let since = filter.since.expect("resolved window start");
    assert!(since >= started - chrono::Duration::days(7));
    assert!(since <= chrono::Utc::now() - chrono::Duration::days(7));
    assert_eq!(filter.before, None);
    assert!(filter.include_tools);
    assert_eq!(filter.order, kernel::memory::MemoryBrowseOrder::Asc);
    assert_eq!(
        filter.conversation_id.map(|id| id.to_string()).as_deref(),
        Some("00000000-0000-4000-8000-000000000009")
    );
    assert_eq!(filter.agent.as_deref(), Some("rivet-claude"));
}

#[test]
fn append_tool_writes_through_the_port() {
    let (store, app) = seeded().expect("seed store");
    let response = post(
        &app,
        "/api/memory/tool/memory_append",
        &wire::gateway::memory::MemoryAppendToolArgs {
            session_id: "s-app".to_owned(),
            content: "appended".to_owned(),
            role: Role::User,
            tool_name: None,
            tool_args: None,
            tool_result: None,
            event_id: None,
            agent: Some("a".to_owned()),
            persona: None,
            source: None,
            channel: None,
        },
    )
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let inner: wire::gateway::OkReply =
        serde_json::from_str(&tool_text(&response)).expect("ok reply");
    assert!(inner.ok);
    let all = block_on(store.browse(full_browse_filter())).expect("browse succeeds");
    assert_eq!(all.len(), 3);
}

#[test]
fn ingest_session_tool_writes_through_the_port() {
    let (store, app) = seeded().expect("seed store");
    let response = post(
        &app,
        "/api/memory/tool/memory_ingest_session",
        &wire::gateway::memory::MemoryIngestSessionToolArgs {
            session_id: "s-ingest".to_owned(),
            messages: vec![
                wire::gateway::memory::MemoryIngestMessage {
                    role: Role::User,
                    content: "one".to_owned(),
                    created_at: None,
                    tool_calls: None,
                },
                wire::gateway::memory::MemoryIngestMessage {
                    role: Role::Assistant,
                    content: "two".to_owned(),
                    created_at: None,
                    tool_calls: None,
                },
            ],
            agent: None,
            persona: None,
            source: None,
            channel: None,
        },
    )
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let inner: wire::capture::CaptureResult =
        serde_json::from_str(&tool_text(&response)).expect("capture result");
    assert_eq!(inner.inserted, 2);
    let all = block_on(store.browse(full_browse_filter())).expect("browse succeeds");
    assert_eq!(all.len(), 4);
}

#[test]
fn unknown_memory_tools_are_404() {
    let (_store, app) = seeded().expect("seed store");
    let raw_tool = |name: &str, raw: &[u8]| {
        block_on(answer(
            app.clone(),
            recorded("POST", &format!("/api/memory/tool/{name}"), raw.to_vec()),
        ))
    };
    let response = raw_tool("memory_tags", b"{}").expect("in-process answer");
    assert_eq!(response.status, 404);
    assert_eq!(error_of(&response).error, "unknown memory tool");

    let response = raw_tool("not_a_tool", b"{}").expect("in-process answer");
    assert_eq!(response.status, 404);
    assert_eq!(error_of(&response).error, "unknown memory tool");

    let response = block_on(answer(
        app.clone(),
        recorded("GET", "/api/memory/tool/memory_stats", Vec::new()),
    ))
    .expect("in-process answer");
    assert_eq!(response.status, 405);
}

#[test]
fn tool_bodies_are_validated() {
    let (_store, app) = seeded().expect("seed store");
    let raw_tool = |raw: &[u8]| {
        block_on(answer(
            app.clone(),
            recorded("POST", "/api/memory/tool/memory_search", raw.to_vec()),
        ))
    };
    let response = raw_tool(br#"{"query": 5}"#).expect("in-process answer");
    assert_eq!(response.status, 400);

    let response = raw_tool(b"[]").expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "body must be a JSON object");

    let response = raw_tool(b"").expect("in-process answer");
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "invalid JSON");
}

#[test]
fn tags_lookup_returns_sessions() {
    let (_store, app) = seeded().expect("seed store");
    let response = post(
        &app,
        "/api/memory/tags/lookup",
        &MemoryTagsLookupRequest {
            session_keys: Some(vec![SESSION.to_owned()]),
            states: Some(vec!["suggested".to_owned()]),
        },
    )
    .expect("in-process answer");
    assert_eq!(response.status, 200);
    let parsed: TagLookupResult = serde_json::from_slice(&response.body).expect("lookup response");
    assert_eq!(parsed.sessions.len(), 0);
}
