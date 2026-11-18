use std::sync::Arc;
use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse as _, Response};
use kernel::memory::{
    MemoryEntry, MemoryHitSource, MemorySearchResult, MemoryStore, SearchOutcome,
};
use kernel::tenancy::{Scope, Viewer};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;
use wire::gateway::memory::{MemorySearchResponse, MemoryToolResponse};
use wire::message::{Role, ToolResult};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const QUERY: &str = "kiln quartz";
const ARGUMENTS: &str = r#"{"query":"kiln quartz"}"#;
const TARGET: &str = "/api/memory/search?q=kiln%20quartz&limit=30";
const TOOL: &str = "/api/memory/tool/memory_search";

fn rerank_config(url: &str, extra: &str) -> TestResult<config::Config> {
    let yaml = format!("memory:\n  recall:\n    rerank:\n      url: {url}\n{extra}");
    Ok(config::type_document(&serde_yaml_ng::from_str(&yaml)?)?.config)
}

#[derive(Debug, Clone, Copy)]
enum Reply {
    Score,
    Status(StatusCode),
    Body(&'static str),
    Slow,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct CapturedChoice {
    kind: String,
    state: String,
    question: String,
    options: Vec<String>,
}

#[derive(Serialize)]
struct Scores {
    probabilities: Vec<f64>,
    choice_index: usize,
}

#[derive(Debug, Clone)]
struct Classifier {
    reply: Reply,
    requests: Arc<Mutex<Vec<CapturedChoice>>>,
}

async fn decide(State(stub): State<Classifier>, Json(request): Json<CapturedChoice>) -> Response {
    let count = request.options.len();
    stub.requests.lock().await.push(request);
    match stub.reply {
        Reply::Score => {
            let probabilities: Vec<_> = (0..count)
                .map(|i| {
                    if i >= count.saturating_sub(2) {
                        0.25
                    } else {
                        0.0
                    }
                })
                .collect();
            Json(Scores {
                probabilities,
                choice_index: 0,
            })
            .into_response()
        }
        Reply::Status(status) => (status, "classifier unavailable").into_response(),
        Reply::Body(body) => (StatusCode::OK, body).into_response(),
        Reply::Slow => {
            let _elapsed =
                tokio::time::timeout(Duration::from_millis(700), std::future::pending::<()>())
                    .await;
            StatusCode::OK.into_response()
        }
    }
}

async fn classifier(
    reply: Reply,
) -> TestResult<(testkit::stub::Stub, Arc<Mutex<Vec<CapturedChoice>>>)> {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let service = axum::Router::new()
        .route("/v1/decide", axum::routing::post(decide))
        .with_state(Classifier {
            reply,
            requests: requests.clone(),
        });
    Ok((testkit::stub_server(service).await?, requests))
}

fn config_at(addr: std::net::SocketAddr, extra: &str) -> TestResult<config::Config> {
    rerank_config(&format!("http://{addr}"), extra)
}

#[derive(Debug, Clone)]
struct App {
    router: axum::Router,
    owner: Arc<gateway::local_owner::LocalOwner>,
}

fn router(store: Arc<dyn MemoryStore>, config: &config::Config) -> TestResult<App> {
    let settings = gateway::Settings::from_config_in(
        config,
        &config::Environment::from_process(),
        "localhost".to_owned(),
        "search-test".to_owned(),
    )?;
    let owner = Arc::new(gateway::local_owner::LocalOwner::unpersisted()?);
    let user = gateway::tenancy::UserId::new(testkit::OWNER.to_owned())?;
    let gateway = gateway::Gateway::new(store, settings)
        .with_local_owner(owner.clone())
        .with_tenancy(gateway::tenancy::Tenancy::local(user))
        .with_search_rerank(mcp::rerank::configured(config));
    Ok(App {
        router: Arc::new(gateway).router(false),
        owner,
    })
}

async fn gateway_search(app: App, target: &str, body: Option<&str>) -> TestResult<Vec<u8>> {
    let mut request = match body {
        Some(body) => Request::post(target)
            .header("content-type", "application/json")
            .body(Body::from(body.to_owned()))?,
        None => Request::get(target).body(Body::empty())?,
    };
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], 21545));
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(addr));
    request.headers_mut().insert(
        axum::http::header::AUTHORIZATION,
        format!("Bearer {}", app.owner.current().expose()).parse()?,
    );
    request
        .headers_mut()
        .insert(axum::http::header::HOST, "localhost".parse()?);
    let response = app.router.oneshot(request).await?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 100_000).await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    Ok(bytes.to_vec())
}

