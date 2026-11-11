#![cfg(test)]

use std::collections::BTreeMap;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::Path as UrlPath;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::http::StatusCode;
use axum::http::Uri;
use axum::response::IntoResponse;
use axum::response::Response;
use cli::Exit;
use config::Environment;
use gateway::tenancy::Tenancy;
use gateway::tenancy::UserId;
use memory_postgres::PostgresMemory;
use parking_lot::Mutex;
use sqlx::PgPool;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use wire::capture::BundledSession;
use wire::capture::Receipt;
use wire::capture::ReceiptVerification;

use crate::support;

const CANARY: &str = "p2canary-7f3a9c";
const DROP_ID: &str = "Qm9iQm9iQm9iQm9iQm9iQg";

#[derive(Default)]
struct Seen {
    uris: Vec<String>,
    tokens: Vec<String>,
    bodies: Vec<Vec<u8>>,
    gets: usize,
    upload_status: Option<StatusCode>,
}

#[derive(Clone, Default)]
struct DropBox(Arc<Mutex<Seen>>);

async fn upload(
    State(drops): State<DropBox>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let refused = {
        let mut seen = drops.0.lock();
        seen.uris.push(uri.to_string());
        seen.tokens.push(token);
        if seen.upload_status.is_none() {
            seen.bodies.push(body.to_vec());
        }
        seen.upload_status
    };
    if let Some(status) = refused {
        return (status, "refused").into_response();
    }
    let bytes = body.len();
    (
        StatusCode::CREATED,
        format!(r#"{{"id":"{DROP_ID}","expires_at":"2026-11-17T12:00:00Z","bytes":{bytes}}}"#),
    )
        .into_response()
}

async fn download(
    State(drops): State<DropBox>,
    uri: Uri,
    UrlPath(id): UrlPath<String>,
) -> Response {
    let mut seen = drops.0.lock();
    seen.uris.push(uri.to_string());
    seen.gets += 1;
    if id != DROP_ID {
        return StatusCode::NOT_FOUND.into_response();
    }
    match seen.bodies.pop() {
        Some(body) => (StatusCode::OK, body).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

struct Served {
    drops: DropBox,
    url: String,
    stop: CancellationToken,
}

async fn drop_box() -> Served {
    let drops = DropBox::default();
    let app = Router::new()
        .route("/api/drops", axum::routing::post(upload))
        .route("/api/drops/{id}", axum::routing::get(download))
        .with_state(drops.clone());
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("listener");
    let url = format!("http://{}/", listener.local_addr().expect("address"));
    let stop = CancellationToken::new();
    let token = stop.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { token.cancelled().await })
            .await
    });
    Served { drops, url, stop }
}

struct Node {
    home: TempDir,
    environment: Environment,
}

