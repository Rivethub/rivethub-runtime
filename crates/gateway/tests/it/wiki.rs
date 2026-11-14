use crate::common;

use std::sync::Arc;
use std::time::Duration;

use testkit::Request;
use testkit::Response;
use wire::gateway::GatewayError;
use wire::gateway::wiki::WikiCitation;
use wire::gateway::wiki::WikiGapsResponse;
use wire::gateway::wiki::WikiHistoryEntry;
use wire::gateway::wiki::WikiMissBody;
use wire::gateway::wiki::WikiPageResponse;
use wire::gateway::wiki::WikiTopicList;

fn request(method: &str, target: &str) -> Request {
    Request {
        method: method.to_owned(),
        target: target.to_owned(),
        headers: vec![("Host".to_owned(), "127.0.0.1:5174".to_owned())],
        body: Vec::new(),
        recipe: None,
        via: None,
    }
}

#[expect(
    clippy::expect_used,
    reason = "request failures must fail the test helper immediately, never produce fallback responses"
)]
async fn answer(app: &axum::Router, request: Request) -> Response {
    tokio::time::timeout(Duration::from_secs(3), common::answer(app.clone(), request))
        .await
        .expect("request completes")
        .expect("request succeeds")
}

#[expect(
    clippy::expect_used,
    reason = "the fixture page is the contract's shape, which the kernel's page record shares"
)]
fn stored(page: WikiPageResponse) -> kernel::wiki::WikiPageResponse {
    serde_json::from_value(serde_json::to_value(page).expect("page")).expect("kernel page")
}

#[expect(
    clippy::expect_used,
    reason = "the page fixture uses a constant calendar date and instant"
)]
fn page() -> WikiPageResponse {
    WikiPageResponse {
        slug: "release".to_owned(),
        title: "Release <guide> & rollout".to_owned(),
        aliases: vec!["Release guide".to_owned()],
        tags: vec!["project:AcmeKit".to_owned()],
        entities: vec!["unwritten".to_owned()],
        current_state: "Ready 🚀 — use 'quotes' and $(substitutions).".to_owned(),
        article: Some(
            "# Checklist\n<script>alert('x')</script>\n[external](javascript:alert(1))".to_owned(),
        ),
        see_also: None,
        history: vec![WikiHistoryEntry {
            date: chrono::NaiveDate::from_ymd_opt(2026, 10, 1).expect("date"),
            title: "Start".to_owned(),
            body: "First release".to_owned(),
        }],
        citations: vec![WikiCitation {
            summary_id: "summary-1".to_owned(),
            date: None,
            kind: None,
            note: Some("<citation>".to_owned()),
            conversation_id: None,
            session_id: None,
            title: None,
            channel: None,
            topic_id: None,
            imported: None,
        }],
        markdown: "# Release\nReady 🚀".to_owned(),
        sources: Vec::new(),
        git_sha: Some("abc123".to_owned()),
        last_verified: None,
        updated_at: chrono::DateTime::parse_from_rfc3339("2026-10-14T00:00:00.000Z")
            .expect("instant")
            .with_timezone(&chrono::Utc),
        related: Some(vec!["quotes 'and' $(substitutions) 🚀".to_owned()]),
        topic_id: None,
    }
}

async fn seeded() -> (axum::Router, Arc<testkit::FakeMemoryStore>) {
    let (gateway, store) = common::gateway();
    store.insert_wiki_page(stored(page())).await;
    (gateway.router(false), store)
}

#[tokio::test]
async fn api_page_keeps_the_typed_store_payload() {
    let (app, store) = seeded().await;
    for path in ["/api/wiki/release", "/api/wiki/page?slug=release"] {
        let response = answer(&app, request("GET", path)).await;
        assert_eq!(response.status, 200);
        let actual: WikiPageResponse = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(actual, page());
    }
    let response = answer(&app, request("GET", "/api/wiki/release/raw")).await;
    assert_eq!(response.status, 200);
    assert!(
        response
            .headers
            .iter()
            .any(|(name, value)| name == "content-type" && value == "text/markdown; charset=utf-8")
    );
    assert_eq!(response.body, page().markdown.into_bytes());
    assert_eq!(
        store.requests().await,
        vec![
            testkit::MemoryRequest::WikiPage("release".to_owned()),
            testkit::MemoryRequest::WikiPage("release".to_owned()),
            testkit::MemoryRequest::WikiPage("release".to_owned()),
        ]
    );
    for retired in ["/api/wiki/page/release", "/api/wiki/gaps"] {
        assert_eq!(
            answer(&app, request("GET", retired)).await.status,
            404,
            "{retired}"
        );
    }
}