fn outcome(count: u32) -> SearchOutcome {
    let results = (0..count)
        .map(|i| MemorySearchResult {
            chunk: None,
            passage: None,
            id: format!("summary-{i}"),
            source: MemoryHitSource::Summary,
            content: format!("{} {QUERY} {}", "é".repeat(350), "λ".repeat(170)),
            role: None,
            agent: None,
            relevance_score: 1.0,
            created_at: jiff::Timestamp::UNIX_EPOCH,
            session_id: None,
            title: Some(format!("  Pottery {i}\nignored second line")),
        })
        .collect();
    SearchOutcome {
        results,
        ..Default::default()
    }
}

fn ids(reply: &[u8]) -> TestResult<Vec<String>> {
    let response: MemorySearchResponse = serde_json::from_slice(reply)?;
    Ok(response.results.into_iter().map(|hit| hit.id).collect())
}

#[tokio::test]
async fn default_order_and_top_24_request_match_the_evaluated_contract() -> TestResult {
    let (stub, requests) = classifier(Reply::Score).await?;
    let store = Arc::new(testkit::FakeMemoryStore::default());
    store.set_search_outcome(outcome(30)).await;
    let baseline = router(store.clone(), &config::Config::default())?;
    let original = gateway_search(baseline.clone(), TARGET, None).await?;
    assert_eq!(gateway_search(baseline, TARGET, None).await?, original);
    assert_eq!(
        ids(&original)?,
        (0..30).map(|i| format!("summary-{i}")).collect::<Vec<_>>()
    );
    assert!(requests.lock().await.is_empty());
    let enabled = router(store, &config_at(stub.addr, "")?)?;
    let ranked = gateway_search(enabled, TARGET, None).await?;
    let expected = [22, 23]
        .into_iter()
        .chain(0..22)
        .chain(24..30)
        .map(|i| format!("summary-{i}"))
        .collect::<Vec<_>>();
    assert_eq!(ids(&ranked)?, expected);
    assert_request(&requests.lock().await, 24);
    stub.close().await?;
    Ok(())
}

fn assert_request(requests: &[CapturedChoice], count: u32) {
    let options: Vec<_> = (0..count)
        .map(|i| format!("Pottery {i}\n{} {QUERY}", "é".repeat(288)))
        .collect();
    assert_eq!(
        requests,
        [CapturedChoice {
            kind: "choice".to_owned(),
            state: format!(
                "A person searching a memory of past working sessions asked this question:\n{QUERY}"
            ),
            question: "Which passage is most likely to contain the answer?".to_owned(),
            options,
        }]
    );
}

#[tokio::test]
async fn too_few_hits_and_explicit_search_modes_skip_the_classifier() -> TestResult {
    let (stub, requests) = classifier(Reply::Score).await?;
    let store = Arc::new(testkit::FakeMemoryStore::default());
    let app = router(store.clone(), &config_at(stub.addr, "")?)?;
    for count in [0, 1] {
        store.set_search_outcome(outcome(count)).await;
        assert_eq!(
            ids(&gateway_search(app.clone(), TARGET, None).await?)?.len(),
            usize::try_from(count)?
        );
    }
    store.set_search_outcome(outcome(4)).await;
    for mode in ["regex", "fts", "trigram", "vector"] {
        let result = gateway_search(app.clone(), &format!("{TARGET}&mode={mode}"), None).await?;
        assert_eq!(ids(&result)?.first().map(String::as_str), Some("summary-0"));
    }
    assert!(requests.lock().await.is_empty());
    let text: MemoryToolResponse =
        serde_json::from_slice(&gateway_search(app, TOOL, Some(ARGUMENTS)).await?)?;
    let ToolResult::Text(text) = text.result else {
        return Err("expected tool result text".into());
    };
    assert_eq!(
        ids(text.as_bytes())?.first().map(String::as_str),
        Some("summary-2")
    );
    assert_request(&requests.lock().await, 4);
    stub.close().await?;
    Ok(())
}

