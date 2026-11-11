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
    taken: Vec<bool>,
    gets: usize,
    refreshes: Vec<String>,
    revokes: Vec<String>,
    revoke_status: Option<StatusCode>,
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
            seen.taken.push(false);
        }
        (seen.upload_status, seen.bodies.len())
    };
    let (refused, count) = refused;
    if let Some(status) = refused {
        return (status, "refused").into_response();
    }
    let bytes = body.len();
    (
        StatusCode::CREATED,
        format!(
            r#"{{"id":"{}","expires_at":"2026-11-17T12:00:00Z","bytes":{bytes}}}"#,
            drop_id(count - 1)
        ),
    )
        .into_response()
}

async fn download(
    State(drops): State<DropBox>,
    uri: Uri,
    UrlPath(id): UrlPath<String>,
) -> Response {
    let body = {
        let mut seen = drops.0.lock();
        seen.uris.push(uri.to_string());
        seen.gets += 1;
        let found = (0..seen.bodies.len()).find(|index| drop_id(*index) == id);
        match found {
            Some(index) if seen.taken.get(index) == Some(&false) => {
                if let Some(taken) = seen.taken.get_mut(index) {
                    *taken = true;
                }
                seen.bodies.get(index).cloned()
            }
            Some(_) | None => None,
        }
    };
    match body {
        Some(body) => (StatusCode::OK, body).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[derive(serde::Deserialize)]
struct Refresh {
    refresh_token: String,
}

async fn refresh(State(drops): State<DropBox>, axum::Json(asked): axum::Json<Refresh>) -> Response {
    let asked_for = asked.refresh_token;
    let accepted = {
        let mut seen = drops.0.lock();
        let current = seen.refreshes.last().map_or_else(
            || "rhcr_throwaway".to_owned(),
            |last| format!("rhcr_{last}"),
        );
        seen.refreshes.push(asked_for.clone());
        drop(seen);
        asked_for == current
    };
    if !accepted {
        return (StatusCode::BAD_REQUEST, r#"{"error":"invalid_grant"}"#).into_response();
    }
    (
        StatusCode::OK,
        format!(
            r#"{{"access_token":"rhca_{asked_for}","token_type":"Bearer","expires_in":900,"refresh_token":"rhcr_{asked_for}","scope":"drops:write"}}"#
        ),
    )
        .into_response()
}

#[derive(serde::Deserialize)]
struct Revoked {
    token: String,
}

async fn revoke(State(drops): State<DropBox>, axum::Json(asked): axum::Json<Revoked>) -> Response {
    let status = {
        let mut seen = drops.0.lock();
        seen.revokes.push(asked.token);
        seen.revoke_status
    };
    match status {
        Some(status) => (status, "down").into_response(),
        None => (StatusCode::OK, "{}").into_response(),
    }
}

fn drop_id(index: usize) -> String {
    if index == 0 {
        DROP_ID.to_owned()
    } else {
        format!("{}{index:02}", &DROP_ID[..20])
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
        .route("/api/cli/token", axum::routing::post(refresh))
        .route("/api/cli/revoke", axum::routing::post(revoke))
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
            &format!("{gateway}memory:\n  share:\n    server:\n      url: {cloud}\n"),
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

    fn with_owner(self, token: &gateway::local_owner::OwnerToken) -> Self {
        support::install_owner_token(&self.environment, token);
        self
    }

    fn sign_in_also(&self, cloud: &str) {
        let path = self.home.path().join("state/rivethub/share-logins.json");
        let text = std::fs::read_to_string(&path).expect("logins");
        let entry = r#"{"access_token":"rhca_throwaway","refresh_token":"rhcr_throwaway","expires_at":"2099-01-01T00:00:00Z","scope":"drops:write"}"#;
        let joined = text.replacen(
            r#"{"logins":{"#,
            &format!(r#"{{"logins":{{"{}":{entry},"#, host_of(cloud)),
            1,
        );
        std::fs::write(&path, joined).expect("logins");
    }

    fn sign_in(&self, cloud: &str) {
        self.sign_in_until(cloud, "2099-01-01T00:00:00Z");
    }

    fn sign_in_until(&self, cloud: &str, expires_at: &str) {
        let path = self.home.path().join("state/rivethub/share-logins.json");
        let host = host_of(cloud);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("state dir");
        std::fs::write(
            &path,
            format!(
                r#"{{"logins":{{"{host}":{{"access_token":"rhca_throwaway","refresh_token":"rhcr_throwaway","expires_at":"{expires_at}","scope":"drops:write"}}}}}}"#
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
    assert!(shared.stdout.contains(&format!(
        "; share code (works once, expires 2026-11-17; send it privately): rhb1:{}/Qm9iQm9iQm9iQm9iQm9iQg#",
        host_of(&served.url)
    )));
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
    let typo = format!(
        "rhb1:{}/{DROP_ID}#qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq",
        host_of(&served.url)
    );
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
        shared.stderr.contains("rh share login"),
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
    let kept = receiver.inbox().join(inbox_name(&served.url, "age"));
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
    owner: gateway::local_owner::OwnerToken,
    stop: CancellationToken,
    port: u16,
    task: tokio::task::JoinHandle<Result<(), gateway::server::ServeError>>,
}

async fn gateway_for(store: &Arc<PostgresMemory>, user: &str, home: &Path) -> Gateway {
    let holder = Arc::new(gateway::local_owner::LocalOwner::unpersisted().expect("owner token"));
    let owner = holder.current();
    let gateway =
        gateway::Gateway::new(Arc::clone(store) as _, crate::capture_hook::settings(home))
            .with_local_owner(holder)
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
    Gateway {
        owner,
        stop,
        port,
        task,
    }
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
    let receiver = Node::new("alex", &served.url, Some(gateway.port)).with_owner(&gateway.owner);
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
    assert!(
        !receiver
            .inbox()
            .join(inbox_name(&served.url, "age"))
            .exists()
    );
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
    let receiver = Node::new("alex", &served.url, Some(gateway.port)).with_owner(&gateway.owner);
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
    let receiver =
        Node::new("alex", "http://127.0.0.1:9/", Some(gateway.port)).with_owner(&gateway.owner);
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
        "rhb1:{}/{DROP_ID}#{}",
        host_of(&url),
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

async fn messages_by_verification(pool: &PgPool) -> Vec<(Option<String>, i64)> {
    sqlx::query_as::<_, (Option<String>, i64)>(
        "SELECT c.settings -> 'receipt' ->> 'verification', count(m.id)
         FROM ros_conversations c JOIN ros_messages m ON m.conversation_id = c.id
         GROUP BY 1 ORDER BY 1",
    )
    .fetch_all(pool)
    .await
    .expect("conversations")
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn an_unsigned_bundle_cannot_join_a_verified_conversation(pool: PgPool) {
    let served = drop_box().await;
    let outside = tempfile::tempdir().expect("outside");
    let alice = Node::new("phil", &served.url, None);
    alice.sign_in(&served.url);
    let key = alice.run(&["memory", "key", "show"]).await.stdout;
    let key = key.lines().next().expect("key line").to_owned();
    let bundle = sanitized(&alice, outside.path(), None).await;
    let mut forged =
        capture::bundle::parse(&std::fs::read_to_string(&bundle).expect("bundle")).expect("parse");
    let shared = alice
        .run(&["memory", "share", &bundle.display().to_string()])
        .await;
    let verified_code = code_of(&shared.stdout);

    let session = forged.sessions.first_mut().expect("session");
    session.origin.updated_at = Some(chrono::DateTime::UNIX_EPOCH);
    let message = session.batch.messages.first_mut().expect("message");
    message.event_id = "mallory-evt".to_owned();
    message.content = "planted by mallory".to_owned();
    let resealed = capture::bundle::seal(
        forged.manifest.exported_by.clone(),
        forged.manifest.exported_at,
        forged.manifest.sanitization.clone(),
        forged.sessions,
    )
    .expect("reseal");
    let mallory = Node::new("phil", &served.url, None);
    mallory.sign_in(&served.url);
    let planted = mallory.home.path().join("forged.clean.json");
    std::fs::write(&planted, resealed.text).expect("forged bundle");
    let forged_share = mallory
        .run(&[
            "memory",
            "share",
            &planted.display().to_string(),
            "--unsigned",
        ])
        .await;
    assert_eq!(forged_share.exit, Exit::Ok, "{}", forged_share.stderr);
    let forged_code = code_of(&forged_share.stdout);

    let store = Arc::new(PostgresMemory::new(pool.clone()));
    let probe = support::home();
    let gateway = gateway_for(&store, "alex", probe.path()).await;
    let receiver = Node::new("alex", &served.url, Some(gateway.port)).with_owner(&gateway.owner);
    let trusted = receiver
        .run(&["memory", "trust", "add", "phil", &key])
        .await;
    assert_eq!(trusted.exit, Exit::Ok, "{}", trusted.stderr);
    let first = receiver.run(&["memory", "receive", &verified_code]).await;
    assert!(
        first.stdout.contains("(verified, signed by"),
        "{}",
        first.stdout
    );
    let second = receiver.run(&["memory", "receive", &forged_code]).await;
    assert_eq!(second.exit, Exit::Ok, "{}", second.stderr);
    assert!(
        second
            .stdout
            .contains("(unverified: the bundle is unsigned)"),
        "{}",
        second.stdout
    );
    assert_eq!(
        messages_by_verification(&pool).await,
        [
            (Some("unsigned".to_owned()), 1),
            (Some("verified".to_owned()), 1)
        ],
        "the forged message lands in a conversation of its own"
    );
    gateway.stop.cancel();
    drop(tokio::time::timeout(Duration::from_secs(5), gateway.task).await);
    served.stop.cancel();
}

#[tokio::test]
async fn concurrent_shares_refresh_the_login_once() {
    let served = drop_box().await;
    let outside = tempfile::tempdir().expect("outside");
    let sender = Node::new("phil", &served.url, None);
    sender.sign_in_until(&served.url, "2020-01-01T00:00:00Z");
    let first = sanitized(&sender, outside.path(), None).await;
    let one = first.with_file_name("one.clean.json");
    std::fs::rename(&first, &one).expect("rename");
    let second = sanitized(&sender, outside.path(), None).await;
    let one_text = one.display().to_string();
    let two_text = second.display().to_string();
    let one_args = ["memory", "share", one_text.as_str()];
    let two_args = ["memory", "share", two_text.as_str()];
    let (a, b) = tokio::join!(sender.run(&one_args), sender.run(&two_args));
    assert_eq!(a.exit, Exit::Ok, "{}", a.stderr);
    assert_eq!(b.exit, Exit::Ok, "{}", b.stderr);
    let seen = served.drops.0.lock();
    assert_eq!(seen.refreshes, ["rhcr_throwaway"], "one refresh, no reuse");
    assert_eq!(seen.tokens.len(), 2);
    assert!(
        seen.tokens
            .iter()
            .all(|token| token == "Bearer rhca_rhcr_throwaway")
    );
    drop(seen);
    served.stop.cancel();
}

#[tokio::test]
async fn a_server_error_then_a_miss_says_the_drop_failed_midway() {
    let calls = Arc::new(Mutex::new(0_usize));
    let counted = Arc::clone(&calls);
    let app = Router::new().route(
        "/api/drops/{id}",
        axum::routing::get(move || async move {
            let mut calls = counted.lock();
            *calls += 1;
            if *calls == 1 {
                StatusCode::INTERNAL_SERVER_ERROR
            } else {
                StatusCode::NOT_FOUND
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("listener");
    let url = format!("http://{}/", listener.local_addr().expect("address"));
    let server = tokio::spawn(async move { axum::serve(listener, app).await });
    let receiver = Node::new("alex", &url, None);
    let code = format!(
        "rhb1:{}/{DROP_ID}#{}",
        host_of(&url),
        capture::seal::ShareKey::generate().reveal()
    );
    let report = receiver.run(&["memory", "receive", &code]).await;
    assert_eq!(report.exit, Exit::Failure);
    assert!(
        report
            .stderr
            .contains("failed while serving bundle Qm9iQm9iQm9iQm9iQm9iQg and the drop is now gone; ask the sender to share it again"),
        "{}",
        report.stderr
    );
    assert_eq!(*calls.lock(), 2);
    server.abort();
}

#[test]
fn a_shared_code_never_prints_its_key_in_debug() {
    let key = capture::seal::ShareKey::generate().reveal();
    let shared = cli::memory::Shared {
        sessions: 1,
        signed: None,
        uploaded: 1,
        expires_at: chrono::DateTime::UNIX_EPOCH,
        code: format!("rhb1:{DROP_ID}#{key}"),
    };
    let shown = format!("{shared:?}");
    assert!(!shown.contains(&key), "{shown}");
    assert!(shown.contains(&format!("rhb1:{DROP_ID}#..")), "{shown}");
}

fn host_of(url: &str) -> String {
    url.trim_start_matches("http://")
        .trim_end_matches('/')
        .to_owned()
}

fn logins(node: &Node) -> String {
    std::fs::read_to_string(node.home.path().join("state/rivethub/share-logins.json"))
        .expect("logins")
}

#[tokio::test]
async fn logout_keeps_the_login_when_the_revoke_fails_and_touches_only_its_server() {
    let team = drop_box().await;
    let other = drop_box().await;
    let node = Node::new("phil", &team.url, None);
    let entry = r#"{"access_token":"rhca_throwaway","refresh_token":"rhcr_throwaway","expires_at":"2099-01-01T00:00:00Z","scope":"drops:write"}"#;
    let store = node.home.path().join("state/rivethub/share-logins.json");
    std::fs::create_dir_all(store.parent().expect("parent")).expect("state dir");
    std::fs::write(
        &store,
        format!(
            r#"{{"logins":{{"{}":{entry},"{}":{entry}}}}}"#,
            host_of(&team.url),
            host_of(&other.url)
        ),
    )
    .expect("logins");
    std::fs::set_permissions(&store, std::fs::Permissions::from_mode(0o600)).expect("chmod");

    team.drops.0.lock().revoke_status = Some(StatusCode::SERVICE_UNAVAILABLE);
    let failed = node.run(&["share", "logout", &host_of(&team.url)]).await;
    assert_eq!(failed.exit, Exit::Failure);
    assert!(failed.stderr.contains("503"), "{}", failed.stderr);
    assert!(
        logins(&node).contains(&host_of(&team.url)),
        "a failed revoke keeps the login"
    );

    team.drops.0.lock().revoke_status = None;
    let done = node.run(&["share", "logout"]).await;
    assert_eq!(done.exit, Exit::Ok, "{}", done.stderr);
    let left = logins(&node);
    assert!(!left.contains(&host_of(&team.url)), "{left}");
    assert!(
        left.contains(&host_of(&other.url)),
        "the other server's login stays"
    );
    assert_eq!(
        team.drops.0.lock().revokes,
        ["rhcr_throwaway", "rhcr_throwaway"]
    );
    assert_eq!(other.drops.0.lock().revokes.len(), 0);
    let status = node.run(&["share", "status"]).await;
    assert!(
        status
            .stdout
            .contains(&format!("signed in to {}", host_of(&other.url))),
        "{}",
        status.stdout
    );
    team.stop.cancel();
    other.stop.cancel();
}

#[tokio::test]
async fn a_receiver_needs_no_share_config_and_share_takes_a_server_flag() {
    let configured = drop_box().await;
    let team = drop_box().await;
    let outside = tempfile::tempdir().expect("outside");
    let sender = Node::new("phil", &configured.url, None);
    sender.sign_in(&team.url);
    let bundle = sanitized(&sender, outside.path(), None).await;
    let shared = sender
        .run(&[
            "memory",
            "share",
            &bundle.display().to_string(),
            "--server",
            &team.url,
        ])
        .await;
    assert_eq!(shared.exit, Exit::Ok, "{}", shared.stderr);
    let code = code_of(&shared.stdout);
    assert!(
        code.starts_with(&format!("rhb1:{}/", host_of(&team.url))),
        "{code}"
    );
    assert_eq!(configured.drops.0.lock().uris.len(), 0);

    let bare = support::home();
    support::config(&bare, "");
    let env = support::environment(&bare)
        .with_override(
            "XDG_STATE_HOME",
            bare.path().join("state").display().to_string(),
        )
        .with_override(
            "XDG_CONFIG_HOME",
            bare.path().join("config").display().to_string(),
        );
    let received = support::run(&["memory", "receive", &code, "--dry-run"], &env).await;
    assert_eq!(received.exit, Exit::Ok, "{}", received.stderr);
    assert!(
        received
            .stdout
            .starts_with("would import 1 session from phil"),
        "{}",
        received.stdout
    );
    assert_eq!(team.drops.0.lock().gets, 1);
    configured.stop.cancel();
    team.stop.cancel();
}

#[tokio::test]
async fn a_bad_trusted_ca_names_its_server() {
    let served = drop_box().await;
    let node = Node::new("phil", &served.url, None);
    support::config(
        &node.home,
        &format!(
            "memory:\n  share:\n    server:\n      url: {}\n    trusted_cas:\n      - host: {}\n        ca: {}\n",
            served.url,
            host_of(&served.url),
            node.home.path().join("missing-ca.pem").display()
        ),
    );
    node.sign_in(&served.url);
    let report = node.run(&["share", "logout"]).await;
    assert_eq!(report.exit, Exit::Failure);
    assert!(
        report.stderr.contains(&format!(
            "for {} in memory.share.trusted_cas",
            host_of(&served.url)
        )),
        "{}",
        report.stderr
    );
    assert_eq!(served.drops.0.lock().revokes.len(), 0);
    served.stop.cancel();
}

fn inbox_name(url: &str, extension: &str) -> String {
    format!("{}~{DROP_ID}.{extension}", host_of(url).replace(':', "_"))
}

#[sqlx::test(migrator = "migrate::POSTGRES")]
async fn the_same_drop_id_on_two_servers_is_received_twice(pool: PgPool) {
    let first = drop_box().await;
    let second = drop_box().await;
    let outside = tempfile::tempdir().expect("outside");
    let sender = Node::new("phil", &first.url, None);
    sender.sign_in(&first.url);
    sender.sign_in_also(&second.url);
    let one = sanitized(&sender, outside.path(), None).await;
    let shared_one = sender
        .run(&["memory", "share", &one.display().to_string()])
        .await;
    let two = sanitized(&sender, outside.path(), None).await;
    let shared_two = sender
        .run(&[
            "memory",
            "share",
            &two.display().to_string(),
            "--server",
            &second.url,
        ])
        .await;
    let (code_one, code_two) = (code_of(&shared_one.stdout), code_of(&shared_two.stdout));
    assert!(
        code_one.contains(&format!("/{DROP_ID}#")) && code_two.contains(&format!("/{DROP_ID}#"))
    );
    assert_ne!(code_one, code_two);

    let store = Arc::new(PostgresMemory::new(pool.clone()));
    let probe = support::home();
    let gateway = gateway_for(&store, "alex", probe.path()).await;
    let receiver = Node::new("alex", &first.url, Some(gateway.port)).with_owner(&gateway.owner);
    let got_one = receiver.run(&["memory", "receive", &code_one]).await;
    assert_eq!(got_one.exit, Exit::Ok, "{}", got_one.stderr);
    assert!(
        got_one.stdout.starts_with("received 1 session from phil"),
        "{}",
        got_one.stdout
    );
    let got_two = receiver.run(&["memory", "receive", &code_two]).await;
    assert_eq!(got_two.exit, Exit::Ok, "{}", got_two.stderr);
    assert!(
        got_two.stdout.starts_with("received 1 session from phil"),
        "{}",
        got_two.stdout
    );
    assert_eq!(first.drops.0.lock().gets, 1);
    assert_eq!(second.drops.0.lock().gets, 1);
    for url in [&first.url, &second.url] {
        assert!(
            receiver.inbox().join(inbox_name(url, "received")).exists(),
            "{url}"
        );
    }
    gateway.stop.cancel();
    drop(tokio::time::timeout(Duration::from_secs(5), gateway.task).await);
    first.stop.cancel();
    second.stop.cancel();
}