impl Node {
    fn new(user: &str, cloud: &str, gateway_port: Option<u16>) -> Self {
        let home = support::home();
        let users = home.path().join("users.json");
        std::fs::write(
            &users,
            format!(r#"{{"ownerUserId":"{user}","unmappedIsOwner":true,"users":{{}}}}"#),
        )
        .expect("users file");
        let gateway = gateway_port
            .map(|port| format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"))
            .unwrap_or_default();
        support::config(
            &home,
            &format!("{gateway}memory:\n  share:\n    cloud:\n      url: {cloud}\n"),
        );
        let environment = support::environment(&home)
            .with_override("RIVETHUB_USERS_FILE", users.display().to_string())
            .with_override("RIVETHUB_NODE_NAME", "desk")
            .with_override(
                "XDG_STATE_HOME",
                home.path().join("state").display().to_string(),
            )
            .with_override(
                "XDG_CONFIG_HOME",
                home.path().join("config").display().to_string(),
            );
        Self { home, environment }
    }

    fn sign_in(&self, cloud: &str) {
        let path = self.home.path().join("state/rivethub/cloud.json");
        std::fs::create_dir_all(path.parent().expect("parent")).expect("state dir");
        std::fs::write(
            &path,
            format!(
                r#"{{"url":"{cloud}","access_token":"rhca_throwaway","refresh_token":"rhcr_throwaway","expires_at":"2099-01-01T00:00:00Z","scope":"drops:write"}}"#
            ),
        )
        .expect("cloud login");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }

    async fn run(&self, args: &[&str]) -> cli::Report {
        support::run(args, &self.environment).await
    }

    fn inbox(&self) -> PathBuf {
        self.home.path().join("state/rivethub/inbox")
    }
}

fn session(settings: Option<wire::JsonObject>) -> BundledSession {
    BundledSession {
        origin: wire::capture::SessionOrigin {
            author: "phil".to_owned(),
            node: "desk".to_owned(),
            conversation_id: "c1".to_owned(),
            session_key: "claude-code:one".to_owned(),
            agent: "rivet".to_owned(),
            created_at: None,
            updated_at: None,
            identity: None,
        },
        batch: wire::capture::CaptureBatchRequest {
            session_key: "claude-code:one".to_owned(),
            agent: "rivet".to_owned(),
            channel: None,
            title: Some("set up AcmeKit".to_owned()),
            settings,
            task_id: None,
            finalize: None,
            created_at: None,
            updated_at: None,
            messages: vec![wire::capture::CaptureMessage {
                event_id: "evt-1".to_owned(),
                role: wire::message::Role::User,
                content: format!("the build plan {CANARY}"),
                tool_name: None,
                tool_args: None,
                tool_result: None,
                metadata: None,
                created_at: None,
            }],
        },
    }
}

async fn sanitized(node: &Node, outside: &Path, settings: Option<wire::JsonObject>) -> PathBuf {
    let raw = outside.join("raw.json");
    let sealed = capture::bundle::seal(
        wire::capture::BundleAuthor {
            user: "phil".to_owned(),
            node: "desk".to_owned(),
        },
        chrono::DateTime::UNIX_EPOCH,
        wire::capture::Sanitization::Raw,
        vec![session(settings)],
    )
    .expect("seal");
    std::fs::write(&raw, sealed.text).expect("raw bundle");
    let out = node.home.path().join("s.clean.json");
    let report = node
        .run(&[
            "memory",
            "sanitize",
            &raw.display().to_string(),
            "--out",
            &out.display().to_string(),
        ])
        .await;
    assert_eq!(report.exit, Exit::Ok, "{}", report.stderr);
    out
}

fn files(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("read dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.insert(path.clone(), std::fs::read(&path).expect("read"));
            }
        }
    }
    found
}

fn holds(bytes: &[u8], needle: &str) -> bool {
    bytes
        .windows(needle.len())
        .any(|window| window == needle.as_bytes())
}

fn code_of(stdout: &str) -> String {
    stdout
        .split_whitespace()
        .find(|word| word.starts_with("rhb1:"))
        .expect("a share code")
        .to_owned()
}

#[tokio::test]
async fn share_writes_no_plaintext() {
    let served = drop_box().await;
    let outside = tempfile::tempdir().expect("outside");
    let sender = Node::new("phil", &served.url, None);
    sender.sign_in(&served.url);
    let bundle = sanitized(&sender, outside.path(), None).await;
    let before = files(sender.home.path());
    let shared = sender
        .run(&["memory", "share", &bundle.display().to_string()])
        .await;
    assert_eq!(shared.exit, Exit::Ok, "{}", shared.stderr);
    assert!(
        shared
            .stdout
            .starts_with("sealed 1 session (signed ed25519:"),
        "{}",
        shared.stdout
    );
    assert!(shared.stdout.contains(
        "; share code (works once, expires 2026-11-17; send it privately): rhb1:Qm9iQm9iQm9iQm9iQm9iQg#"
    ));
    assert!(
        !bundle.exists(),
        "the sanitized bundle is unlinked after a 201"
    );
    let after = files(sender.home.path());
    let added: Vec<&PathBuf> = after
        .keys()
        .filter(|path| !before.contains_key(*path))
        .collect();
    assert_eq!(
        added,
        [&sender.home.path().join("state/rivethub/node-signing.key")],
        "share writes only the signing key"
    );
    for (path, bytes) in &after {
        if before.get(path) != Some(bytes) {
            assert!(!holds(bytes, CANARY), "{} holds plaintext", path.display());
        }
    }
    let seen = served.drops.0.lock();
    assert_eq!(seen.tokens, ["Bearer rhca_throwaway"]);
    let body = seen.bodies.first().expect("uploaded");
    assert!(body.starts_with(b"age-encryption.org/v1\n"));
    assert!(!holds(body, CANARY));
    drop(seen);
    served.stop.cancel();
}

