use std::net::SocketAddr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use chrono::{DateTime, Utc};
use gateway::Gateway;
use gateway::tenancy::{Tenancy, UserId};
use kernel::memory::share::desk::{
    DeskError, Draft, DraftId, DraftRequest, DraftSession, DraftTotals, EscapedText, FindQuery,
    FlagState, FlagView, Found, LoginId, LoginRequest, LoginStep, Mark, Marked, NodeKeyView,
    SendRequest, Sent, ServerOrigin, Servers, ShareDesk, TokenStore,
};
use parking_lot::Mutex;
use tower::ServiceExt as _;
use uuid::Uuid;
use wire::gateway::share::{DraftReport, RefusalReason, ShareRefusal};
use wire::plugin::manifest::Permission;

use crate::common;
use crate::plugin_support::{Failure, TestResult, harness_with_desk, write_manifest};

const DEVICE: &str = "desk";
const LOOPBACK: &str = "127.0.0.1:41000";
const LAN: &str = "203.0.113.20:41000";
const SESSION: &str = "0b7a7c62-3c43-4a1e-8d57-6d7a4f2a9e01";
const HIDDEN: &str = "pay\u{202E}me\u{200B}now";

#[derive(Debug, Default)]
struct FakeDesk {
    calls: Mutex<Vec<&'static str>>,
    failure: Mutex<Option<DeskError>>,
    draft: Mutex<Option<DraftId>>,
}

impl FakeDesk {
    fn enter(&self, call: &'static str) -> Result<(), DeskError> {
        self.calls.lock().push(call);
        self.failure.lock().take().map_or(Ok(()), Err)
    }

    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().clone()
    }

    fn fail_with(&self, error: DeskError) {
        *self.failure.lock() = Some(error);
    }

    fn id(&self) -> DraftId {
        *self.draft.lock().get_or_insert_with(DraftId::random)
    }

    fn drafted(&self) -> Draft {
        Draft {
            id: self.id(),
            expires_at: expiry(),
            totals: DraftTotals::default(),
            rules: Vec::new(),
            sessions: vec![DraftSession {
                id: Uuid::nil(),
                agent: "claude".to_owned(),
                messages: 3,
            }],
            not_scanned: String::new(),
            best_effort: String::new(),
            decision: None,
            flags: vec![FlagView {
                number: 1,
                score: 0.9,
                session: None,
                place: None,
                length: HIDDEN.chars().count(),
                text: EscapedText::escape(HIDDEN),
                state: FlagState::Open,
            }],
        }
    }
}

fn expiry() -> DateTime<Utc> {
    DateTime::<Utc>::UNIX_EPOCH
}

#[async_trait]
impl ShareDesk for FakeDesk {
    async fn find(&self, _query: FindQuery) -> Result<Found, DeskError> {
        self.enter("find")?;
        Ok(Found {
            sessions: Vec::new(),
            dropped: 0,
        })
    }

    async fn draft(&self, _request: DraftRequest) -> Result<Draft, DeskError> {
        self.enter("draft")?;
        Ok(self.drafted())
    }

    async fn report(&self, _id: DraftId) -> Result<Draft, DeskError> {
        self.enter("report")?;
        Ok(self.drafted())
    }

    async fn mark(&self, _id: DraftId, _marks: Vec<Mark>) -> Result<Marked, DeskError> {
        self.enter("mark")?;
        Ok(Marked {
            draft: self.drafted(),
            rechecked: Vec::new(),
            learned: None,
        })
    }

    async fn send(&self, _id: DraftId, _send: SendRequest) -> Result<Sent, DeskError> {
        self.enter("send")?;
        Ok(Sent {
            code: "drop#key".to_owned(),
            expires_at: expiry(),
            bytes: 10,
            signed: None,
        })
    }

    async fn discard(&self, _id: DraftId) -> Result<(), DeskError> {
        self.enter("discard")
    }

    async fn servers(&self) -> Result<Servers, DeskError> {
        self.enter("servers")?;
        Ok(Servers {
            servers: Vec::new(),
            logins_file: "share-logins.json".to_owned(),
            token_store: TokenStore::File,
        })
    }

    async fn login(&self, _request: LoginRequest) -> Result<LoginStep, DeskError> {
        self.enter("login")?;
        Ok(LoginStep::Pending { interval: 5 })
    }

    async fn poll(&self, _login: LoginId) -> Result<LoginStep, DeskError> {
        self.enter("poll")?;
        Ok(LoginStep::Pending { interval: 5 })
    }

    async fn logout(&self, _origin: ServerOrigin) -> Result<(), DeskError> {
        self.enter("logout")
    }