#[tokio::test]
async fn decide_failures_return_original_bytes_without_retries() -> TestResult {
    let store = Arc::new(testkit::FakeMemoryStore::default());
    store.set_search_outcome(outcome(2)).await;
    let expected = gateway_search(
        router(store.clone(), &config::Config::default())?,
        TARGET,
        None,
    )
    .await?;
    for reply in [
        Reply::Status(StatusCode::SERVICE_UNAVAILABLE),
        Reply::Status(StatusCode::TEMPORARY_REDIRECT),
        Reply::Body("{oops"),
        Reply::Body(r#"{"probabilities":[1]}"#),
        Reply::Body(r#"{"probabilities":[1e999,0]}"#),
        Reply::Body(r#"{"probabilities":[-1,0]}"#),
        Reply::Slow,
    ] {
        let (stub, requests) = classifier(reply).await?;
        assert_failure(store.clone(), stub.addr, &expected).await?;
        assert_request(&requests.lock().await, 2);
        stub.close().await?;
    }
    let refused = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let addr = refused.local_addr()?;
    drop(refused);
    assert_failure(store, addr, &expected).await
}

async fn assert_failure(
    store: Arc<dyn MemoryStore>,
    addr: std::net::SocketAddr,
    expected: &[u8],
) -> TestResult {
    let app = router(store, &config_at(addr, "      timeout_ms: 150\n")?)?;
    let actual = tokio::time::timeout(
        Duration::from_millis(600),
        gateway_search(app, TARGET, None),
    )
    .await??;
    assert_eq!(actual, expected);
    Ok(())
}

#[tokio::test]
async fn rerank_runs_on_the_sqlite_mcp_path() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("search.sqlite");
    migrate::sqlite(&path)?;
    let store = Arc::new(memory_sqlite::SqliteStore::open(&path)?);
    assert_mcp_path(store).await
}

#[tokio::test]
async fn rerank_runs_on_the_postgres_mcp_path() -> TestResult {
    let db = crate::pgdb::TestDb::migrated().await?;
    let pool = memory_postgres::scoped(sqlx::postgres::PgPoolOptions::new())
        .connect(&db.served_url().await?)
        .await?;
    let store = Arc::new(memory_postgres::PostgresMemory::new(pool));
    let owner = testkit::owner()?;
    Scope::Viewer(Viewer::new(owner, Vec::new()))
        .run(assert_mcp_path(store.clone()))
        .await?;
    store.close().await?;
    Ok(())
}

async fn assert_mcp_path(store: Arc<dyn MemoryStore>) -> TestResult {
    let (stub, requests) = classifier(Reply::Score).await?;
    store.append(&testkit::owner()?, entries()).await?;
    let ordinary = mcp::Server::new(store.clone(), CancellationToken::new())?;
    let before = mcp_search(&ordinary).await?;
    let enabled = mcp::Server::builder(store, CancellationToken::new())
        .search_rerank(&config_at(stub.addr, "      top: 3\n      chars: 200\n")?)
        .build()?;
    let after = mcp_search(&enabled).await?;
    let original = message_order(&before);
    let [first, second, third] = original.as_slice() else {
        return Err(format!("expected three hits: {before}").into());
    };
    assert_eq!(message_order(&after), [*second, *third, *first]);
    let captured = requests.lock().await;
    let [request] = captured.as_slice() else {
        return Err("expected one decide request".into());
    };
    assert_eq!(request.options.len(), 3);
    for (option, label) in request.options.iter().zip(original) {
        assert!(option.contains(label), "{option}");
    }
    drop(captured);
    stub.close().await?;
    Ok(())
}

fn entries() -> Vec<MemoryEntry> {
    ["etched", "glazed", "fired"]
        .into_iter()
        .map(|label| MemoryEntry {
            provenance: None,
            id: None,
            session_id: "test:pottery".to_owned(),
            agent: "searcher".to_owned(),
            channel: "test".to_owned(),
            role: Role::User,
            content: format!(
                "{QUERY} {label} samples awaited careful inspection on the studio shelf."
            ),
            tool_name: None,
            tool_args: None,
            tool_result: None,
            metadata: None,
            created_at: None,
        })
        .collect()
}

async fn mcp_search(server: &mcp::Server) -> TestResult<String> {
    let mut request = rmcp::model::CallToolRequestParams::new("memory_search");
    request.arguments = Some(serde_json::from_str(ARGUMENTS)?);
    let result = server.call(request).await?;
    assert_ne!(result.is_error, Some(true), "{result:?}");
    Ok(result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.as_str()))
        .collect())
}

fn message_order(text: &str) -> Vec<&str> {
    text.lines()
        .filter_map(|line| line.strip_prefix("kiln quartz "))
        .collect()
}