#[tokio::test]
async fn the_code_key_never_reaches_a_url() {
    let served = drop_box().await;
    let outside = tempfile::tempdir().expect("outside");
    let sender = Node::new("phil", &served.url, None);
    sender.sign_in(&served.url);
    let bundle = sanitized(&sender, outside.path(), None).await;
    let shared = sender
        .run(&["memory", "share", &bundle.display().to_string()])
        .await;
    assert_eq!(shared.exit, Exit::Ok, "{}", shared.stderr);
    let code = code_of(&shared.stdout);
    let key = code.split_once('#').expect("key part").1.to_owned();
    let alex = Node::new("alex", &served.url, None);
    let received = alex.run(&["memory", "receive", &code, "--dry-run"]).await;
    assert_eq!(received.exit, Exit::Ok, "{}", received.stderr);
    assert!(
        received
            .stdout
            .starts_with("would import 1 session from phil (unverified: signed by ed25519:"),
        "{}",
        received.stdout
    );
    let seen = served.drops.0.lock();
    assert_eq!(seen.uris, ["/api/drops", &format!("/api/drops/{DROP_ID}")]);
    for uri in &seen.uris {
        assert!(!uri.contains('#') && !uri.contains(&key), "{uri}");
    }
    drop(seen);
    served.stop.cancel();
}

#[tokio::test]
async fn a_typo_is_refused_before_any_request() {
    let served = drop_box().await;
    let receiver = Node::new("alex", &served.url, None);
    let typo = format!("rhb1:{DROP_ID}#qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq");
    let report = receiver.run(&["memory", "receive", &typo]).await;
    assert_eq!(report.exit, Exit::Failure);
    assert!(
        report.stderr.contains("fails its checksum"),
        "{}",
        report.stderr
    );
    assert_eq!(served.drops.0.lock().uris.len(), 0);
    served.stop.cancel();
}

#[tokio::test]
async fn a_refused_upload_prints_no_code_and_keeps_the_bundle() {
    for status in [
        StatusCode::PAYLOAD_TOO_LARGE,
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::SERVICE_UNAVAILABLE,
    ] {
        let served = drop_box().await;
        served.drops.0.lock().upload_status = Some(status);
        let outside = tempfile::tempdir().expect("outside");
        let sender = Node::new("phil", &served.url, None);
        sender.sign_in(&served.url);
        let bundle = sanitized(&sender, outside.path(), None).await;
        let shared = sender
            .run(&["memory", "share", &bundle.display().to_string()])
            .await;
        assert_eq!(shared.exit, Exit::Failure, "{status}");
        assert!(!shared.stdout.contains("rhb1:") && !shared.stderr.contains("rhb1:"));
        assert!(
            shared.stderr.contains(&status.as_u16().to_string()),
            "{}",
            shared.stderr
        );
        assert!(bundle.exists(), "the bundle stays for a retry");
        served.stop.cancel();
    }
}

#[tokio::test]
async fn a_signed_out_sender_uploads_nothing() {
    let served = drop_box().await;
    let outside = tempfile::tempdir().expect("outside");
    let sender = Node::new("phil", &served.url, None);
    let bundle = sanitized(&sender, outside.path(), None).await;
    let shared = sender
        .run(&["memory", "share", &bundle.display().to_string()])
        .await;
    assert_eq!(shared.exit, Exit::Failure);
    assert!(
        shared.stderr.contains("rh cloud login"),
        "{}",
        shared.stderr
    );
    assert!(bundle.exists());
    assert_eq!(served.drops.0.lock().uris.len(), 0);
    served.stop.cancel();
}

