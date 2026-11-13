use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Method;
use axum::http::Request;
use axum::http::StatusCode;
use kernel::memory::MemoryStore;
use kernel::memory::share::receive::*;
use parking_lot::Mutex;
use testkit::FakeMemoryStore;
use tower::ServiceExt as _;
use wire::gateway::share_receive as body;

use crate::common;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;

const SECRET_CODE: &str = "rhb1:AAAAAAAAAAAAAAAAAAAAAA#AGE-SECRET-KEY-canary0123456789";

#[derive(Debug, Default)]
struct Script {
    inspect: Mutex<Vec<Result<Inspected, ReceiveError>>>,
    import: Mutex<Vec<Result<Imported, ReceiveError>>>,
    codes: Mutex<Vec<String>>,
    imports: Mutex<Vec<ImportRequest>>,
    senders: Mutex<Vec<Sender>>,
}

#[derive(Debug)]
struct Fake(Arc<Script>);

#[async_trait]
impl ReceiveDesk for Fake {
    async fn inspect(&self, code: ReceiveCode) -> Result<Inspected, ReceiveError> {
        self.0.codes.lock().push(code.reveal().to_owned());
        self.0.inspect.lock().remove(0)
    }

    async fn import(
        &self,
        code: ReceiveCode,
        request: ImportRequest,
    ) -> Result<Imported, ReceiveError> {
        self.0.codes.lock().push(code.reveal().to_owned());
        self.0.imports.lock().push(request);
        self.0.import.lock().remove(0)
    }

    async fn trusted(&self) -> Result<Vec<Sender>, ReceiveError> {
        Ok(self.0.senders.lock().clone())
    }

    async fn trust(&self, name: String, key: String) -> Result<Sender, ReceiveError> {
        let sender = Sender {
            name,
            fingerprint: format!("fp-{key}"),
            key,
        };
        self.0.senders.lock().push(sender.clone());
        Ok(sender)
    }

    async fn untrust(&self, name: String, _key: Option<String>) -> Result<usize, ReceiveError> {
        let mut senders = self.0.senders.lock();
        let before = senders.len();
        senders.retain(|sender| sender.name != name);
        Ok(before - senders.len())
    }
}

fn rig() -> (axum::Router, Arc<Script>) {
    let script = Arc::new(Script::default());
    let store: Arc<dyn MemoryStore> = Arc::new(FakeMemoryStore::default());
    let gateway = common::owned(gateway::Gateway::new(store, common::settings()))
        .with_receive_desk(Arc::new(Fake(Arc::clone(&script))));
    (Arc::new(gateway).router(false), script)
}

async fn call(
    app: &axum::Router,
    method: Method,
    path: &str,
    json: &str,
) -> Result<(StatusCode, String), Box<dyn std::error::Error + Send + Sync>> {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "127.0.0.1:5174")
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {}", common::owner_token()))
        .body(Body::from(json.to_owned()))?;
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:41000".parse::<std::net::SocketAddr>()?,
    ));
    let response = app.clone().oneshot(request).await?;
    let status = response.status();
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|value| value.to_str().ok()),
        Some("no-store"),
        "{path}"
    );
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await?;
    Ok((status, String::from_utf8(bytes.to_vec())?))
}

fn inspected(findings: usize, secret: bool) -> Inspected {
    Inspected {
        bundle: "AAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        server: "rivethub.cloud".to_owned(),
        downloaded: true,
        author: "alex".to_owned(),
        node: "desk".to_owned(),
        verification: Verification::UnknownKey,
        reason: "unverified: signed by ed25519:abc".to_owned(),
        signer: Some(Signer {
            key: "ed25519:abcdef".to_owned(),
            fingerprint: "abc".to_owned(),
        }),
        sessions: vec![Inbound {
            number: 1,
            author: Some("phil".to_owned()),
            title: "fix the reader".to_owned(),
            agent: "rivet".to_owned(),
            messages: 4,
            created_at: None,
            updated_at: None,
            findings,
            rows: Vec::new(),
            secret,
        }],
        author_findings: 0,
        author_rows: Vec::new(),
        summary: String::new(),
    }
}