#[tokio::test]
async fn api_page_names_the_author_of_an_imported_citation() {
    let (gateway, store) = common::gateway();
    let mut imported = stored(page());
    let provenance = kernel::memory::Provenance {
        team_owner: None,
        author: Some("phil".to_owned()),
        origin_date: Some("2026-10-14T09:12:00Z".parse().unwrap()),
        verification: kernel::memory::Verification::Unreceived,
        checks: None,
    };
    for citation in &mut imported.citations {
        citation.imported = Some(provenance.clone());
    }
    store.insert_wiki_page(imported).await;
    let response = answer(&gateway.router(false), request("GET", "/api/wiki/release")).await;
    assert_eq!(response.status, 200);
    let body: wire::gateway::wiki::WikiPageResponse =
        serde_json::from_slice(&response.body).unwrap();
    assert_eq!(
        body.citations
            .first()
            .and_then(|citation| citation.imported.clone()),
        Some(wire::memory::Provenance {
            author: Some("phil".to_owned()),
            origin_date: Some("2026-10-14T09:12:00Z".parse().unwrap()),
            verification: wire::memory::Verification::Unreceived,
            checks: None,
        })
    );
    let native = answer(&seeded().await.0, request("GET", "/api/wiki/release")).await;
    assert!(!String::from_utf8(native.body).unwrap().contains("imported"));
}

#[tokio::test]
async fn list_search_filters_and_pagination_reach_the_port() {
    let (app, store) = seeded().await;
    for path in [
        "/api/wiki",
        "/api/wiki/topics",
        "/api/wiki/search?q=release&tag=project%3AAcmeKit",
    ] {
        let response = answer(&app, request("GET", path)).await;
        assert_eq!(response.status, 200);
        let actual: WikiTopicList = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(actual.total, 1);
        assert_eq!(actual.topics.len(), 1);
        assert_eq!(actual.topics.first().unwrap().slug, "release");
        assert_eq!(actual.topics.first().unwrap().excerpt, page().current_state);
    }
    let response = answer(&app, request("GET", "/api/wiki/topics?limit=999&offset=1")).await;
    let actual: WikiTopicList = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(
        actual,
        WikiTopicList {
            total: 1,
            topics: Vec::new()
        }
    );
    assert_eq!(
        store.requests().await.last(),
        Some(&testkit::MemoryRequest::WikiTopics(
            kernel::wiki::WikiFilter {
                query: None,
                tag: None,
                limit: 500,
                offset: 1,
            }
        ))
    );
}

#[tokio::test]
async fn wrong_query_types_and_unsafe_slugs_are_refused_before_store_access() {
    let (app, store) = seeded().await;
    for path in [
        "/api/wiki?limit=no",
        "/api/wiki?limit=-1",
        "/api/wiki?offset=1.5",
        "/api/wiki/page?slug=..",
        "/api/wiki/page?slug=../sibling",
        "/api/wiki/page?slug=",
        "/api/wiki/%2E%2E",
        "/api/wiki/%2Fetc",
        "/api/wiki/a%5Cb",
        "/api/wiki/%00",
        "/api/wiki/%2E%2E/raw",
    ] {
        let response = answer(&app, request("GET", path)).await;
        assert_eq!(response.status, 400, "{path}");
    }
    assert_eq!(store.requests().await.len(), 0);
}

#[tokio::test]
async fn redirects_are_local_and_preserve_ordinary_slug_characters() {
    let (app, store) = seeded().await;
    let mut target = page();
    target.slug = "quotes 'and' $(substitutions) 🚀".to_owned();
    store.insert_wiki_page(stored(target.clone())).await;
    store
        .insert_wiki_redirect("old".to_owned(), target.slug.clone())
        .await;
    for prefix in ["/wiki/", "/api/wiki/"] {
        let response = answer(&app, request("GET", &format!("{prefix}old"))).await;
        assert_eq!(response.status, 308);
        let location = &response
            .headers
            .iter()
            .find(|(name, _)| name == "location")
            .unwrap()
            .1;
        assert!(location.starts_with(prefix));
        let followed = answer(&app, request("GET", location)).await;
        assert_eq!(followed.status, 200);
        if prefix == "/api/wiki/" {
            let actual: WikiPageResponse = serde_json::from_slice(&followed.body).unwrap();
            assert_eq!(actual, target);
        }
    }
    for malicious in [
        "//evil.example",
        "../sibling",
        "https://evil.example",
        "a\r\nx: y",
        "old",
    ] {
        store
            .insert_wiki_redirect("old".to_owned(), malicious.to_owned())
            .await;
        let response = answer(&app, request("GET", "/wiki/old")).await;
        assert_eq!(response.status, 500);
        assert!(!response.headers.iter().any(|(name, _)| name == "location"));
    }
}