#[tokio::test]
async fn receive_keeps_ciphertext_on_failure() {
    let served = drop_box().await;
    let outside = tempfile::tempdir().expect("outside");
    let sender = Node::new("phil", &served.url, None);
    sender.sign_in(&served.url);
    let bundle = sanitized(&sender, outside.path(), None).await;
    let shared = sender
        .run(&["memory", "share", &bundle.display().to_string()])
        .await;
    let code = code_of(&shared.stdout);
    let closed = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("port")
        .local_addr()
        .expect("address")
        .port();
    let receiver = Node::new("alex", &served.url, Some(closed));
    let failed = receiver.run(&["memory", "receive", &code]).await;
    assert_eq!(failed.exit, Exit::Failure, "{}", failed.stdout);
    let kept = receiver.inbox().join(format!("{DROP_ID}.age"));
    let ciphertext = std::fs::read(&kept).expect("the ciphertext stays in the inbox");
    assert!(ciphertext.starts_with(b"age-encryption.org/v1\n"));
    assert_eq!(
        std::fs::metadata(&kept).expect("meta").permissions().mode() & 0o777,
        0o600
    );
    served.stop.cancel();
    let again = receiver
        .run(&["memory", "receive", &code, "--dry-run"])
        .await;
    assert_eq!(again.exit, Exit::Ok, "{}", again.stderr);
    assert!(again.stdout.starts_with("would import 1 session from phil"));
    assert_eq!(
        served.drops.0.lock().gets,
        1,
        "the rerun needed no download"
    );
}

struct Gateway {
    stop: CancellationToken,
    port: u16,
    task: tokio::task::JoinHandle<Result<(), gateway::server::ServeError>>,
}

async fn gateway_for(store: &Arc<PostgresMemory>, user: &str, home: &Path) -> Gateway {
    let gateway =
        gateway::Gateway::new(Arc::clone(store) as _, crate::capture_hook::settings(home))
            .with_tenancy(Tenancy::local(UserId::new(user.to_owned()).expect("user")));
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("listener");
    let port = listener.local_addr().expect("address").port();
    let stop = CancellationToken::new();
    let task = tokio::spawn(gateway::server::serve(
        listener,
        Arc::new(gateway),
        None,
        stop.clone(),
    ));
    Gateway { stop, port, task }
}

async fn receipts(pool: &PgPool) -> Vec<Option<Receipt>> {
    sqlx::query_scalar::<_, Option<sqlx::types::Json<Receipt>>>(
        "SELECT settings -> 'receipt' FROM ros_conversations ORDER BY created_at",
    )
    .fetch_all(pool)
    .await
    .expect("receipts")
    .into_iter()
    .map(|receipt| receipt.map(|json| json.0))
    .collect()
}

