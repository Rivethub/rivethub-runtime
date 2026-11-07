use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use chrono::Duration;
use chrono::Utc;
use gateway::Gateway;
use gateway::tenancy::{Tenancy, UserId};
use kernel::harness_setup::CaptureActivity;
use kernel::harness_setup::CaptureMethod;
use kernel::harness_setup::ConnectOutcome;
use kernel::harness_setup::ConnectPart;
use kernel::harness_setup::HarnessProbe;
use kernel::harness_setup::HarnessSetup;
use kernel::harness_setup::RecallState;
use kernel::harness_setup::RecallStatus;
use kernel::harness_setup::SetupError;
use parking_lot::Mutex;
use serde::Deserialize;
use tower::ServiceExt as _;
use wire::gateway::GatewayError;

use crate::common;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

#[derive(Debug, Default)]
struct ScriptedSetup {
    connects: Mutex<Vec<(String, ConnectPart, bool)>>,
}

fn probe(id: &str, installed: bool) -> HarnessProbe {
    HarnessProbe {
        id: id.to_owned(),
        name: id.to_uppercase(),
        session_prefix: id.to_owned(),
        found: true,
        path: Some(format!("/usr/bin/{id}")),
        version: None,
        method: CaptureMethod::Hook,
        capture_installed: installed,
        install_detail: if installed {
            "hook installed".to_owned()
        } else {
            "no rh hook".to_owned()
        },
        recall: RecallStatus {
            state: RecallState::NotConnected,
            detail: "no rh mcp entry".to_owned(),
        },
        can_connect: !installed,
    }
}

#[async_trait]
impl HarnessSetup for ScriptedSetup {
    async fn probe(&self) -> Result<Vec<HarnessProbe>, SetupError> {
        Ok(vec![
            probe("claude-code", true),
            probe("codex", true),
            probe("cursor", false),
        ])
    }

    async fn connect(
        &self,
        id: &str,
        part: ConnectPart,
        dry_run: bool,
    ) -> Result<ConnectOutcome, SetupError> {
        if id == "nope" {
            return Err(SetupError::UnknownHarness(id.to_owned()));
        }
        self.connects.lock().push((id.to_owned(), part, dry_run));
        Ok(ConnectOutcome {
            diff: "--- a/x\n+++ b/x\n".to_owned(),
            backup_path: if dry_run {
                String::new()
            } else {
                "/home/u/.rivethub/backups/x".to_owned()
            },
        })
    }
}

struct Node {
    app: Router,
    setup: Arc<ScriptedSetup>,
}

async fn node(root: &std::path::Path) -> Result<Node, Box<dyn std::error::Error + Send + Sync>> {
    let owner = UserId::new("owner".to_owned())?;
    let tenancy = Tenancy::local(owner.clone())
        .with_token("desk", owner)?
        .with_token("other", UserId::new("other".to_owned())?)?;
    let store = Arc::new(testkit::FakeMemoryStore::default());
    let now = Utc::now();
    store
        .script_capture_activity(vec![
            CaptureActivity {
                key_prefix: "claude-code".to_owned(),
                last_at: now - Duration::hours(1),
            },
            CaptureActivity {
                key_prefix: "claude-code".to_owned(),
                last_at: now - Duration::days(30),
            },
            CaptureActivity {
                key_prefix: "codex".to_owned(),
                last_at: now - Duration::days(10),
            },
        ])
        .await;
    let mut settings = common::settings();
    settings.state_dir = root.join("state");
    let setup = Arc::new(ScriptedSetup::default());
    let gateway = Gateway::new(store, settings)
        .with_tenancy(tenancy)
        .with_harness_setup(setup.clone());
    Ok(Node {
        app: Arc::new(gateway).router(true),
        setup,
    })
}

async fn send(
    app: &Router,
    method: Method,
    path: &str,
    body: &str,
    remote: &str,
    device: &str,
) -> Result<(StatusCode, Vec<u8>), Box<dyn std::error::Error + Send + Sync>> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:5174")
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))?;
    request
        .extensions_mut()
        .insert(ConnectInfo(remote.parse::<SocketAddr>()?));
    request.headers_mut().insert(
        axum::http::header::AUTHORIZATION,
        axum::http::HeaderValue::from_str(&format!("Bearer {}", device.to_owned()))?,
    );
    let response = app.clone().oneshot(request).await?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await?;
    Ok((status, bytes.to_vec()))
}

