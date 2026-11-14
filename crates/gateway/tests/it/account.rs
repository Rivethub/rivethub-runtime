use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair as _};
use axum::Router;
use axum::body::Body;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::{HeaderMap, Method, Request, StatusCode};
use axum::response::{IntoResponse as _, Response};
use axum::routing::{get, post};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, Utc};
use config::HubEntry;
use gateway::Gateway;
use gateway::account::{
    AccountFiles, AccountService, CloudAccess, CloudOs, CloudSettings, CrashJob, CrashReport, Hubs,
    PanicSite, UpdateKey, queue, queued,
};
use gateway::tenancy::{Tenancy, UserId};
use parking_lot::Mutex;
use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt as _;
use uuid::Uuid;
use wire::gateway::account::{
    Account, Hub, HubKind, ReleaseChannel, SignInProgress, SignInStarted, UpdateSource,
};

use crate::common;

type TestResult = Result<(), Box<dyn std::error::Error + Send + Sync>>;
type Failure = Box<dyn std::error::Error + Send + Sync>;

const TOKEN: &str = "rht_test-install-token";
const EMAIL: &str = "user@example.com";
const OWNER_DEVICE: &str = "desk";
const ARCHIVE: &[u8] = b"signed release archive";
const REVOKED: &str = "0.0.0-revoked";

#[derive(Debug, Default)]
struct Seen {
    starts: Mutex<Vec<String>>,
    signed: std::sync::atomic::AtomicBool,
    unavailable: std::sync::atomic::AtomicBool,
    auth_reads: AtomicU32,
    polls: AtomicU32,
    bearers: Mutex<Vec<(String, String)>>,
    crashes: Mutex<Vec<String>>,
    latest: Mutex<Vec<String>>,
    requests: AtomicU32,
}

struct Fake {
    seen: Arc<Seen>,
    stub: testkit::Stub,
    key: UpdateKey,
}

#[derive(Debug, Deserialize)]
struct Latest {
    channel: String,
    os: String,
}

