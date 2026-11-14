use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode, header};
use gateway::tenancy::{Tenancy, UserId};
use gateway::{Gateway, ModelAssignment};
use kernel::memory::MemoryStore;
use kernel::model::{Degradation, ProviderHealth, ProviderRecovery, ProviderStatus};
use kernel::queue::{FixedJob, Job, JobKeyMode, JobName, JobQueue};
use memory_sqlite::SqliteStore;
use tokio::sync::watch;
use tower::ServiceExt;
use wire::gateway::memory_models::{ModelHealth, ModelJob, ModelRole};

use super::{TestResult, settings};

const OWNER_DEVICE: &str = "desk";
const OTHER_DEVICE: &str = "win-other";

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct Refused(&'static str);

fn assignment(
    role: ModelRole,
    provider: &str,
    model: &str,
    inherits_default: bool,
) -> ModelAssignment {
    ModelAssignment {
        role,
        provider: Some(provider.to_owned()),
        model: Some(model.to_owned()),
        inherits_default,
    }
}

fn roles() -> Vec<ModelAssignment> {
    vec![
        assignment(ModelRole::Default, "big", "big-model", false),
        assignment(ModelRole::Summarizer, "small", "small-model", false),
        assignment(ModelRole::Categorizer, "big", "big-model", true),
        assignment(ModelRole::WikiWriter, "big", "big-model", true),
        ModelAssignment {
            role: ModelRole::Embedder,
            provider: None,
            model: Some("bge-test".to_owned()),
            inherits_default: false,
        },
    ]
}

fn degraded(
    id: &str,
    recovery: ProviderRecovery,
) -> (ProviderHealth, watch::Sender<ProviderStatus>) {
    let cause: Arc<dyn std::error::Error + Send + Sync> = Arc::new(Refused("endpoint refused"));
    let (sender, status) =
        watch::channel(ProviderStatus::Degraded(Degradation { recovery, cause }));
    (
        ProviderHealth {
            id: id.to_owned(),
            status,
        },
        sender,
    )
}

struct Node {
    router: axum::Router,
    store: Arc<SqliteStore>,
    _root: tempfile::TempDir,
}

fn node(providers: Vec<ProviderHealth>) -> TestResult<Node> {
    node_with(providers, roles())
}

fn node_with(
    providers: Vec<ProviderHealth>,
    assignments: Vec<ModelAssignment>,
) -> TestResult<Node> {
    let root = tempfile::tempdir()?;
    let path = root.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let store = Arc::new(SqliteStore::open(&path)?);
    let memory: Arc<dyn MemoryStore> = store.clone();
    let owner = UserId::new("owner".to_owned())?;
    let tenancy = Tenancy::local(owner.clone())
        .with_token(OWNER_DEVICE, owner)?
        .with_token(OTHER_DEVICE, UserId::new("other".to_owned())?)?;
    let gateway = Gateway::new(memory, settings(root.path()))
        .with_tenancy(tenancy)
        .with_tidy(store.clone(), store.clone())
        .with_provider_health(providers)
        .with_model_assignments(assignments);
    Ok(Node {
        router: Arc::new(gateway).router(true),
        store,
        _root: root,
    })
}

async fn models(node: &Node, device: &str) -> TestResult<(StatusCode, Vec<u8>)> {
    let mut request = Request::builder()
        .method("GET")
        .uri("/api/memory/models")
        .header(header::HOST, "127.0.0.1:5976")
        .body(Body::empty())?;
    request
        .extensions_mut()
        .insert(ConnectInfo("192.0.2.7:41000".parse::<SocketAddr>()?));
    request.headers_mut().insert(
        axum::http::header::AUTHORIZATION,
        axum::http::HeaderValue::from_str(&format!("Bearer {}", device.to_owned()))?,
    );
    let response = node.router.clone().oneshot(request).await?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await?;
    Ok((status, bytes.to_vec()))
}

async fn queue(node: &Node, job: FixedJob, key: &str) -> TestResult {
    node.store
        .enqueue(Job {
            name: JobName::Fixed(job),
            payload: wire::JsonObject::default(),
            key: Some(key.to_owned()),
            key_mode: JobKeyMode::Replace,
            priority: 0,
            run_at: None,
            max_attempts: Some(5),
        })
        .await?;
    Ok(())
}

fn jobs(bytes: &[u8]) -> TestResult<Vec<ModelJob>> {
    Ok(serde_json::from_slice(bytes)?)
}

#[tokio::test]
async fn the_owner_reads_every_role_with_its_queue_depth() -> TestResult {
    let node = node(Vec::new())?;
    queue(&node, FixedJob::SummarizeConversation, "s1").await?;
    queue(&node, FixedJob::SummarizeConversation, "s2").await?;
    queue(&node, FixedJob::FileConversation, "f1").await?;
    let (status, bytes) = models(&node, OWNER_DEVICE).await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let text = String::from_utf8(bytes.clone())?;
    assert!(text.contains("\"inheritsDefault\":true"), "{text}");
    assert!(text.contains("\"role\":\"wiki-writer\""), "{text}");
    let listed = jobs(&bytes)?;
    let summary: Vec<_> = listed
        .iter()
        .map(|job| (job.role, job.queued, job.inherits_default, job.health))
        .collect();
    assert_eq!(
        summary,
        vec![
            (ModelRole::Default, 0, false, ModelHealth::Healthy),
            (ModelRole::Summarizer, 2, false, ModelHealth::Healthy),
            (ModelRole::Categorizer, 1, true, ModelHealth::Healthy),
            (ModelRole::WikiWriter, 0, true, ModelHealth::Healthy),
            (ModelRole::Embedder, 0, false, ModelHealth::Healthy),
        ]
    );
    let default = listed.first().ok_or("default row")?;
    assert_eq!(default.provider.as_deref(), Some("big"));
    assert_eq!(default.model.as_deref(), Some("big-model"));
    assert_eq!(default.detail, "inherited by categorizer, wiki-writer");
    let summarizer = listed.get(1).ok_or("summarizer row")?;
    assert_eq!(summarizer.detail, "2 queued");
    Ok(())
}

#[tokio::test]
async fn provider_trouble_and_failing_jobs_set_the_health() -> TestResult {
    let (small, _small_keeper) = degraded("small", ProviderRecovery::Permanent);
    let (big, _big_keeper) = degraded("big", ProviderRecovery::Retrying);
    let node = node(vec![small, big])?;
    let (_, bytes) = models(&node, OWNER_DEVICE).await?;
    let listed = jobs(&bytes)?;
    let health: Vec<_> = listed.iter().map(|job| (job.role, job.health)).collect();
    assert_eq!(
        health,
        vec![
            (ModelRole::Default, ModelHealth::Slow),
            (ModelRole::Summarizer, ModelHealth::Failing),
            (ModelRole::Categorizer, ModelHealth::Slow),
            (ModelRole::WikiWriter, ModelHealth::Slow),
            (ModelRole::Embedder, ModelHealth::Healthy),
        ]
    );
    let summarizer = listed.get(1).ok_or("summarizer row")?;
    assert!(
        summarizer
            .detail
            .contains("provider permanent: endpoint refused"),
        "{}",
        summarizer.detail
    );
    Ok(())
}

#[tokio::test]
async fn failed_jobs_degrade_the_role_that_owns_them() -> TestResult {
    let node = node(Vec::new())?;
    queue(&node, FixedJob::FileConversation, "f1").await?;
    let claimed = node
        .store
        .claim(&kernel::queue::ClaimSelector::matching(vec![
            JobName::Fixed(FixedJob::FileConversation),
        ]))
        .await?
        .ok_or("a filing job is claimed")?;
    node.store.fail(claimed.id, "model refused").await?;
    let (_, bytes) = models(&node, OWNER_DEVICE).await?;
    let listed = jobs(&bytes)?;
    let categorizer = listed.get(2).ok_or("categorizer row")?;
    assert_eq!(
        (categorizer.role, categorizer.health, categorizer.queued),
        (ModelRole::Categorizer, ModelHealth::Slow, 1)
    );
    assert_eq!(categorizer.detail, "1 failing; 1 queued");
    let summarizer = listed.get(1).ok_or("summarizer row")?;
    assert_eq!(summarizer.health, ModelHealth::Healthy);
    Ok(())
}

#[tokio::test]
async fn only_the_owner_reads_the_models() -> TestResult {
    let node = node(Vec::new())?;
    let (status, bytes) = models(&node, OTHER_DEVICE).await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        String::from_utf8_lossy(&bytes).contains("owner_required"),
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    Ok(())
}

#[tokio::test]
async fn a_node_without_a_model_reports_every_role_as_unconfigured() -> TestResult {
    let node = node_with(Vec::new(), Vec::new())?;
    let (status, bytes) = models(&node, OWNER_DEVICE).await?;
    assert_eq!(status, StatusCode::OK);
    let jobs: Vec<wire::gateway::memory_models::ModelJob> = serde_json::from_slice(&bytes)?;
    assert_eq!(jobs.len(), 5);
    assert!(jobs.iter().all(|job| job.health
        == wire::gateway::memory_models::ModelHealth::Unconfigured
        && job.provider.is_none()));
    Ok(())
}