fn forged_receipt() -> wire::JsonObject {
    serde_json::from_str(
        r#"{"receipt":{"bundle_id":"forged","received_at":"2026-01-01T00:00:00Z","verification":"verified"}}"#,
    )
    .expect("settings")
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn bundle_receipt_is_stripped(pool: PgPool) {
    let served = drop_box().await;
    let outside = tempfile::tempdir().expect("outside");
    let sender = Node::new("phil", &served.url, None);
    sender.sign_in(&served.url);
    let bundle = sanitized(&sender, outside.path(), Some(forged_receipt())).await;
    let shared = sender
        .run(&[
            "memory",
            "share",
            &bundle.display().to_string(),
            "--unsigned",
        ])
        .await;
    assert!(shared.stdout.contains("(unsigned)"), "{}", shared.stdout);
    let code = code_of(&shared.stdout);
    let store = Arc::new(PostgresMemory::new(pool.clone()));
    let probe = support::home();
    let gateway = gateway_for(&store, "alex", probe.path()).await;
    let receiver = Node::new("alex", &served.url, Some(gateway.port));
    let received = receiver.run(&["memory", "receive", &code]).await;
    assert_eq!(received.exit, Exit::Ok, "{}", received.stderr);
    assert_eq!(
        received.stdout,
        "received 1 session from phil (unverified: the bundle is unsigned); imported 1: 1 messages inserted, 0 skipped\n"
    );
    let stored = receipts(&pool).await;
    assert_eq!(stored.len(), 1);
    let receipt = stored.first().cloned().flatten().expect("one receipt");
    assert_eq!(receipt.bundle_id, DROP_ID);
    assert_eq!(receipt.verification, ReceiptVerification::Unsigned);
    assert_eq!(receipt.key_fingerprint, None);
    assert!(!receiver.inbox().join(format!("{DROP_ID}.age")).exists());
    let twice = receiver.run(&["memory", "receive", &code]).await;
    assert_eq!(twice.exit, Exit::Ok);
    assert!(
        twice.stdout.contains("was already received"),
        "{}",
        twice.stdout
    );
    assert_eq!(served.drops.0.lock().gets, 1, "no second download");
    gateway.stop.cancel();
    tokio::time::timeout(Duration::from_secs(5), gateway.task)
        .await
        .expect("gateway stops")
        .expect("gateway task")
        .expect("gateway");
    served.stop.cancel();
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn a_trusted_signed_bundle_imports_as_verified(pool: PgPool) {
    let served = drop_box().await;
    let outside = tempfile::tempdir().expect("outside");
    let sender = Node::new("phil", &served.url, None);
    sender.sign_in(&served.url);
    let shown = sender.run(&["memory", "key", "show"]).await;
    let key = shown.stdout.lines().next().expect("key line").to_owned();
    let bundle = sanitized(&sender, outside.path(), None).await;
    let shared = sender
        .run(&["memory", "share", &bundle.display().to_string()])
        .await;
    let code = code_of(&shared.stdout);
    let store = Arc::new(PostgresMemory::new(pool.clone()));
    let probe = support::home();
    let gateway = gateway_for(&store, "alex", probe.path()).await;
    let receiver = Node::new("alex", &served.url, Some(gateway.port));
    let trusted = receiver
        .run(&["memory", "trust", "add", "phil", &key])
        .await;
    assert_eq!(trusted.exit, Exit::Ok, "{}", trusted.stderr);
    let received = receiver.run(&["memory", "receive", &code]).await;
    assert_eq!(received.exit, Exit::Ok, "{}", received.stderr);
    assert!(
        received
            .stdout
            .starts_with("received 1 session from phil (verified, signed by ed25519:"),
        "{}",
        received.stdout
    );
    let stored = receipts(&pool).await;
    let receipt = stored.first().cloned().flatten().expect("one receipt");
    assert_eq!(receipt.verification, ReceiptVerification::Verified);
    assert!(
        receipt
            .key_fingerprint
            .is_some_and(|text| text.starts_with("ed25519:"))
    );
    gateway.stop.cancel();
    drop(tokio::time::timeout(Duration::from_secs(5), gateway.task).await);
    served.stop.cancel();
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn ingest_bundle_strips_a_bundle_receipt(pool: PgPool) {
    let outside = tempfile::tempdir().expect("outside");
    let store = Arc::new(PostgresMemory::new(pool.clone()));
    let probe = support::home();
    let gateway = gateway_for(&store, "alex", probe.path()).await;
    let receiver = Node::new("alex", "http://127.0.0.1:9/", Some(gateway.port));
    let bundle = sanitized(&receiver, outside.path(), Some(forged_receipt())).await;
    let ingested = receiver
        .run(&["memory", "ingest-bundle", &bundle.display().to_string()])
        .await;
    assert_eq!(ingested.exit, Exit::Ok, "{}", ingested.stderr);
    assert_eq!(receipts(&pool).await, [None]);
    gateway.stop.cancel();
    drop(tokio::time::timeout(Duration::from_secs(5), gateway.task).await);
}

#[tokio::test]
async fn a_truncated_download_is_a_burned_drop_and_leaves_nothing() {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("listener");
    let url = format!("http://{}/", listener.local_addr().expect("address"));
    let server = tokio::spawn(async move {
        use tokio::io::AsyncReadExt as _;
        use tokio::io::AsyncWriteExt as _;
        let (mut socket, _) = listener.accept().await.expect("accept");
        let mut request = [0u8; 4096];
        let read = socket.read(&mut request).await.expect("request");
        socket
            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 4096\r\ncontent-type: application/octet-stream\r\n\r\nage-encryption.org/v1\n")
            .await
            .expect("partial body");
        drop(socket);
        String::from_utf8_lossy(request.get(..read).unwrap_or_default()).into_owned()
    });
    let receiver = Node::new("alex", &url, None);
    let code = format!(
        "rhb1:{DROP_ID}#{}",
        capture::seal::ShareKey::generate().reveal()
    );
    let report = receiver.run(&["memory", "receive", &code]).await;
    assert_eq!(report.exit, Exit::Failure);
    assert!(
        report
            .stderr
            .contains("a drop burns once its download starts, so ask the sender to share it again"),
        "{}",
        report.stderr
    );
    let request = server.await.expect("server");
    assert!(
        request.starts_with(&format!("GET /api/drops/{DROP_ID} HTTP/1.1")),
        "{request}"
    );
    let left: Vec<_> = std::fs::read_dir(receiver.inbox())
        .expect("inbox")
        .map(|entry| entry.expect("entry").file_name())
        .collect();
    assert_eq!(left, Vec::<std::ffi::OsString>::new());
}