    async fn key(&self) -> Result<NodeKeyView, DeskError> {
        self.enter("key")?;
        Ok(NodeKeyView {
            key: "ed25519:AAAA".to_owned(),
            fingerprint: "ed25519:0123".to_owned(),
        })
    }
}

fn node(desk: &Arc<FakeDesk>) -> Result<Router, Failure> {
    let owner = UserId::new("owner".to_owned())?;
    let store = Arc::new(testkit::FakeMemoryStore::default());
    let tenancy = Tenancy::local(owner.clone()).with_token(DEVICE, owner)?;
    let gateway = Gateway::new(store, common::settings())
        .with_local_owner(common::local_owner())
        .with_tenancy(tenancy)
        .with_share_desk(Arc::clone(desk) as Arc<dyn ShareDesk>);
    Ok(Arc::new(gateway).router(true))
}

struct Route {
    method: Method,
    path: String,
    body: Option<String>,
}

fn route(method: Method, path: &str, body: Option<&str>) -> Route {
    Route {
        method,
        path: path.to_owned(),
        body: body.map(str::to_owned),
    }
}

fn draft_path() -> String {
    format!("/api/share/drafts/{SESSION}")
}

fn create_draft() -> Route {
    route(
        Method::POST,
        "/api/share/drafts",
        Some(&format!(
            r#"{{"sessions":[{{"id":"{SESSION}","decision":"approve"}}]}}"#
        )),
    )
}

fn read_draft() -> Route {
    route(Method::GET, &draft_path(), None)
}