struct Release {
    version: &'static str,
    signature: Signing,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Signing {
    Good,
    Forged,
}

async fn fake_cloud(release: Release) -> Result<Fake, Failure> {
    let pair = Ed25519KeyPair::generate()?;
    let key = UpdateKey::parse(&STANDARD.encode(pair.public_key().as_ref()))?;
    let digest: [u8; 32] = Sha256::digest(ARCHIVE).into();
    let signed = match release.signature {
        Signing::Good => digest,
        Signing::Forged => Sha256::digest(b"another archive").into(),
    };
    let signature = STANDARD.encode(pair.sign(&signed).as_ref());
    let sha = hex::encode(digest);
    let version = release.version;
    let seen = Arc::new(Seen::default());
    let routes = Router::new()
        .route("/api/auth/oauth2/revoke", post(bearer))
        .route("/v1/telemetry/crash", post(crash))
        .route(
            "/v1/updates/latest",
            get(
                move |State(seen): State<Arc<Seen>>,
                      headers: HeaderMap,
                      Query(query): Query<Latest>| async move {
                    seen.latest.lock().push(format!(
                        "{}|{}|{}",
                        authorization(&headers),
                        query.channel,
                        query.os
                    ));
                    if version == REVOKED {
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    let host = headers
                        .get("host")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_owned();
                    axum::Json(wire_manifest(version, &host, &sha, &signature)).into_response()
                },
            ),
        )
        .route("/archive", get(|| async { ARCHIVE.to_vec() }))
        .route(
            "/healthz",
            get(|| async {
                json(
                    r#"{"ok":true,"sessions":0,"name":"team","node":"team","version":"0.9.0"}"#
                        .to_owned(),
                )
            }),
        )
        .layer(axum::middleware::from_fn_with_state(
            Arc::clone(&seen),
            counted,
        ))
        .with_state(Arc::clone(&seen));
    Ok(Fake {
        seen,
        stub: testkit::stub_server(routes).await?,
        key,
    })
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    version: String,
    url: String,
    sha256: String,
    signature: String,
    notes_url: String,
}

fn wire_manifest(version: &str, host: &str, sha: &str, signature: &str) -> Manifest {
    Manifest {
        version: version.to_owned(),
        url: format!("http://{host}/archive"),
        sha256: sha.to_owned(),
        signature: signature.to_owned(),
        notes_url: "https://example.com/notes".to_owned(),
    }
}

async fn counted(
    State(seen): State<Arc<Seen>>,
    request: Request<Body>,
    next: axum::middleware::Next,
) -> Response {
    seen.requests.fetch_add(1, Ordering::SeqCst);
    next.run(request).await
}

fn authorization(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned()
}

fn json(body: String) -> Response {
    ([("content-type", "application/json")], body).into_response()
}

#[derive(Debug)]
struct FakeLogin(Arc<Seen>);
#[async_trait::async_trait]
impl gateway::account::AccountLogin for FakeLogin {
    async fn start(&self) -> Result<kernel::account::DeviceLogin, kernel::account::LoginError> {
        if self.0.signed.load(Ordering::SeqCst) {
            return Err(kernel::account::LoginError::SignedIn);
        }
        self.0.starts.lock().push("{}".to_owned());
        Ok(kernel::account::DeviceLogin {
            user_code: "ABCD-EFGH".to_owned(),
            verification_uri: "https://rivethub.cloud/device".to_owned(),
            expires_in: 600,
            poll_interval_ms: 1000,
        })
    }
    async fn poll(&self) -> Result<gateway::account::LoginProgress, kernel::account::LoginError> {
        if self.0.starts.lock().is_empty() {
            return Err(kernel::account::LoginError::NoSignIn);
        }
        if self.0.polls.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(gateway::account::LoginProgress::Pending);
        }
        self.0.signed.store(true, Ordering::SeqCst);
        Ok(gateway::account::LoginProgress::SignedIn)
    }
    async fn identity(&self) -> Result<Option<(String, String)>, kernel::account::LoginError> {
        self.0.auth_reads.fetch_add(1, Ordering::SeqCst);
        if self.0.unavailable.load(Ordering::SeqCst) {
            return Err(kernel::account::LoginError::CloudReply);
        }
        Ok(self
            .0
            .signed
            .load(Ordering::SeqCst)
            .then(|| (TOKEN.to_owned(), EMAIL.to_owned())))
    }
    async fn sign_out(&self, _revoke: bool) -> Result<(), kernel::account::LoginError> {
        self.0.signed.store(false, Ordering::SeqCst);
        self.0.bearers.lock().push((
            "POST /api/auth/oauth2/revoke".to_owned(),
            format!("Bearer {TOKEN}"),
        ));
        Ok(())
    }
}

async fn bearer(
    State(seen): State<Arc<Seen>>,
    method: Method,
    uri: axum::http::Uri,
    headers: HeaderMap,
) -> Response {
    seen.bearers
        .lock()
        .push((format!("{method} {}", uri.path()), authorization(&headers)));
    json(r#"{"ok":true}"#.to_owned())
}

async fn crash(State(seen): State<Arc<Seen>>, body: String) -> Response {
    seen.crashes.lock().push(body);
    StatusCode::ACCEPTED.into_response()
}

struct Node {
    app: Router,
    account: Arc<AccountService>,
}

async fn node(
    root: &Path,
    fake: &Fake,
    access: CloudAccess,
    hubs: Vec<HubEntry>,
) -> Result<Node, Failure> {
    node_reporting_to(root, fake, access, hubs, None).await
}

async fn node_reporting_to(
    root: &Path,
    fake: &Fake,
    access: CloudAccess,
    hubs: Vec<HubEntry>,
    telemetry: Option<&Fake>,
) -> Result<Node, Failure> {
    let owner = UserId::new("owner".to_owned())?;
    let tenancy = Tenancy::local(owner.clone()).with_token(OWNER_DEVICE, owner)?;
    let mut settings = common::settings();
    settings.state_dir = root.join("state");
    let cloud = CloudSettings {
        base: format!("http://{}/", fake.stub.addr).parse()?,
        telemetry: format!("http://{}/", telemetry.unwrap_or(fake).stub.addr).parse()?,
        update_key: fake.key,
        access,
    };
    let hubs = Hubs::new(
        "desk-node",
        "http://127.0.0.1:5174".to_owned(),
        hubs,
        Vec::new(),
    )?;
    let account = Arc::new(
        AccountService::open(&settings.state_dir, cloud, hubs)
            .await?
            .with_login(Arc::new(FakeLogin(Arc::clone(&fake.seen)))),
    );
    let gateway = Gateway::new(Arc::new(testkit::FakeMemoryStore::default()), settings)
        .with_tenancy(tenancy)
        .with_account(Arc::clone(&account));
    Ok(Node {
        app: Arc::new(gateway).router(true),
        account,
    })
}

async fn send(
    app: &Router,
    method: Method,
    path: &str,
    body: &str,
    remote: &str,
) -> Result<(StatusCode, String), Failure> {
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
        axum::http::HeaderValue::from_str(&format!("Bearer {}", OWNER_DEVICE.to_owned()))?,
    );
    let response = app.clone().oneshot(request).await?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024).await?;
    Ok((status, String::from_utf8(bytes.to_vec())?))
}