fn imported() -> Imported {
    Imported {
        bundle: "AAAAAAAAAAAAAAAAAAAAAA".to_owned(),
        received_at: chrono::Utc::now(),
        author: "alex".to_owned(),
        verification: Verification::Unsigned,
        signer: None,
        sessions: vec![ImportedSession {
            number: 1,
            key: "import:abc".to_owned(),
            verification: Verification::Unsigned,
            verdict: Verdict::Clean,
            inserted: 4,
            skipped: 0,
        }],
        skipped: vec![2],
    }
}

fn request(json: &str) -> String {
    json.replace("CODE", SECRET_CODE)
}

#[tokio::test]
async fn the_share_code_travels_only_in_the_body_and_never_comes_back() -> TestResult {
    let (app, script) = rig();
    script.inspect.lock().push(Ok(inspected(0, false)));
    let (status, text) = call(
        &app,
        Method::POST,
        "/api/share/receive?code=ignored",
        &request(r#"{"code":"CODE"}"#),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(*script.codes.lock(), vec![SECRET_CODE.to_owned()]);
    let report: body::ReceiveReport = serde_json::from_str(&text)?;
    assert_eq!(
        report.verification,
        wire::capture::ReceiptVerification::UnknownKey
    );
    assert!(
        !text.contains("canary"),
        "the report must not carry the code: {text}"
    );

    for json in [
        r#"{"code":"CODE","extra":"CODE"}"#,
        r#"{"code":["CODE"]}"#,
        r#"{"codes":"CODE"}"#,
        r#"["CODE"#,
    ] {
        let (status, text) = call(&app, Method::POST, "/api/share/receive", &request(json)).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert!(!text.contains("canary"), "{json}: {text}");
    }
    assert_eq!(
        script.codes.lock().len(),
        1,
        "a bad body never reaches the desk"
    );
    for method in [Method::GET, Method::PUT, Method::DELETE] {
        let (status, _) = call(&app, method, "/api/share/receive", "").await?;
        assert_eq!(status, StatusCode::METHOD_NOT_ALLOWED);
    }
    Ok(())
}

#[tokio::test]
async fn the_report_offers_each_session_only_the_decisions_it_may_take() -> TestResult {
    let (app, script) = rig();
    for (findings, secret, offered) in [
        (0, false, vec!["Import", "Skip"]),
        (2, false, vec!["Import", "Skip", "HoldSpans"]),
        (2, true, vec!["Skip", "HoldSpans"]),
    ] {
        script.inspect.lock().push(Ok(inspected(findings, secret)));
        let (status, text) = call(
            &app,
            Method::POST,
            "/api/share/receive",
            &request(r#"{"code":"CODE"}"#),
        )
        .await?;
        assert_eq!(status, StatusCode::OK, "{text}");
        let report: body::ReceiveReport = serde_json::from_str(&text)?;
        let allowed: Vec<String> = report
            .sessions
            .first()
            .map(|session| session.allowed.iter().map(|d| format!("{d:?}")).collect())
            .unwrap_or_default();
        assert_eq!(allowed, offered);
        assert_eq!(report.held, findings > 0);
    }
    Ok(())
}

#[tokio::test]
async fn import_honours_each_decision() -> TestResult {
    let (app, script) = rig();
    script.import.lock().push(Ok(imported()));
    let (status, text) = call(
        &app,
        Method::POST,
        "/api/share/receive/import",
        &request(
            r#"{"code":"CODE","sessions":[{"n":1,"decision":"import"},{"n":2,"decision":"skip"},{"n":3,"decision":"hold_spans"}]}"#,
        ),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{text}");
    let reply: body::ImportReply = serde_json::from_str(&text)?;
    assert_eq!(reply.skipped, vec![2]);
    assert_eq!(reply.imported.len(), 1);
    assert!(!text.contains("canary"), "{text}");
    assert_eq!(
        script.imports.lock().clone(),
        vec![ImportRequest {
            sessions: vec![
                SessionChoice {
                    number: 1,
                    choice: Choice::Import
                },
                SessionChoice {
                    number: 2,
                    choice: Choice::Skip
                },
                SessionChoice {
                    number: 3,
                    choice: Choice::HoldSpans
                },
            ],
            force: false,
            fresh_login: true,
        }]
    );

    for json in [
        r#"{"code":"CODE","sessions":[{"n":0,"decision":"skip"}]}"#,
        r#"{"code":"CODE","sessions":[{"n":1,"decision":"skip"},{"n":1,"decision":"import"}]}"#,
        r#"{"code":"CODE","sessions":[{"n":1,"decision":"CODE"}]}"#,
    ] {
        let (status, text) = call(
            &app,
            Method::POST,
            "/api/share/receive/import",
            &request(json),
        )
        .await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{json}");
        assert!(!text.contains("canary"), "{json}: {text}");
    }
    assert_eq!(
        script.imports.lock().len(),
        1,
        "a bad decision never reaches the desk"
    );
    Ok(())
}

#[tokio::test]
async fn each_refusal_names_its_reason_and_the_sessions_it_holds() -> TestResult {
    let (app, script) = rig();
    let cases: Vec<(ReceiveError, StatusCode, &str, Option<Vec<usize>>)> = vec![
        (
            ReceiveError::Undecided(vec![1, 3]),
            StatusCode::CONFLICT,
            "undecided",
            Some(vec![1, 3]),
        ),
        (
            ReceiveError::SecretImport(vec![2]),
            StatusCode::CONFLICT,
            "secret_import",
            Some(vec![2]),
        ),
        (
            ReceiveError::AlreadyReceived("abc".into()),
            StatusCode::CONFLICT,
            "already_received",
            None,
        ),
        (
            ReceiveError::Gone("gone".into()),
            StatusCode::NOT_FOUND,
            "gone",
            None,
        ),
        (
            ReceiveError::Burned("burned".into()),
            StatusCode::GONE,
            "burned",
            None,
        ),
        (
            ReceiveError::BadSignature("forged".into()),
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_signature",
            None,
        ),
        (
            ReceiveError::BadCode("typo".into()),
            StatusCode::BAD_REQUEST,
            "bad_code",
            None,
        ),
    ];
    for (error, status, reason, sessions) in cases {
        script.inspect.lock().push(Err(error));
        let (got, text) = call(
            &app,
            Method::POST,
            "/api/share/receive",
            &request(r#"{"code":"CODE"}"#),
        )
        .await?;
        assert_eq!(got, status, "{reason}: {text}");
        let refusal: body::ReceiveRefusal = serde_json::from_str(&text)?;
        assert_eq!(serde_json::to_value(refusal.reason)?, reason);
        assert_eq!(refusal.sessions, sessions, "{reason}");
    }
    script.inspect.lock().push(Err(ReceiveError::Failed(format!(
        "disk said {SECRET_CODE}"
    ))));
    let (status, text) = call(
        &app,
        Method::POST,
        "/api/share/receive",
        &request(r#"{"code":"CODE"}"#),
    )
    .await?;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        !text.contains("canary"),
        "an internal failure is not echoed: {text}"
    );
    Ok(())
}

#[tokio::test]
async fn trust_routes_list_add_and_remove_a_sender() -> TestResult {
    let (app, _script) = rig();
    let (status, text) = call(
        &app,
        Method::POST,
        "/api/share/trust",
        r#"{"name":"alex","key":"ed25519:abc"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::CREATED, "{text}");
    let (_, text) = call(&app, Method::GET, "/api/share/trust", "").await?;
    let listed: body::TrustReply = serde_json::from_str(&text)?;
    assert_eq!(
        listed.senders,
        vec![body::TrustedSender {
            name: "alex".to_owned(),
            key: "ed25519:abc".to_owned(),
            fingerprint: "fp-ed25519:abc".to_owned(),
        }]
    );
    let (status, text) = call(
        &app,
        Method::DELETE,
        "/api/share/trust",
        r#"{"name":"alex"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{text}");
    let removed: body::UntrustReply = serde_json::from_str(&text)?;
    assert_eq!(removed.removed, 1);
    Ok(())
}

#[tokio::test]
async fn a_node_without_a_receive_desk_says_so() -> TestResult {
    let (gateway, _store) = common::gateway();
    let app = gateway.router(false);
    let (status, _) = call(
        &app,
        Method::POST,
        "/api/share/receive",
        &request(r#"{"code":"CODE"}"#),
    )
    .await?;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    Ok(())
}

#[tokio::test]
async fn a_node_admin_who_is_not_the_owner_is_refused() -> TestResult {
    let script = Arc::new(Script::default());
    let store: Arc<dyn MemoryStore> = Arc::new(FakeMemoryStore::default());
    let owner = gateway::tenancy::UserId::new(testkit::OWNER.to_owned())?;
    let admin = gateway::tenancy::UserId::new("admin".to_owned())?;
    let tenancy = gateway::tenancy::Tenancy::local(owner)
        .with_admin(admin.clone())
        .with_token("admin-secret", admin)?;
    let gateway = common::owned(gateway::Gateway::new(store, common::settings()))
        .with_tenancy(tenancy)
        .with_receive_desk(Arc::new(Fake(Arc::clone(&script))));
    let app = Arc::new(gateway).router(false);
    let mut request = Request::builder()
        .method(Method::GET)
        .uri("/api/share/trust")
        .header("host", "127.0.0.1:5174")
        .header("authorization", "Bearer admin-secret")
        .body(Body::empty())?;
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:41000".parse::<std::net::SocketAddr>()?,
    ));
    let response = app.oneshot(request).await?;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await?;
    let refusal: body::ReceiveRefusal = serde_json::from_slice(&bytes)?;
    assert_eq!(refusal.reason, body::ReceiveReason::NotLocal);
    Ok(())
}

#[tokio::test]
async fn a_users_file_bearer_mapped_to_the_owner_cannot_receive_or_change_trust() -> TestResult {
    let script = Arc::new(Script::default());
    script.inspect.lock().push(Ok(inspected(0, false)));
    script.import.lock().push(Ok(imported()));
    let store: Arc<dyn MemoryStore> = Arc::new(FakeMemoryStore::default());
    let owner = gateway::tenancy::UserId::new(testkit::OWNER.to_owned())?;
    let tenancy =
        gateway::tenancy::Tenancy::local(owner.clone()).with_token("owner-users-file", owner)?;
    let gateway = common::owned(gateway::Gateway::new(store, common::settings()))
        .with_tenancy(tenancy)
        .with_receive_desk(Arc::new(Fake(Arc::clone(&script))));
    let app = Arc::new(gateway).router(false);
    for (method, path, json) in [
        (Method::GET, "/api/share/trust", String::new()),
        (
            Method::POST,
            "/api/share/trust",
            r#"{"name":"mallory","key":"age1abc"}"#.to_owned(),
        ),
        (
            Method::DELETE,
            "/api/share/trust",
            r#"{"name":"mallory"}"#.to_owned(),
        ),
        (
            Method::POST,
            "/api/share/receive",
            request(r#"{"code":"CODE"}"#),
        ),
        (
            Method::POST,
            "/api/share/receive/import",
            request(r#"{"code":"CODE","sessions":[],"force":true}"#),
        ),
    ] {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "127.0.0.1:5174")
            .header("content-type", "application/json")
            .header("authorization", "Bearer owner-users-file")
            .body(Body::from(json))?;
        request.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:41000".parse::<std::net::SocketAddr>()?,
        ));
        let response = app.clone().oneshot(request).await?;
        assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 20).await?;
        let refusal: body::ReceiveRefusal = serde_json::from_slice(&bytes)?;
        assert_eq!(refusal.reason, body::ReceiveReason::NotLocal, "{path}");
    }
    assert!(
        script.codes.lock().is_empty(),
        "no refused call reaches the desk"
    );
    assert!(script.senders.lock().is_empty(), "trust is unchanged");
    Ok(())
}
