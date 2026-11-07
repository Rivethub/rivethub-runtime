use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use axum::http::StatusCode;
use axum::http::header;
use gateway::Gateway;
use gateway::Settings;
use gateway::tenancy::{Tenancy, UserId};
use kernel::memory::MemoryStore;
use memory_postgres::PostgresMemory;
use rivethub::composition::agent_presets;
use sqlx::PgPool;
use tower::ServiceExt;
use wire::gateway::GatewayError;
use wire::gateway::agents::AgentListResponse;
use wire::gateway::agents::AgentResponse;
use wire::gateway::agents::AgentStoreBackend;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const NODE: &str = "node-a";
const BODY_LIMIT: usize = 1 << 20;

fn settings(root: &Path) -> Settings {
    Settings {
        hostname: "node-a-host".to_owned(),
        node: NODE.to_owned(),
        allowed_origins: Vec::new(),
        allowed_hosts: Vec::new(),
        files_root: None,
        static_dir: None,
        root_redirect: None,
        state_dir: root.join("state"),
        directory_root: root.join("agents"),
        shared_dir: root.join("shared"),
        allowed_harnesses: None,
        devices_enabled: false,
        probe_timeout: Duration::from_millis(200),
        mesh_ca: None,
    }
}

async fn call(
    router: &axum::Router,
    method: &str,
    target: &str,
    body: &str,
) -> TestResult<(StatusCode, Vec<u8>)> {
    let mut request = Request::builder()
        .method(method)
        .uri(target)
        .header(header::HOST, "127.0.0.1:5976")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))?;
    request
        .extensions_mut()
        .insert(ConnectInfo("127.0.0.1:40000".parse::<SocketAddr>()?));
    let response = router.clone().oneshot(request).await?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), BODY_LIMIT).await?;
    Ok((status, bytes.to_vec()))
}

#[sqlx::test(migrations = false)]
async fn a_database_without_the_preset_table_keeps_the_file_store(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool);
    assert!(agent_presets(&memory).await?.is_none());
    Ok(())
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_migrated_database_serves_presets_from_postgres(pool: PgPool) -> TestResult {
    let memory = PostgresMemory::new(pool.clone());
    let presets = agent_presets(&memory)
        .await?
        .ok_or("the preset table was not selected")?;
    assert_eq!(
        presets.backend(),
        kernel::agent_presets::AgentStoreBackend::Postgres
    );
    sqlx::query(
        "INSERT INTO rh_agent_presets (id, name, node, directory, created_at)
         VALUES ('p-remote', 'Remote', 'node-b', '/home/user/remote', '2026-09-01T00:00:00Z')",
    )
    .execute(&pool)
    .await?;
    let root = tempfile::tempdir()?;
    let store: Arc<dyn MemoryStore> = Arc::new(testkit::FakeMemoryStore::default());
    let owner = Tenancy::local(UserId::new("owner".to_owned())?);
    let gateway = Arc::new(
        Gateway::new(store, settings(root.path()))
            .with_agent_presets(presets)
            .with_tenancy(owner),
    );
    let router = gateway.router(false);

    let (status, body) = call(&router, "POST", "/api/agents", r#"{"name":"Kit"}"#).await?;
    assert_eq!(
        status,
        StatusCode::CREATED,
        "{}",
        String::from_utf8_lossy(&body)
    );
    let created = serde_json::from_slice::<AgentResponse>(&body)?.agent;
    assert_eq!(created.node.as_deref(), Some(NODE));
    assert_eq!(created.node_base_url.as_deref(), Some(""));
    let directory = root.path().join("agents").join(&created.id);
    assert_eq!(created.directory.as_deref(), directory.to_str());
    assert!(directory.is_dir());
    assert_eq!(
        std::fs::read_link(directory.join("rivet-shared"))?,
        root.path().join("shared")
    );
    assert!(!root.path().join("state").join("agents.json").exists());

    let (status, body) = call(&router, "POST", "/api/agents", r#"{"name":"kit"}"#).await?;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        serde_json::from_slice::<GatewayError>(&body)?.error,
        "an agent named \"kit\" already exists"
    );

    let (status, body) = call(&router, "GET", "/api/agents", "").await?;
    assert_eq!(status, StatusCode::OK);
    let listed = serde_json::from_slice::<AgentListResponse>(&body)?;
    assert_eq!(listed.backend, Some(AgentStoreBackend::Postgres));
    assert_eq!(
        listed
            .agents
            .iter()
            .map(|agent| (agent.id.as_str(), agent.node.as_deref()))
            .collect::<Vec<_>>(),
        [
            ("p-remote", Some("node-b")),
            (created.id.as_str(), Some(NODE))
        ]
    );
    let (_, body) = call(&router, "GET", "/api/agents?node=node-b", "").await?;
    let remote = serde_json::from_slice::<AgentListResponse>(&body)?;
    assert_eq!(remote.agents.len(), 1);

    let (status, body) = call(&router, "PATCH", "/api/agents/p-remote", r#"{"model":"x"}"#).await?;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        serde_json::from_slice::<GatewayError>(&body)?
            .node
            .as_deref(),
        Some("node-b")
    );

    let target = format!("/api/agents/{}", created.id);
    let (status, body) = call(&router, "PATCH", &target, r#"{"name":"Remote"}"#).await?;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        serde_json::from_slice::<GatewayError>(&body)?.error,
        "an agent named \"Remote\" already exists"
    );
    let (status, body) = call(&router, "PATCH", &target, r#"{"node":"node-b"}"#).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        serde_json::from_slice::<GatewayError>(&body)?.error,
        "node is immutable; recreate the agent"
    );
    let (status, body) = call(&router, "PATCH", &target, r#"{"model":"opus"}"#).await?;
    assert_eq!(status, StatusCode::OK);
    let patched = serde_json::from_slice::<AgentResponse>(&body)?.agent;
    assert_eq!(patched.model, "opus");
    assert_eq!(patched.name, "Kit");
    assert!(patched.updated_at >= created.updated_at);

    let (status, _) = call(&router, "DELETE", &target, "").await?;
    assert_eq!(status, StatusCode::OK);
    assert!(directory.is_dir());
    let (status, _) = call(&router, "GET", &target, "").await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let remaining: i64 = sqlx::query_scalar("SELECT count(*) FROM rh_agent_presets")
        .fetch_one(&pool)
        .await?;
    assert_eq!(remaining, 1);
    Ok(())
}