async fn local(
    app: &Router,
    method: Method,
    path: &str,
    body: &str,
) -> Result<(StatusCode, String), Failure> {
    send(app, method, path, body, "127.0.0.1:41000").await
}

async fn account(app: &Router) -> Result<Account, Failure> {
    let (status, body) = local(app, Method::GET, "/api/account", "").await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    Ok(serde_json::from_str(&body)?)
}

fn release() -> Release {
    Release {
        version: "99.0.0",
        signature: Signing::Good,
    }
}

#[tokio::test]
async fn a_fresh_install_has_an_id_and_privacy_defaults_in_an_owner_only_file() -> TestResult {
    let root = tempfile::tempdir()?;
    let fake = fake_cloud(release()).await?;
    let node = node(root.path(), &fake, CloudAccess::Open, Vec::new()).await?;
    let shown = account(&node.app).await?;
    assert_eq!(
        shown,
        Account {
            email: None,
            update_source: UpdateSource::Github,
            channel: ReleaseChannel::Stable,
            auto_download: true,
            version: gateway::VERSION.to_owned(),
            pending_update: None,
            crash_reports: true,
            usage_counts: true,
            cloud_disabled: false,
        }
    );
    let files = AccountFiles::new(&root.path().join("state"));
    let mode = std::fs::metadata(files.install())?.permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let first = files.read()?.ok_or("install state missing")?.install_id;
    assert_eq!(first.get_version_num(), 4);
    drop(node);
    let again = self::node(root.path(), &fake, CloudAccess::Open, Vec::new()).await?;
    assert_eq!(
        again.account.files().read()?.ok_or("missing")?.install_id,
        first
    );
    Ok(())
}

#[tokio::test]
async fn signing_in_keeps_the_install_token_out_of_every_response() -> TestResult {
    let root = tempfile::tempdir()?;
    let fake = fake_cloud(release()).await?;
    let node = node(root.path(), &fake, CloudAccess::Open, Vec::new()).await?;
    let (status, body) = local(&node.app, Method::POST, "/api/account/signin/poll", "").await?;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, body) = local(&node.app, Method::POST, "/api/account/signin", "{}").await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let started: SignInStarted = serde_json::from_str(&body)?;
    assert_eq!(started.user_code, "ABCD-EFGH");
    let files = AccountFiles::new(&root.path().join("state"));

    let (_, body) = local(&node.app, Method::POST, "/api/account/signin/poll", "").await?;
    assert_eq!(
        serde_json::from_str::<SignInProgress>(&body)?,
        SignInProgress::Pending
    );
    let (status, body) = local(&node.app, Method::POST, "/api/account/signin/poll", "").await?;
    assert_eq!(status, StatusCode::OK);
    assert!(!body.contains(TOKEN), "{body}");
    let SignInProgress::SignedIn { account } = serde_json::from_str(&body)? else {
        return Err(format!("still pending: {body}").into());
    };
    assert_eq!(account.email.as_deref(), Some(EMAIL));
    assert_eq!(account.update_source, UpdateSource::RivethubCloud);
    let (_, body) = local(&node.app, Method::GET, "/api/account", "").await?;
    assert!(!body.contains(TOKEN), "{body}");
    assert_eq!(
        files
            .read()?
            .and_then(|state| state.token().map(str::to_owned))
            .as_deref(),
        None
    );

    let (status, body) = local(
        &node.app,
        Method::PATCH,
        "/api/account",
        r#"{"updateSource":"github","channel":"beta"}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let changed: Account = serde_json::from_str(&body)?;
    assert_eq!(
        (changed.update_source, changed.channel),
        (UpdateSource::Github, ReleaseChannel::Beta)
    );

    let reads = fake.seen.auth_reads.load(Ordering::SeqCst);
    node.account
        .tick(Utc::now(), &CancellationToken::new())
        .await;
    assert_eq!(fake.seen.auth_reads.load(Ordering::SeqCst), reads);
    let (status, body) = local(&node.app, Method::POST, "/api/account/signout", "").await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let signed_out: Account = serde_json::from_str(&body)?;
    assert_eq!(
        (signed_out.email, signed_out.update_source),
        (None, UpdateSource::Github)
    );
    assert_eq!(
        fake.seen.bearers.lock().clone(),
        vec![(
            "POST /api/auth/oauth2/revoke".to_owned(),
            format!("Bearer {TOKEN}")
        )]
    );
    assert_eq!(
        files
            .read()?
            .and_then(|state| state.token().map(str::to_owned)),
        None
    );
    Ok(())
}

