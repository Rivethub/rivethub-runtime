use crate::common;

use std::sync::Arc;

use common::answer;
use futures::executor::block_on;
use gateway::Gateway;
use kernel::memory::Limit;
use kernel::memory::MemoryEntry;
use kernel::memory::MemoryStore;
use testkit::Request;
use wire::gateway::GatewayError;
use wire::gateway::memory::MemoryBrowseResponse;
use wire::gateway::memory::MemoryBrowseToolArgs;
use wire::gateway::memory::MemoryGetFullToolArgs;
use wire::gateway::memory::MemorySearchResponse;
use wire::gateway::memory::MemorySearchToolArgs;
use wire::gateway::memory::MemoryTagsAddRequest;
use wire::gateway::memory::MemoryTagsDecideRequest;
use wire::gateway::memory::MemoryTagsLookupRequest;
use wire::gateway::memory::MemoryToolResponse;
use wire::gateway::memory::PendingTagListResult;
use wire::gateway::memory::TagAddResult;
use wire::gateway::memory::TagCountsResult;
use wire::gateway::memory::TagDecisionResult;
use wire::gateway::memory::TagListResult;
use wire::gateway::memory::TagLookupResult;
use wire::gateway::memory::TagState;
use wire::gateway::memory::TagTaxonomyDecideRequest;
use wire::gateway::memory::TagTaxonomyListResult;
use wire::gateway::memory::TagTaxonomyMergeRequest;
use wire::gateway::memory::TagTaxonomyMergeResult;
use wire::gateway::memory::TagTaxonomyUpsertRequest;
use wire::gateway::memory::TagTaxonomyUpsertResult;
use wire::message::Role;

const SESSION: &str = "claude-code:11111111-2222-4333-8444-555555555555";

fn recorded(method: &str, target: &str, body: Vec<u8>) -> Request {
    Request {
        method: method.to_string(),
        target: target.to_string(),
        headers: vec![("Host".to_string(), "127.0.0.1:5174".to_string())],
        body,
        recipe: None,
        via: None,
    }
}

fn post<T: serde::Serialize>(
    app: &axum::Router,
    path: &str,
    value: &T,
) -> Result<testkit::Response, String> {
    let body = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    block_on(answer(app.clone(), recorded("POST", path, body)))
}

fn seeded() -> Result<(Arc<testkit::FakeMemoryStore>, axum::Router), String> {
    let (gateway, store) = common::gateway();
    let batch = vec![
        entry("m1", "hello world", "2026-10-10T12:00:00Z"),
        entry("m2", "goodbye", "2026-10-10T13:00:00Z"),
    ];
    let owner = testkit::owner().map_err(|error| error.to_string())?;
    block_on(store.append(&owner, batch)).map_err(|error| error.to_string())?;
    Ok((store, gateway.router(false)))
}

fn entry(id: &str, content: &str, created_at: &str) -> MemoryEntry {
    #[expect(
        clippy::expect_used,
        reason = "the test literals are constants; a malformed literal must fail the test, not drop the timestamp"
    )]
    fn at(created_at: &str) -> chrono::DateTime<chrono::Utc> {
        chrono::DateTime::parse_from_rfc3339(created_at)
            .expect("entry created_at is an RFC 3339 literal")
            .with_timezone(&chrono::Utc)
    }
    MemoryEntry {
        id: Some(id.to_owned()),
        session_id: SESSION.to_owned(),
        agent: "rivet-claude".to_owned(),
        channel: "claude-code".to_owned(),
        role: Role::User,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: Some(at(created_at)),
    }
}

fn error_of(response: &testkit::Response) -> GatewayError {
    serde_json::from_slice(&response.body)
        .unwrap_or_else(|_| GatewayError::new(String::from_utf8_lossy(&response.body).into_owned()))
}

fn tool_text(response: &testkit::Response) -> String {
    let body = String::from_utf8_lossy(&response.body).into_owned();
    let Ok(parsed) = serde_json::from_str::<MemoryToolResponse>(&body) else {
        return format!("<not a tool reply: {body}>");
    };
    let wire::message::ToolResult::Text(text) = parsed.result else {
        return format!("<not a text tool reply: {body}>");
    };
    text
}