#[tokio::test]
async fn missing_pages_and_gaps_have_typed_responses() {
    let (app, _) = seeded().await;
    let response = answer(&app, request("GET", "/api/wiki/guide")).await;
    assert_eq!(response.status, 404);
    let actual: WikiMissBody = serde_json::from_slice(&response.body).unwrap();
    assert_eq!(actual.suggestions.len(), 1);
    assert_eq!(actual.suggestions.first().unwrap().slug, "release");
    let response = answer(&app, request("GET", "/api/wiki/_gaps")).await;
    assert_eq!(response.status, 200);
    let actual: WikiGapsResponse = serde_json::from_slice(&response.body).unwrap();
    let stalest = actual.stalest.first().unwrap();
    assert_eq!(actual.stalest.len(), 1);
    assert_eq!(
        (stalest.slug.as_str(), stalest.title.as_str()),
        ("release", "Release <guide> & rollout")
    );
    assert_eq!(actual.red_links.first().unwrap().entity, "unwritten");
    assert_eq!(actual.red_links.first().unwrap().referenced_by, ["release"]);
}

#[tokio::test]
async fn html_escapes_stored_content_and_search_text_and_head_has_no_body() {
    let (app, _) = seeded().await;
    for path in ["/wiki", "/wiki/", "/wiki/release", "/wiki/missing"] {
        let response = answer(&app, request("GET", path)).await;
        assert_eq!(
            response.status,
            if path == "/wiki/missing" { 404 } else { 200 }
        );
        assert!(
            response
                .headers
                .iter()
                .any(|(name, value)| name == "content-type" && value == "text/html; charset=utf-8")
        );
        assert!(
            response
                .headers
                .iter()
                .any(|(name, value)| name == "content-security-policy"
                    && value.contains("default-src 'none'"))
        );
        let body = String::from_utf8(response.body).unwrap();
        assert!(body.contains("RivetHub Wiki"));
        assert!(!body.contains("<script>"));
        if path == "/wiki/release" {
            assert!(body.contains("&lt;script&gt;"));
            assert!(body.contains("Ready 🚀"));
            assert!(body.contains("&lt;citation&gt;"));
            assert!(body.contains("Release &lt;guide&gt; &amp; rollout"));
        }
        let head = answer(&app, request("HEAD", path)).await;
        assert_eq!(head.status, response.status);
        assert_eq!(head.body.len(), 0);
    }
    let response = answer(&app, request("GET", "/wiki?q=%22%3E%3Cscript%3E")).await;
    let body = String::from_utf8(response.body).unwrap();
    assert!(body.contains("&quot;&gt;&lt;script&gt;"));
    assert!(!body.contains("<script>"));
}

#[tokio::test]
async fn auth_origin_preflight_and_methods_use_existing_gateway_policy() {
    let (app, store) = seeded().await;
    for path in ["/api/wiki/topics", "/api/wiki/release", "/wiki/release"] {
        let mut remote = request("GET", path);
        remote.via = Some("remote".to_owned());
        remote
            .headers
            .push(("x-rivetos-user".to_owned(), "owner".to_owned()));
        assert_eq!(answer(&app, remote).await.status, 401);
        let mut hostile = request("GET", path);
        hostile
            .headers
            .push(("Origin".to_owned(), "https://evil.example".to_owned()));
        assert_eq!(answer(&app, hostile).await.status, 403);
        assert_eq!(answer(&app, request("POST", path)).await.status, 405);
        assert_eq!(answer(&app, request("OPTIONS", path)).await.status, 200);
    }
    assert_eq!(store.requests().await.len(), 0);
    let mut legitimate = request("GET", "/api/wiki");
    legitimate
        .headers
        .push(("Origin".to_owned(), "app://bundle".to_owned()));
    assert_eq!(answer(&app, legitimate).await.status, 200);
}

#[tokio::test]
async fn store_failures_never_become_empty_successes() {
    let app = common::failing_gateway().router(false);
    for path in [
        "/api/wiki/topics",
        "/api/wiki/_gaps",
        "/api/wiki/release",
        "/wiki",
        "/wiki/release",
    ] {
        let response = answer(&app, request("GET", path)).await;
        assert_eq!(response.status, 503);
        let error: GatewayError = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(error, GatewayError::new("memory store unavailable"));
    }
}