#[tokio::test]
async fn a_cloud_failure_hides_identity_and_a_saved_cloud_source_survives() -> TestResult {
    let (_root, fake, node) = signed_in(release()).await?;
    fake.seen.unavailable.store(true, Ordering::SeqCst);
    assert!(self::account(&node.app).await?.email.is_none());
    fake.seen.unavailable.store(false, Ordering::SeqCst);
    let (_, selected) = local(
        &node.app,
        Method::PATCH,
        "/api/account",
        r#"{"updateSource":"rivethub-cloud"}"#,
    )
    .await?;
    let selected: Account = serde_json::from_str(&selected)?;
    assert_eq!(selected.update_source, UpdateSource::RivethubCloud);
    assert_eq!(selected.email.as_deref(), Some(EMAIL));
    Ok(())
}

#[tokio::test]
async fn deleting_the_account_signs_out_for_web_deletion() -> TestResult {
    let root = tempfile::tempdir()?;
    let fake = fake_cloud(release()).await?;
    let node = node(root.path(), &fake, CloudAccess::Open, Vec::new()).await?;
    let (status, _) = local(&node.app, Method::DELETE, "/api/account", "").await?;
    assert_eq!(status, StatusCode::OK);
    fake.seen.bearers.lock().clear();
    local(&node.app, Method::POST, "/api/account/signin", "{}").await?;
    local(&node.app, Method::POST, "/api/account/signin/poll", "").await?;
    local(&node.app, Method::POST, "/api/account/signin/poll", "").await?;
    let (status, body) = local(&node.app, Method::POST, "/api/account/signin", "{}").await?;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let (status, body) = local(&node.app, Method::DELETE, "/api/account", "").await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(serde_json::from_str::<Account>(&body)?.email, None);
    assert_eq!(
        fake.seen.bearers.lock().clone(),
        vec![(
            "POST /api/auth/oauth2/revoke".to_owned(),
            format!("Bearer {TOKEN}")
        )]
    );
    Ok(())
}

#[tokio::test]
async fn account_routes_answer_only_on_loopback() -> TestResult {
    let root = tempfile::tempdir()?;
    let fake = fake_cloud(release()).await?;
    let node = node(root.path(), &fake, CloudAccess::Open, Vec::new()).await?;
    for (method, path) in [
        (Method::GET, "/api/account"),
        (Method::PATCH, "/api/account"),
        (Method::POST, "/api/account/signin"),
        (Method::POST, "/api/account/signin/poll"),
        (Method::POST, "/api/account/signout"),
        (Method::DELETE, "/api/account"),
    ] {
        let (status, body) = send(&node.app, method.clone(), path, "{}", "192.0.2.7:41000").await?;
        assert_eq!(status, StatusCode::FORBIDDEN, "{method} {path}: {body}");
    }
    let (status, _) = send(&node.app, Method::GET, "/api/hubs", "", "192.0.2.7:41000").await?;
    assert_eq!(status, StatusCode::OK);
    Ok(())
}

fn report(install_id: Uuid) -> CrashReport {
    CrashReport::panic(
        install_id,
        CloudOs::LinuxX86,
        CrashJob::Gateway,
        Some(PanicSite {
            file: "crates/gateway/src/routes/memory.rs",
            line: 12,
        }),
        "",
        DateTime::<Utc>::UNIX_EPOCH,
    )
}