fn full_browse_filter() -> kernel::memory::BrowseFilter {
    kernel::memory::BrowseFilter {
        conversation_id: None,
        agent: None,
        role: None,
        tool_name: None,
        tag: None,
        window: None,
        since: None,
        before: None,
        include_tools: false,
        order: kernel::memory::MemoryBrowseOrder::Desc,
        limit: Limit::clamped(None, 50, 1..=200),
    }
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "only the search arm carries a query; the fake records sixteen request kinds"
)]
fn recorded_searches(requests: &[testkit::MemoryRequest]) -> Vec<&kernel::memory::SearchQuery> {
    requests
        .iter()
        .filter_map(|request| match request {
            testkit::MemoryRequest::Search(query) => Some(query),
            _ => None,
        })
        .collect()
}

#[expect(
    clippy::wildcard_enum_match_arm,
    reason = "only the browse arm carries a filter; the fake records sixteen request kinds"
)]
fn recorded_browses(requests: &[testkit::MemoryRequest]) -> Vec<&kernel::memory::BrowseFilter> {
    requests
        .iter()
        .filter_map(|request| match request {
            testkit::MemoryRequest::Browse(filter) => Some(filter),
            _ => None,
        })
        .collect()
}

fn search_rejections(app: &axum::Router) -> Result<(), String> {
    let call = |method: &str, target: &str| {
        block_on(answer(app.clone(), recorded(method, target, Vec::new())))
    };
    let response = call("GET", "/api/memory/search")?;
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "q required");

    let response = call("GET", "/api/memory/search?q=x&scope=bogus")?;
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "invalid scope");

    let response = call("GET", "/api/memory/search?q=x&limit=abc")?;
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "invalid limit");

    let response = call("GET", "/api/memory/search?q=x&limit=0")?;
    assert_eq!(response.status, 200);

    let response = call("GET", "/api/memory/search?q=x&tag=nokey")?;
    assert_eq!(response.status, 400);
    assert_eq!(error_of(&response).error, "tag must be key:value");

    let response = call("POST", "/api/memory/search")?;
    assert_eq!(response.status, 405);
    assert_eq!(error_of(&response).error, "method not allowed");
    Ok(())
}

#[test]
fn memory_contract_is_a_header_and_a_route() -> Result<(), String> {
    let (gateway, _) = common::gateway();
    let app = gateway.router(false);
    let response = block_on(answer(
        app,
        recorded("GET", "/api/memory/contract", Vec::new()),
    ))?;
    assert_eq!(response.status, 200);
    let body =
        serde_json::from_slice::<wire::gateway::memory::MemoryContractResponse>(&response.body)
            .map_err(|error| error.to_string())?;
    assert_eq!(body.contract_version, wire::contract::MEMORY_CONTRACT);
    assert!(response.headers.iter().any(|(name, value)| {
        name.eq_ignore_ascii_case("x-rivethub-contract")
            && value == &wire::contract::MEMORY_CONTRACT.to_string()
    }));
    let store: Arc<dyn MemoryStore> = Arc::new(testkit::FakeMemoryStore::default());
    let narrowed = Arc::new(Gateway::new(store, common::settings()).with_memory_profile());
    let app = narrowed.router(false);
    let hidden = block_on(answer(
        app.clone(),
        recorded("GET", "/api/terminal/list", Vec::new()),
    ))?;
    assert_eq!(hidden.status, 404);
    let sessions = block_on(answer(app, recorded("GET", "/sessions", Vec::new())))?;
    assert_eq!(sessions.status, 404);
    Ok(())
}

#[path = "memory_api/archive.rs"]
mod archive;
#[path = "memory_api/search_port.rs"]
mod search_port;
#[path = "memory_api/tags_and_health.rs"]
mod tags_and_health;
#[path = "memory_api/tools.rs"]
mod tools;
