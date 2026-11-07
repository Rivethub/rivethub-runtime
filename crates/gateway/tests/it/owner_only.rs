use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use gateway::Gateway;
use gateway::tenancy::{Tenancy, UserId};
use tower::ServiceExt as _;
use wire::gateway::GatewayError;

use crate::common;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

const OWNER_DEVICE: &str = "desk";
const OTHER_DEVICE: &str = "win-other";

fn node(root: &std::path::Path) -> Result<Router, Box<dyn std::error::Error + Send + Sync>> {
    let owner = UserId::new("owner".to_owned())?;
    let tenancy = Tenancy::local(owner.clone())
        .with_token(OWNER_DEVICE, owner)?
        .with_token(OTHER_DEVICE, UserId::new("other".to_owned())?)?;
    let mut settings = common::settings();
    settings.state_dir = root.join("state");
    settings.directory_root = root.join("agents");
    let gateway =
        Gateway::new(Arc::new(testkit::FakeMemoryStore::default()), settings).with_tenancy(tenancy);
    Ok(Arc::new(gateway).router(true))
}

async fn send(
    app: &Router,
    method: Method,
    path: &str,
    body: &str,
    device: &str,
) -> Result<(StatusCode, Option<String>), Box<dyn std::error::Error + Send + Sync>> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:5174")
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))?;
    request
        .extensions_mut()
        .insert(ConnectInfo("192.0.2.7:41000".parse::<SocketAddr>()?));
    request.headers_mut().insert(
        axum::http::header::AUTHORIZATION,
        axum::http::HeaderValue::from_str(&format!("Bearer {}", device.to_owned()))?,
    );
    let response = app.clone().oneshot(request).await?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await?;
    let code = serde_json::from_slice::<GatewayError>(&bytes)
        .ok()
        .and_then(|error| error.code);
    Ok((status, code))
}

#[tokio::test]
async fn only_the_owner_changes_agent_presets() -> TestResult {
    let root = tempfile::tempdir()?;
    let app = node(root.path())?;
    let preset = r#"{"name": "helper"}"#;
    let (status, code) = send(&app, Method::POST, "/api/agents", preset, OTHER_DEVICE).await?;
    assert_eq!(
        (status, code.as_deref()),
        (StatusCode::FORBIDDEN, Some("owner_required"))
    );
    let (status, _) = send(&app, Method::POST, "/api/agents", preset, OWNER_DEVICE).await?;
    assert_eq!(status, StatusCode::CREATED);
    for method in [Method::PATCH, Method::DELETE] {
        let body = if method == Method::PATCH {
            r#"{"name": "taken"}"#
        } else {
            ""
        };
        let (status, code) = send(&app, method, "/api/agents/helper", body, OTHER_DEVICE).await?;
        assert_eq!(
            (status, code.as_deref()),
            (StatusCode::FORBIDDEN, Some("owner_required"))
        );
    }
    let (status, _) = send(&app, Method::GET, "/api/agents", "", OTHER_DEVICE).await?;
    assert_eq!(status, StatusCode::OK);
    Ok(())
}
