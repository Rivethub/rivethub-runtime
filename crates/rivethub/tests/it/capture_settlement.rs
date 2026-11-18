use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use axum::response::Response;
use gateway::Gateway;
use gateway::tenancy::{Tenancy, UserId};
use serde::de::DeserializeOwned;
use tower::ServiceExt as _;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const CLEO: &str = "cleo-capture-token";

const BATCH: &str = r#"{"session_key":"cli:owned","agent":"rivet","channel":"cli","messages":[{"event_id":"e1","role":"user","content":"hello"}]}"#;

async fn post(app: &Router, path: &str, body: &str, token: Option<&str>) -> TestResult<Response> {
    let mut request = Request::post(path)
        .header(header::HOST, "127.0.0.1:5174")
        .header(header::CONTENT_TYPE, "application/json")
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 41000))));
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    Ok(app
        .clone()
        .oneshot(request.body(Body::from(body.to_owned()))?)
        .await?)
}

async fn read_json<T: DeserializeOwned>(response: Response) -> TestResult<T> {
    let body = to_bytes(response.into_body(), 4096).await?;
    Ok(serde_json::from_slice(&body)?)
}

#[tokio::test]
async fn top_down_settle_requires_the_owner_and_persists_the_reply() -> TestResult {
    use kernel::memory::SummaryStore as _;
    use sha2::{Digest, Sha256};
    use wire::gateway::users::{ScopedToken, TokenScope, UserToken};
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("settlement.sqlite");
    migrate::sqlite(&path)?;
    let store = Arc::new(memory_sqlite::SqliteStore::open(&path)?);
    let dana = UserId::new("dana".to_owned())?;
    let drops = UserToken::Scoped(ScopedToken {
        digest: hex::encode(Sha256::digest("dana-drops-only")),
        scopes: vec![TokenScope::DropsWrite],
    });
    let tenancy = Tenancy::local(dana.clone())
        .with_token("dana-full", dana.clone())?
        .with_token(CLEO, UserId::new("cleo".to_owned())?)?
        .with_users_file_token(&drops, dana)?;
    let settings = gateway::Settings {
        hostname: "gold-host".to_owned(),
        node: "golden-node".to_owned(),
        allowed_origins: vec!["https://hub.example.test".to_owned()],
        allowed_hosts: vec!["den.example.test".to_owned()],
        static_dir: None,
        root_redirect: None,
        state_dir: dir.path().join("state"),
        private_ca: None,
        request_deadline: gateway::REQUEST_DEADLINE,
        policy: None,
    };
    let app = Arc::new(Gateway::new(store.clone(), settings).with_tenancy(tenancy)).router(false);
    let captured = post(&app, "/api/capture", BATCH, Some("dana-full")).await?;
    assert_eq!(captured.status(), 200);
    let capture: wire::capture::CaptureResult = read_json(captured).await?;
    let url = format!(
        "/api/memory/conversations/{}/settle",
        capture.conversation_id
    );
    for (token, status) in [
        (None, 401),
        (Some(CLEO), 404),
        (Some("dana-drops-only"), 403),
    ] {
        let response = post(&app, &url, "", token).await?;
        assert_eq!(response.status(), status);
    }
    let accepted = post(&app, &url, "", Some("dana-full")).await?;
    assert_eq!(accepted.status(), 202);
    let accepted: kernel::memory::settlement::SettleResult = read_json(accepted).await?;
    let id = accepted.conversation_id;
    assert_eq!(
        accepted.status,
        kernel::memory::settlement::SettleStatus::Settled
    );
    assert_eq!(accepted.input_revision, 1);
    let retry = post(&app, &url, "", Some("dana-full")).await?;
    assert_eq!(retry.status(), 202);
    let retry: kernel::memory::settlement::SettleResult = read_json(retry).await?;
    assert_eq!(
        retry.status,
        kernel::memory::settlement::SettleStatus::AlreadySettled
    );
    assert_eq!(accepted.settled_at, retry.settled_at);
    let reopened = memory_sqlite::SqliteStore::open(&path)?;
    assert!(reopened.summary_eligible(id, 1).await?);
    Ok(())
}