#[tokio::test]
async fn queued_crash_reports_are_sent_once_and_dropped_when_turned_off() -> TestResult {
    let root = tempfile::tempdir()?;
    let fake = fake_cloud(release()).await?;
    let node = node(root.path(), &fake, CloudAccess::Open, Vec::new()).await?;
    let files = node.account.files().clone();
    let install_id = files.read()?.ok_or("missing")?.install_id;
    queue(&files, &report(install_id))?;
    node.account
        .tick(Utc::now(), &CancellationToken::new())
        .await;
    let sent = fake.seen.crashes.lock().clone();
    assert_eq!(sent.len(), 1);
    let parsed: CrashReport = serde_json::from_str(sent.first().ok_or("no crash")?)?;
    assert_eq!(parsed.location, "gateway::routes::memory:12");
    assert_eq!(queued(&files)?, Vec::<std::path::PathBuf>::new());

    local(
        &node.app,
        Method::PATCH,
        "/api/account",
        r#"{"crashReports":false}"#,
    )
    .await?;
    queue(&files, &report(install_id))?;
    node.account
        .tick(Utc::now(), &CancellationToken::new())
        .await;
    assert_eq!(fake.seen.crashes.lock().len(), 1);
    assert_eq!(queued(&files)?, Vec::<std::path::PathBuf>::new());
    Ok(())
}

#[tokio::test]
async fn crash_reports_go_to_the_telemetry_url_and_never_to_the_cloud() -> TestResult {
    for access in [CloudAccess::Open, CloudAccess::TelemetryOnly] {
        let root = tempfile::tempdir()?;
        let cloud = fake_cloud(release()).await?;
        let collector = fake_cloud(release()).await?;
        let node =
            node_reporting_to(root.path(), &cloud, access, Vec::new(), Some(&collector)).await?;
        let files = node.account.files().clone();
        let install_id = files.read()?.ok_or("missing")?.install_id;
        queue(&files, &report(install_id))?;
        node.account
            .tick(Utc::now(), &CancellationToken::new())
            .await;
        assert_eq!(collector.seen.crashes.lock().len(), 1, "{access:?}");
        assert_eq!(cloud.seen.crashes.lock().len(), 0, "{access:?}");
        assert_eq!(
            queued(&files)?,
            Vec::<std::path::PathBuf>::new(),
            "{access:?}"
        );
    }
    Ok(())
}

#[tokio::test]
async fn telemetry_only_keeps_reports_on_but_refuses_cloud_routes() -> TestResult {
    let root = tempfile::tempdir()?;
    let cloud = fake_cloud(release()).await?;
    let collector = fake_cloud(release()).await?;
    let node = node_reporting_to(
        root.path(),
        &cloud,
        CloudAccess::TelemetryOnly,
        Vec::new(),
        Some(&collector),
    )
    .await?;
    let shown = account(&node.app).await?;
    assert!(shown.cloud_disabled, "{shown:?}");
    assert_eq!(shown.update_source, UpdateSource::Github);
    assert_eq!((shown.crash_reports, shown.usage_counts), (true, true));
    let (status, text) = local(&node.app, Method::POST, "/api/account/signin", "{}").await?;
    assert_eq!(status, StatusCode::CONFLICT, "{text}");
    assert_eq!(cloud.seen.requests.load(Ordering::SeqCst), 0);
    Ok(())
}

#[tokio::test]
async fn a_hub_policy_turns_telemetry_off_and_keeps_it_off() -> TestResult {
    let root = tempfile::tempdir()?;
    let fake = fake_cloud(release()).await?;
    let node = node(root.path(), &fake, CloudAccess::NoTelemetry, Vec::new()).await?;
    let shown = account(&node.app).await?;
    assert_eq!((shown.crash_reports, shown.usage_counts), (false, false));
    let (status, body) = local(
        &node.app,
        Method::PATCH,
        "/api/account",
        r#"{"usageCounts":true}"#,
    )
    .await?;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    let files = node.account.files().clone();
    queue(&files, &report(files.read()?.ok_or("missing")?.install_id))?;
    node.account
        .tick(Utc::now(), &CancellationToken::new())
        .await;
    assert!(fake.seen.crashes.lock().is_empty());
    Ok(())
}

async fn signed_in(release: Release) -> Result<(tempfile::TempDir, Fake, Node), Failure> {
    let root = tempfile::tempdir()?;
    let fake = fake_cloud(release).await?;
    let node = node(root.path(), &fake, CloudAccess::Open, Vec::new()).await?;
    local(&node.app, Method::POST, "/api/account/signin", "{}").await?;
    local(&node.app, Method::POST, "/api/account/signin/poll", "").await?;
    local(&node.app, Method::POST, "/api/account/signin/poll", "").await?;
    Ok((root, fake, node))
}