fn mark_draft() -> Route {
    route(
        Method::POST,
        &format!("{}/marks", draft_path()),
        Some(r#"{"marks":[]}"#),
    )
}

fn discard_draft() -> Route {
    route(Method::DELETE, &draft_path(), None)
}

fn read_key() -> Route {
    route(Method::GET, "/api/share/key", None)
}

fn routes() -> Vec<Route> {
    vec![
        route(Method::POST, "/api/share/find", Some(r#"{"query":"x"}"#)),
        create_draft(),
        read_draft(),
        mark_draft(),
        route(
            Method::POST,
            &format!("{}/send", draft_path()),
            Some(r#"{"server":"s","signed":false,"confirm":{"sessions":1,"kept_flags":0}}"#),
        ),
        discard_draft(),
        route(Method::GET, "/api/share/servers", None),
        route(
            Method::POST,
            "/api/share/servers/login",
            Some(r#"{"kind":"cloud","origin":"o"}"#),
        ),
        route(
            Method::POST,
            "/api/share/servers/login/poll",
            Some(&format!(r#"{{"login":"{SESSION}"}}"#)),
        ),
        route(
            Method::POST,
            "/api/share/servers/logout",
            Some(r#"{"origin":"o"}"#),
        ),
        read_key(),
    ]
}

async fn call(
    app: &Router,
    route: &Route,
    remote: &str,
    headers: &[(&str, &str)],
) -> Result<(StatusCode, axum::http::HeaderMap, String), Failure> {
    call_as(app, route, remote, headers, Some(&common::owner_token())).await
}

async fn call_as(
    app: &Router,
    route: &Route,
    remote: &str,
    headers: &[(&str, &str)],
    bearer: Option<&str>,
) -> Result<(StatusCode, axum::http::HeaderMap, String), Failure> {
    let mut request = Request::builder()
        .method(route.method.clone())
        .uri(&route.path)
        .header("content-type", "application/json");
    if !headers.iter().any(|(name, _)| *name == "host") {
        request = request.header("host", "127.0.0.1:5174");
    }
    if let Some(bearer) = bearer {
        request = request.header("authorization", format!("Bearer {bearer}"));
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let mut request = request.body(route.body.clone().map_or_else(Body::empty, Body::from))?;
    request
        .extensions_mut()
        .insert(ConnectInfo(remote.parse::<SocketAddr>()?));
    let response = app.clone().oneshot(request).await?;
    let status = response.status();
    let headers = response.headers().clone();
    let body = axum::body::to_bytes(response.into_body(), 1 << 20).await?;
    Ok((status, headers, String::from_utf8(body.to_vec())?))
}

fn refusal(body: &str) -> Result<ShareRefusal, Failure> {
    Ok(serde_json::from_str(body)?)
}

#[tokio::test(flavor = "multi_thread")]
async fn share_routes_refuse_non_loopback_forwarded_and_foreign_host() -> TestResult {
    let desk = Arc::new(FakeDesk::default());
    let app = node(&desk)?;
    let cases: [(&str, &[(&str, &str)]); 4] = [
        (LAN, &[]),
        (LOOPBACK, &[("forwarded", "for=203.0.113.9")]),
        (LOOPBACK, &[("x-forwarded-for", "203.0.113.9")]),
        (LOOPBACK, &[("host", "attacker.example")]),
    ];
    let owner = common::owner_token();
    for route in routes() {
        for (remote, headers) in cases {
            for (bearer, want) in [
                (owner.as_str(), StatusCode::UNAUTHORIZED),
                (DEVICE, StatusCode::FORBIDDEN),
            ] {
                let (status, _, body) =
                    call_as(&app, &route, remote, headers, Some(bearer)).await?;
                let label = format!(
                    "{} {} from {remote} with {headers:?} and {bearer}",
                    route.method, route.path
                );
                assert_eq!(status, want, "{label}: {body}");
                if want == StatusCode::FORBIDDEN {
                    assert_eq!(refusal(&body)?.reason, RefusalReason::NotLocal, "{label}");
                }
            }
        }
    }
    assert_eq!(desk.calls(), Vec::<&str>::new());
    let (status, _, body) = call(&app, &read_key(), LOOPBACK, &[]).await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(desk.calls(), ["key"]);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn share_routes_need_the_local_owner_credential() -> TestResult {
    let desk = Arc::new(FakeDesk::default());
    let app = node(&desk)?;
    for route in routes() {
        for bearer in [None, Some("not-the-owner-token")] {
            let (status, headers, body) = call_as(&app, &route, LOOPBACK, &[], bearer).await?;
            let label = format!("{} {} with {bearer:?}", route.method, route.path);
            assert_eq!(status, StatusCode::UNAUTHORIZED, "{label}: {body}");
            assert!(headers.contains_key("www-authenticate"), "{label}");
        }
    }
    assert_eq!(desk.calls(), Vec::<&str>::new());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn share_routes_are_refused_to_plugins_with_memory_read() -> TestResult {
    let root = tempfile::tempdir()?;
    let source = root.path().join("source");
    write_manifest(
        &source,
        &[
            Permission::SessionsRead,
            Permission::TopicsRead,
            Permission::MemoryRead,
            Permission::MemoryWrite,
        ],
    )?;
    let desk = Arc::new(FakeDesk::default());
    let host = harness_with_desk(root.path(), Arc::clone(&desk) as Arc<dyn ShareDesk>).await?;
    host.install_and_enable(&source).await?;
    for (method, path) in [
        ("GET", "/api/share/key"),
        ("GET", "/api/share/servers"),
        ("POST", "/api/share/find"),
        ("POST", "/api/share/drafts"),
        ("GET", "/api/memory/stats"),
    ] {
        let answer = host.probe(method, path).await?;
        let want = if path.starts_with("/api/share") {
            "403"
        } else {
            "200"
        };
        assert_eq!(answer, want, "{method} {path}");
    }
    assert_eq!(desk.calls(), Vec::<&str>::new());
    host.gateway.close().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn flag_text_is_escaped() -> TestResult {
    let desk = Arc::new(FakeDesk::default());
    let app = node(&desk)?;
    for route in [create_draft(), read_draft(), mark_draft()] {
        let (status, headers, body) = call(&app, &route, LOOPBACK, &[]).await?;
        assert!(status.is_success(), "{}: {body}", route.path);
        assert!(body.contains("pay<U+202E>me<U+200B>now"), "{body}");
        assert!(!body.contains('\u{202E}') && !body.contains('\u{200B}'));
        assert_eq!(
            headers
                .get("cache-control")
                .map(axum::http::HeaderValue::as_bytes),
            Some(&b"no-store"[..])
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_created_draft_round_trips_into_the_wire_type() -> TestResult {
    let desk = Arc::new(FakeDesk::default());
    let app = node(&desk)?;
    let (status, _, body) = call(&app, &create_draft(), LOOPBACK, &[]).await?;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let report: DraftReport = serde_json::from_str(&body)?;
    assert_eq!(report.draft, desk.id().to_string());
    assert_eq!(report.sessions.len(), 1);
    assert_eq!(report.flags.len(), 1);
    let texts: Vec<&str> = report.flags.iter().map(|flag| flag.text.as_str()).collect();
    assert_eq!(texts, ["pay<U+202E>me<U+200B>now"]);
    assert!(!format!("{report:?}").contains("pay"));
    let (status, _, body) = call(&app, &discard_draft(), LOOPBACK, &[]).await?;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    Ok(())
}

type Mapping = (DeskError, u16, RefusalReason, Option<u16>);

fn origin() -> ServerOrigin {
    ServerOrigin::from("https://share.example".to_owned())
}

fn mappings() -> Vec<Mapping> {
    let mut rows = request_mappings();
    rows.extend(login_mappings());
    rows.extend(server_mappings());
    rows
}

fn request_mappings() -> Vec<Mapping> {
    vec![
        (
            DeskError::UnknownDraft,
            404,
            RefusalReason::UnknownDraft,
            None,
        ),
        (
            DeskError::UnknownLogin,
            404,
            RefusalReason::UnknownLogin,
            None,
        ),
        (
            DeskError::UnknownSessions("x".to_owned()),
            404,
            RefusalReason::UnknownSessions,
            None,
        ),
        (
            DeskError::OpenFlags { open: 2 },
            409,
            RefusalReason::OpenFlags,
            None,
        ),
        (
            DeskError::Counts {
                sessions: 1,
                kept: 0,
                confirmed_sessions: 2,
                confirmed_kept: 0,
            },
            409,
            RefusalReason::Counts,
            None,
        ),
        (
            DeskError::DecideUnconfigured,
            409,
            RefusalReason::DecideUnconfigured,
            None,
        ),
        (DeskError::Full, 429, RefusalReason::Full, None),
        (
            DeskError::Choices("c".to_owned()),
            400,
            RefusalReason::Invalid,
            None,
        ),
        (
            DeskError::Marks("m".to_owned()),
            400,
            RefusalReason::Invalid,
            None,
        ),
    ]
}

fn login_mappings() -> Vec<Mapping> {
    vec![
        (
            DeskError::BadToken("t".to_owned()),
            400,
            RefusalReason::BadToken,
            None,
        ),
        (
            DeskError::BadServer("s".to_owned()),
            400,
            RefusalReason::BadServer,
            None,
        ),
        (
            DeskError::SignedOut(origin()),
            401,
            RefusalReason::SignedOut,
            None,
        ),
        (
            DeskError::LoginExpired(origin()),
            401,
            RefusalReason::LoginExpired,
            None,
        ),
        (
            DeskError::LoginDenied("denied"),
            403,
            RefusalReason::LoginDenied,
            None,
        ),
    ]
}

fn server_mappings() -> Vec<Mapping> {
    vec![
        (
            DeskError::DropBox {
                status: 413,
                message: "too big".to_owned(),
            },
            502,
            RefusalReason::DropBox,
            Some(413),
        ),
        (
            DeskError::Unreachable("down".to_owned()),
            502,
            RefusalReason::Unreachable,
            None,
        ),
        (
            DeskError::Failed("/home/phil/secret path".to_owned()),
            500,
            RefusalReason::Failed,
            None,
        ),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn desk_errors_map_to_one_status_and_reason() -> TestResult {
    let desk = Arc::new(FakeDesk::default());
    let app = node(&desk)?;
    let key = read_key();
    for (error, status, reason, upstream) in mappings() {
        let label = format!("{error:?}");
        desk.fail_with(error);
        let (got, _, body) = call(&app, &key, LOOPBACK, &[]).await?;
        assert_eq!(got.as_u16(), status, "{label}: {body}");
        let refused = refusal(&body)?;
        assert_eq!(refused.reason, reason, "{label}");
        assert_eq!(refused.upstream_status, upstream, "{label}");
        assert!(!body.contains("/home/phil"), "{label}: {body}");
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn a_malformed_request_is_an_invalid_refusal_that_never_reaches_the_desk() -> TestResult {
    let desk = Arc::new(FakeDesk::default());
    let app = node(&desk)?;
    let bad = |path: &str, body: &str| Route {
        method: Method::POST,
        path: path.to_owned(),
        body: Some(body.to_owned()),
    };
    for route in [
        bad("/api/share/find", r#"{"query":"x","extra":1}"#),
        bad("/api/share/find", "not json"),
        bad(
            "/api/share/drafts",
            r#"{"sessions":[{"id":"nope","decision":"approve"}]}"#,
        ),
        bad(
            "/api/share/drafts",
            &format!(
                r#"{{"sessions":[{{"id":"{SESSION}","decision":"approve","redact":{{"terms":["a"]}}}}]}}"#
            ),
        ),
    ] {
        let (status, _, body) = call(&app, &route, LOOPBACK, &[]).await?;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(refusal(&body)?.reason, RefusalReason::Invalid, "{body}");
    }
    let unknown = Route {
        method: Method::GET,
        path: "/api/share/drafts/not-an-id".to_owned(),
        body: None,
    };
    let (status, _, body) = call(&app, &unknown, LOOPBACK, &[]).await?;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
    assert_eq!(refusal(&body)?.reason, RefusalReason::UnknownDraft);
    assert_eq!(desk.calls(), Vec::<&str>::new());
    Ok(())
}
