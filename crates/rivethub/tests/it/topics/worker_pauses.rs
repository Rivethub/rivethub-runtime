use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use gateway::Gateway;
use gateway::tenancy::{Tenancy, UserId};
use kernel::memory::MemoryStore;
use kernel::queue::{FixedJob, Job, JobKeyMode, JobName, JobQueue as _};
use memory_postgres::{PostgresMemory, PostgresQueue};
use tower::ServiceExt;
use wire::gateway::worker_pauses::{PausableRole, WorkerPause, WorkerPauses};

use super::{TestResult, settings};

const OWNER: &str = "desk";
const OTHER: &str = "win-other";

struct Hub {
    router: axum::Router,
    _db: crate::pgdb::TestDb,
    _root: tempfile::TempDir,
}

async fn hub(pauses: bool) -> TestResult<Hub> {
    let db = crate::pgdb::TestDb::migrated().await?;
    testkit::graphile_schema::install_graphile_worker(&db.pool()).await?;
    let queue = PostgresQueue::connect(db.pool()).await?;
    for key in ["e1", "e2"] {
        queue
            .enqueue(Job {
                name: JobName::Fixed(FixedJob::ExtractEntities),
                payload: wire::JsonObject::default(),
                key: Some(key.to_owned()),
                key_mode: JobKeyMode::Replace,
                priority: 0,
                run_at: None,
                max_attempts: Some(5),
            })
            .await?;
    }
    let root = tempfile::tempdir()?;
    let memory = Arc::new(PostgresMemory::new(db.pool()));
    let store: Arc<dyn MemoryStore> = memory.clone();
    let owner = UserId::new("owner".to_owned())?;
    let tenancy = Tenancy::local(owner.clone())
        .with_token(OWNER, owner)?
        .with_token(OTHER, UserId::new("other".to_owned())?)?;
    let gateway = Gateway::new(store, settings(root.path()))
        .with_tenancy(tenancy)
        .with_tidy(memory.clone(), memory.clone());
    let gateway = if pauses {
        gateway.with_worker_pauses(memory)
    } else {
        gateway
    };
    Ok(Hub {
        router: Arc::new(gateway).router(true),
        _db: db,
        _root: root,
    })
}

async fn call(
    hub: &Hub,
    method: &str,
    uri: &str,
    device: &str,
    body: Option<&str>,
) -> TestResult<(StatusCode, Vec<u8>)> {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:5976")
        .header(header::AUTHORIZATION, format!("Bearer {device}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(body.map_or_else(Body::empty, |text| Body::from(text.to_owned())))?;
    request
        .extensions_mut()
        .insert(ConnectInfo("192.0.2.7:41000".parse::<SocketAddr>()?));
    let response = hub.router.clone().oneshot(request).await?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await?;
    Ok((status, bytes.to_vec()))
}

async fn put(hub: &Hub, role: &str, paused: bool) -> TestResult<(StatusCode, Vec<u8>)> {
    call(
        hub,
        "PUT",
        &format!("/api/memory/workers/{role}/pause"),
        OWNER,
        Some(&format!("{{\"paused\":{paused}}}")),
    )
    .await
}

#[tokio::test]
async fn the_owner_pauses_and_resumes_a_role_and_repeats_change_nothing() -> TestResult {
    let hub = hub(true).await?;
    let (status, bytes) = call(&hub, "GET", "/api/memory/workers/pauses", OWNER, None).await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let listed: WorkerPauses = serde_json::from_slice(&bytes)?;
    let rows: Vec<_> = listed
        .roles
        .iter()
        .map(|row| (row.role, row.paused, row.queued))
        .collect();
    assert_eq!(
        rows,
        [
            (PausableRole::Categorizer, false, 0),
            (PausableRole::WikiBuilder, false, 0),
            (PausableRole::EntityBuilder, false, 2),
        ]
    );

    let (status, bytes) = put(&hub, "entity-builder", true).await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let first: WorkerPause = serde_json::from_slice(&bytes)?;
    assert!(first.paused);
    assert_eq!(first.paused_by.as_deref(), Some("owner"));
    assert_eq!(first.queued, 2);
    let (_, bytes) = put(&hub, "entity-builder", true).await?;
    let again: WorkerPause = serde_json::from_slice(&bytes)?;
    assert_eq!(again, first);

    let (_, bytes) = call(&hub, "GET", "/api/memory/workers/pauses", OWNER, None).await?;
    let listed: WorkerPauses = serde_json::from_slice(&bytes)?;
    let paused: Vec<_> = listed
        .roles
        .iter()
        .filter(|row| row.paused)
        .map(|row| (row.role, row.paused_at))
        .collect();
    assert_eq!(paused, [(PausableRole::EntityBuilder, first.paused_at)]);

    for _ in 0..2 {
        let (status, bytes) = put(&hub, "entity-builder", false).await?;
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let resumed: WorkerPause = serde_json::from_slice(&bytes)?;
        assert!(!resumed.paused);
        assert_eq!(resumed.paused_at, None);
        assert!(!String::from_utf8(bytes)?.contains("pausedAt"));
    }
    Ok(())
}

#[tokio::test]
async fn only_the_owner_reads_or_changes_pauses() -> TestResult {
    let hub = hub(true).await?;
    let (status, _) = call(&hub, "GET", "/api/memory/workers/pauses", OTHER, None).await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &hub,
        "PUT",
        "/api/memory/workers/wiki-builder/pause",
        OTHER,
        Some("{\"paused\":true}"),
    )
    .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (_, bytes) = call(&hub, "GET", "/api/memory/workers/pauses", OWNER, None).await?;
    let listed: WorkerPauses = serde_json::from_slice(&bytes)?;
    assert!(listed.roles.iter().all(|row| !row.paused));
    Ok(())
}

#[tokio::test]
async fn a_role_that_cannot_pause_and_a_bad_body_are_refused() -> TestResult {
    let hub = hub(true).await?;
    let (status, bytes) = put(&hub, "summarizer", true).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        String::from_utf8(bytes)?
            .contains("summarizer cannot be paused; the pausable roles are categorizer, wiki-builder, entity-builder")
    );
    let (status, _) = call(
        &hub,
        "PUT",
        "/api/memory/workers/categorizer/pause",
        OWNER,
        Some("{\"paused\":\"yes\"}"),
    )
    .await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &hub,
        "POST",
        "/api/memory/workers/categorizer/pause",
        OWNER,
        None,
    )
    .await?;
    assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    Ok(())
}

#[tokio::test]
async fn a_hub_without_pauses_says_so() -> TestResult {
    let hub = hub(false).await?;
    let (status, bytes) = call(&hub, "GET", "/api/memory/workers/pauses", OWNER, None).await?;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(String::from_utf8(bytes)?.contains("pausing workers needs a Postgres hub"));
    Ok(())
}