#[tokio::test]
async fn a_signed_newer_release_becomes_a_pending_update_with_its_archive_kept() -> TestResult {
    let (_root, fake, node) = signed_in(release()).await?;
    node.account
        .tick(Utc::now(), &CancellationToken::new())
        .await;
    let pending = account(&node.app)
        .await?
        .pending_update
        .ok_or("no pending update")?;
    assert_eq!(pending.version, "99.0.0");
    let os = CloudOs::current().ok_or("unsupported test platform")?;
    assert_eq!(
        fake.seen.latest.lock().clone(),
        vec![format!("Bearer {TOKEN}|stable|{os}")]
    );
    let kept = std::fs::read(node.account.files().updates().join("rh-99.0.0.tar.gz"))?;
    assert_eq!(kept, ARCHIVE);
    Ok(())
}

#[tokio::test]
async fn a_release_whose_signature_fails_is_never_offered() -> TestResult {
    let (_root, _fake, node) = signed_in(Release {
        version: "99.0.0",
        signature: Signing::Forged,
    })
    .await?;
    node.account
        .tick(Utc::now(), &CancellationToken::new())
        .await;
    assert_eq!(account(&node.app).await?.pending_update, None);
    assert!(
        !node
            .account
            .files()
            .updates()
            .join("rh-99.0.0.tar.gz")
            .exists()
    );
    Ok(())
}

#[tokio::test]
async fn an_older_release_is_not_pending() -> TestResult {
    let (_root, _fake, node) = signed_in(Release {
        version: "0.0.1",
        signature: Signing::Good,
    })
    .await?;
    node.account
        .tick(Utc::now(), &CancellationToken::new())
        .await;
    assert_eq!(account(&node.app).await?.pending_update, None);
    Ok(())
}

#[tokio::test]
async fn hubs_list_the_serving_node_first_and_probe_the_others() -> TestResult {
    let root = tempfile::tempdir()?;
    let fake = fake_cloud(release()).await?;
    let closed = std::net::TcpListener::bind("127.0.0.1:0")?.local_addr()?;
    let hubs = vec![
        HubEntry {
            name: "Household Team".to_owned(),
            url: format!("http://{}", fake.stub.addr).parse()?,
            kind: HubKind::Team,
        },
        HubEntry {
            name: "lab".to_owned(),
            url: format!("http://{closed}").parse()?,
            kind: HubKind::Team,
        },
    ];
    let node = node(root.path(), &fake, CloudAccess::Open, hubs).await?;
    let (status, body) = local(&node.app, Method::GET, "/api/hubs", "").await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    let listed: Vec<Hub> = serde_json::from_str(&body)?;
    let summary: Vec<_> = listed
        .iter()
        .map(|hub| (hub.id.as_str(), hub.kind, hub.online, hub.current))
        .collect();
    assert_eq!(
        summary,
        vec![
            ("this-node", HubKind::Local, true, true),
            ("household-team", HubKind::Team, true, false),
            ("lab", HubKind::Team, false, false),
        ]
    );
    assert_eq!(
        listed
            .iter()
            .map(|hub| hub.detail.as_str())
            .take(2)
            .collect::<Vec<_>>(),
        vec!["This node", "rh 0.9.0"]
    );
    assert_eq!(
        listed.first().map(|hub| hub.name.as_str()),
        Some("desk-node")
    );
    Ok(())
}

#[test]
fn a_crash_report_holds_module_paths_and_line_numbers_only() {
    let home = "/home/user/projects/acmekit/notes.md";
    let session = "the user asked about the AcmeKit launch plan";
    let backtrace = format!(
        "   0: std::backtrace::Backtrace::force_capture\n             at /rustc/abc/library/std/src/backtrace.rs:312:13\n   1: gateway::routes::memory::search::{{{{closure}}}}::h0123456789abcdef\n             at ./crates/gateway/src/routes/memory.rs:88:21\n   2: {home}::render {session}\n   3: tokio::runtime::task::harness::poll\n             at /home/user/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/tokio-1.47.1/src/runtime/task/harness.rs:473:19\n"
    );
    let report = CrashReport::panic(
        Uuid::nil(),
        CloudOs::LinuxX86,
        CrashJob::Gateway,
        Some(PanicSite {
            file: "/home/user/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/serde_json-1.0.140/src/de.rs",
            line: 42,
        }),
        &backtrace,
        DateTime::<Utc>::UNIX_EPOCH,
    );
    assert_eq!(report.location, "serde_json::de:42");
    assert_eq!(
        report.frames,
        vec![
            "gateway::routes::memory::search::<closure>:88".to_owned(),
            "tokio::runtime::task::harness::poll:473".to_owned(),
        ]
    );
    assert_eq!(report.at, "1970-01-01T00:00:00.000Z");
    let text = serde_json::to_string(&report).unwrap_or_default();
    for leaked in ["/home", "acmekit", "AcmeKit", session] {
        assert!(!text.contains(leaked), "{leaked} in {text}");
    }
}