const LOOPBACK: &str = "127.0.0.1:41000";
const LAN: &str = "192.0.2.7:41000";

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Row {
    id: String,
    name: String,
    found: bool,
    path: Option<String>,
    capture: CaptureRow,
    recall: RecallRow,
    can_connect: bool,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CaptureRow {
    method: String,
    state: String,
    last_capture_at: Option<String>,
    detail: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RecallRow {
    state: String,
    detail: String,
}

#[tokio::test]
async fn status_lists_each_harness_with_the_state_the_capture_history_gives() -> TestResult {
    let root = tempfile::tempdir()?;
    let node = node(root.path()).await?;
    let (status, body) = send(
        &node.app,
        Method::GET,
        "/api/capture/status",
        "",
        LOOPBACK,
        "desk",
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    let rows: Vec<Row> = serde_json::from_slice(&body)?;
    let seen: Vec<_> = rows
        .iter()
        .map(|row| {
            (
                row.id.as_str(),
                row.capture.method.as_str(),
                row.capture.state.as_str(),
                row.capture.last_capture_at.is_some(),
                row.can_connect,
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("claude-code", "hook", "receiving", true, false),
            ("codex", "hook", "stale", true, false),
            ("cursor", "hook", "not-connected", false, true),
        ]
    );
    let first = rows.first().ok_or("a first row")?;
    assert_eq!(first.name, "CLAUDE-CODE");
    assert!(first.found);
    assert_eq!(first.path.as_deref(), Some("/usr/bin/claude-code"));
    assert_eq!(first.recall.state, "not-connected");
    assert_eq!(first.recall.detail, "no rh mcp entry");
    assert!(
        first
            .capture
            .detail
            .starts_with("hook installed; last capture ")
    );
    Ok(())
}

#[tokio::test]
async fn connect_returns_the_diff_and_backup_and_passes_the_dry_run_flag_on() -> TestResult {
    let root = tempfile::tempdir()?;
    let node = node(root.path()).await?;
    let path = "/api/harnesses/cursor/connect";
    let (status, body) = send(
        &node.app,
        Method::POST,
        path,
        r#"{"part":"recall"}"#,
        LOOPBACK,
        "desk",
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    let outcome: ConnectOutcome = serde_json::from_slice(&body)?;
    assert_eq!(outcome.backup_path, "/home/u/.rivethub/backups/x");
    assert!(outcome.diff.starts_with("--- a/x"));
    let (_, dry) = send(
        &node.app,
        Method::POST,
        path,
        r#"{"part":"capture","dryRun":true}"#,
        LOOPBACK,
        "desk",
    )
    .await?;
    let dry: ConnectOutcome = serde_json::from_slice(&dry)?;
    assert_eq!(dry.backup_path, "");
    assert_eq!(
        *node.setup.connects.lock(),
        vec![
            ("cursor".to_owned(), ConnectPart::Recall, false),
            ("cursor".to_owned(), ConnectPart::Capture, true)
        ]
    );
    Ok(())
}

#[tokio::test]
async fn off_loopback_callers_get_403_even_as_the_owner() -> TestResult {
    let root = tempfile::tempdir()?;
    let node = node(root.path()).await?;
    let (status, _) = send(
        &node.app,
        Method::GET,
        "/api/capture/status",
        "",
        LAN,
        "desk",
    )
    .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = send(
        &node.app,
        Method::POST,
        "/api/harnesses/cursor/connect",
        r#"{"part":"recall"}"#,
        LAN,
        "desk",
    )
    .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(node.setup.connects.lock().is_empty());
    Ok(())
}

#[tokio::test]
async fn a_loopback_caller_who_is_not_the_owner_gets_403() -> TestResult {
    let root = tempfile::tempdir()?;
    let node = node(root.path()).await?;
    let (status, body) = send(
        &node.app,
        Method::GET,
        "/api/capture/status",
        "",
        LOOPBACK,
        "other",
    )
    .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let error: GatewayError = serde_json::from_slice(&body)?;
    assert_eq!(error.code.as_deref(), Some("owner_required"));
    let (status, _) = send(
        &node.app,
        Method::POST,
        "/api/harnesses/cursor/connect",
        r#"{"part":"recall"}"#,
        LOOPBACK,
        "other",
    )
    .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(node.setup.connects.lock().is_empty());
    Ok(())
}

#[tokio::test]
async fn an_unknown_harness_is_404_and_a_bad_part_is_400() -> TestResult {
    let root = tempfile::tempdir()?;
    let node = node(root.path()).await?;
    let (status, _) = send(
        &node.app,
        Method::POST,
        "/api/harnesses/nope/connect",
        r#"{"part":"recall"}"#,
        LOOPBACK,
        "desk",
    )
    .await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &node.app,
        Method::POST,
        "/api/harnesses/cursor/connect",
        r#"{"part":"both"}"#,
        LOOPBACK,
        "desk",
    )
    .await?;
    assert!(status.is_client_error());
    Ok(())
}