#[tokio::test]
async fn a_token_the_cloud_revoked_signs_this_install_out() -> TestResult {
    let (_root, _fake, node) = signed_in(Release {
        version: REVOKED,
        signature: Signing::Good,
    })
    .await?;
    node.account
        .tick(Utc::now(), &CancellationToken::new())
        .await;
    let shown = account(&node.app).await?;
    assert_eq!(
        (shown.email, shown.update_source),
        (None, UpdateSource::Github)
    );
    Ok(())
}

#[tokio::test]
async fn a_disabled_cloud_is_never_called_and_refuses_cloud_routes() -> TestResult {
    let (root, fake, node) = signed_in(release()).await?;
    let files = node.account.files().clone();
    drop(node);
    let node = self::node(root.path(), &fake, CloudAccess::Disabled, Vec::new()).await?;
    let before = fake.seen.requests.load(Ordering::SeqCst);

    let shown = account(&node.app).await?;
    assert!(shown.cloud_disabled);
    assert_eq!(shown.update_source, UpdateSource::Github);
    assert_eq!((shown.crash_reports, shown.usage_counts), (false, false));
    for (method, path, body) in [
        (Method::POST, "/api/account/signin", "{}"),
        (Method::POST, "/api/account/signin/poll", ""),
        (
            Method::PATCH,
            "/api/account",
            r#"{"updateSource":"rivethub-cloud"}"#,
        ),
        (Method::DELETE, "/api/account", ""),
    ] {
        let (status, text) = local(&node.app, method.clone(), path, body).await?;
        assert_eq!(status, StatusCode::CONFLICT, "{method} {path}: {text}");
        assert!(
            text.contains("cloud sign-in is disabled by this hub's policy"),
            "{method} {path}: {text}"
        );
    }

    queue(&files, &report(files.read()?.ok_or("missing")?.install_id))?;
    node.account.count(gateway::account::UsageEvent::Search);
    node.account
        .tick(Utc::now(), &CancellationToken::new())
        .await;
    let (status, text) = local(&node.app, Method::POST, "/api/account/signout", "").await?;
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(account(&node.app).await?.email, None);
    assert_eq!(fake.seen.requests.load(Ordering::SeqCst), before);
    Ok(())
}

async fn healthz(yaml: &str) -> Result<String, Failure> {
    let home = tempfile::tempdir()?;
    let env = config::Environment::from_vars([("HOME".as_ref(), home.path().as_os_str())]);
    let document: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml)?;
    let config = config::type_document(&document)?.config;
    let settings =
        gateway::Settings::from_config_in(&config, &env, "host".to_owned(), "node".to_owned())?;
    let app = Arc::new(Gateway::new(
        Arc::new(testkit::FakeMemoryStore::default()),
        settings,
    ))
    .router(true);
    let (status, body) = local(&app, Method::GET, "/healthz", "").await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    Ok(body)
}

#[tokio::test]
async fn healthz_states_a_hub_policy_only_when_one_is_set() -> TestResult {
    let open = healthz("memory:\n  backend: sqlite\ntelemetry:\n  disabled: true\n").await?;
    assert!(!open.contains("policy"), "{open}");
    for (yaml, policy) in [
        (
            "memory:\n  backend: sqlite\n  share:\n    allowed_servers: [team.example.com]\ncloud:\n  disabled: true\n",
            r#""policy":{"cloud":"disabled","shareServers":["team.example.com"]}"#,
        ),
        (
            "cloud:\n  disabled: true\n",
            r#""policy":{"cloud":"disabled"}"#,
        ),
        (
            "memory:\n  share:\n    allowed_servers: [team.example.com]\n",
            r#""policy":{"cloud":"enabled","shareServers":["team.example.com"]}"#,
        ),
    ] {
        let body = healthz(yaml).await?;
        assert!(body.contains(policy), "{yaml}: {body}");
    }
    Ok(())
}
