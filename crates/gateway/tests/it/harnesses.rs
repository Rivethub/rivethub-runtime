#![cfg(test)]
use crate::session_support;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use gateway::tenancy::Tenancy;
use gateway::uploads::{UploadOptions, UploadService};
use gateway::{Gateway, Settings};
use kernel::harness::{
    EffortOption, HarnessCapabilities, HarnessControlCapabilities, HarnessModelOption,
    HarnessModelsSource, HarnessObservationCapabilities, HarnessOffering,
};
use serde::{Deserialize, Serialize};
use session_support::{Fixture, user};
use wire::gateway::GatewayError;
use wire::gateway::uploads::{UploadCreated, UploadList};
use wire::harness::{HarnessId, HarnessOfferingList};

const READ_LIMIT: usize = 4 * 1024 * 1024;

fn settings() -> Settings {
    let scratch = PathBuf::from("/tmp/rr-gateway-unused-agents");
    Settings {
        hostname: "gold-host".to_owned(),
        node: "golden-node".to_owned(),
        allowed_origins: Vec::new(),
        allowed_hosts: Vec::new(),
        files_root: None,
        static_dir: None,
        root_redirect: None,
        state_dir: scratch.join("state"),
        directory_root: scratch.join("agents"),
        shared_dir: scratch.join("shared"),
        allowed_harnesses: None,
        devices_enabled: false,
        probe_timeout: Duration::from_millis(200),
        mesh_ca: None,
    }
}

fn tenancy() -> Tenancy {
    Tenancy::local(user("owner"))
        .with_token("alice-test-token", user("alice"))
        .expect("token")
}

fn opened(root: &Path, max_bytes: u64, ttl: Duration) -> Arc<Gateway> {
    Arc::new(
        Gateway::new(Arc::new(testkit::FakeMemoryStore::default()), settings())
            .with_tenancy(tenancy())
            .with_uploads(UploadService::new(UploadOptions {
                root: root.to_path_buf(),
                max_bytes,
                ttl,
            })),
    )
}

fn bare() -> Arc<Gateway> {
    Arc::new(
        Gateway::new(Arc::new(testkit::FakeMemoryStore::default()), settings())
            .with_tenancy(tenancy()),
    )
}

async fn call(
    gateway: &Arc<Gateway>,
    method: &str,
    path: &str,
    body: Vec<u8>,
    token: Option<&str>,
) -> (u16, Vec<u8>, Option<String>) {
    use tower::ServiceExt as _;
    let mut request = axum::http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost");
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let mut request = request.body(axum::body::Body::from(body)).expect("request");
    request.extensions_mut().insert(axum::extract::ConnectInfo(
        "127.0.0.1:1234"
            .parse::<std::net::SocketAddr>()
            .expect("address"),
    ));
    let response = gateway
        .router(false)
        .oneshot(request)
        .await
        .expect("response");
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok().map(str::to_owned));
    let bytes = axum::body::to_bytes(response.into_body(), READ_LIMIT)
        .await
        .expect("body")
        .to_vec();
    (status, bytes, content_type)
}

fn encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(*byte));
        } else {
            encoded.push('%');
            encoded.push(hex_digit(byte >> 4));
            encoded.push(hex_digit(byte & 0x0f));
        }
    }
    encoded
}

fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => char::from(b'0' + nibble),
        10..=15 => char::from(b'A' + (nibble - 10)),
        _ => char::from(b'A'),
    }
}

fn upload_path(name: &str, media_type: &str, session: Option<&str>) -> String {
    let mut path = format!(
        "/api/uploads?name={}&mediaType={}",
        encode(name),
        encode(media_type)
    );
    if let Some(session) = session {
        let session = encode(session);
        path = format!("{path}&session={session}");
    }
    path
}

async fn post_upload(
    gateway: &Arc<Gateway>,
    name: &str,
    media_type: &str,
    session: Option<&str>,
    body: &[u8],
    token: Option<&str>,
) -> (u16, Vec<u8>) {
    let (status, bytes, _) = call(
        gateway,
        "POST",
        &upload_path(name, media_type, session),
        body.to_vec(),
        token,
    )
    .await;
    (status, bytes)
}

fn error_text(bytes: &[u8]) -> String {
    serde_json::from_slice::<GatewayError>(bytes)
        .expect("error json")
        .error
}

fn created(bytes: &[u8]) -> UploadCreated {
    serde_json::from_slice(bytes).expect("upload json")
}

#[tokio::test]
async fn uploads_round_trip_legitimate_names_and_refuse_danger() {
    let root = tempfile::tempdir().expect("root");
    let gateway = opened(root.path(), 64, Duration::from_secs(60));
    for (name, media_type, extension) in [
        ("notes.md", "text/plain; charset=utf-8", "md"),
        ("café.PNG", "image/png", "png"),
        ("foo..bar", "image/svg+xml", "bar"),
        ("README", "text/plain", "bin"),
    ] {
        let (status, body) = post_upload(&gateway, name, media_type, None, b"hello", None).await;
        assert_eq!(status, 201, "{name}: {}", String::from_utf8_lossy(&body));
        let upload = created(&body);
        assert_eq!(upload.name, name);
        assert!(
            upload.uri.ends_with(&format!("/{}.{extension}", upload.id)),
            "{name}: {}",
            upload.uri
        );
        assert_eq!(std::fs::read(&upload.uri).expect("stored file"), b"hello");
        assert_eq!(upload.media_type, media_type);
        assert_eq!(upload.bytes, 5);
        assert!(upload.session.is_none());
        assert!(upload.expires_at > chrono::Utc::now());
        let (status, bytes, content_type) = call(
            &gateway,
            "GET",
            &format!("/api/uploads/{}", upload.id),
            Vec::new(),
            None,
        )
        .await;
        assert_eq!(status, 200);
        assert_eq!(bytes, b"hello");
        assert_eq!(content_type.as_deref(), Some(media_type));
    }
}

#[tokio::test]
async fn reopening_the_store_keeps_uploads_with_their_extensions() {
    let root = tempfile::tempdir().expect("root");
    let first = opened(root.path(), 64, Duration::from_secs(60));
    let (status, body) = post_upload(&first, "shot.png", "image/png", None, b"pixels", None).await;
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&body));
    let upload = created(&body);
    let reopened = opened(root.path(), 64, Duration::from_secs(60));
    let (status, body, _) = call(&reopened, "GET", "/api/uploads", Vec::new(), None).await;
    assert_eq!(status, 200);
    let list: UploadList = serde_json::from_slice(&body).expect("list");
    assert_eq!(list.uploads, vec![upload]);
}

#[tokio::test]
async fn the_turn_journal_beside_the_uploads_does_not_break_reopening() {
    let root = tempfile::tempdir().expect("root");
    let first = opened(root.path(), 64, Duration::from_secs(60));
    let (status, body) = post_upload(&first, "shot.png", "image/png", None, b"pixels", None).await;
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&body));
    let upload = created(&body);
    let journal = root.path().join("turns");
    std::fs::create_dir_all(&journal).expect("journal directory");
    std::fs::write(journal.join(format!("{}.json", "0".repeat(64))), b"[]").expect("journal");
    let reopened = opened(root.path(), 64, Duration::from_secs(60));
    let (status, body, _) = call(&reopened, "GET", "/api/uploads", Vec::new(), None).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let list: UploadList = serde_json::from_slice(&body).expect("list");
    assert_eq!(list.uploads, vec![upload]);
}

#[tokio::test]
async fn uploads_refuse_empty_oversize_bad_names_and_types() {
    let root = tempfile::tempdir().expect("root");
    let gateway = opened(root.path(), 64, Duration::from_secs(60));
    let (status, body) = post_upload(&gateway, "empty.txt", "text/plain", None, b"", None).await;
    assert_eq!(status, 400);
    assert_eq!(error_text(&body), "upload is empty");
    let (status, body) = post_upload(
        &gateway,
        "big.bin",
        "application/octet-stream",
        None,
        &[0; 65],
        None,
    )
    .await;
    assert_eq!(status, 413);
    assert_eq!(error_text(&body), "body too large");
    for name in [
        "../etc/passwd",
        "foo/bar",
        "foo\\bar",
        ".",
        "..",
        "bad\nname",
    ] {
        let (status, body) = post_upload(&gateway, name, "text/plain", None, b"x", None).await;
        assert_eq!(status, 400, "{name}");
        assert_eq!(error_text(&body), "upload name is invalid");
    }
    let (status, body, _) = call(
        &gateway,
        "POST",
        "/api/uploads?mediaType=text%2Fplain",
        b"x".to_vec(),
        None,
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(error_text(&body), "upload name is required");
    for media_type in [
        "not-a-type",
        "image/png/extra",
        "text/plain; charset=utf-8; evil=1",
    ] {
        let (status, body) = post_upload(&gateway, "shot.png", media_type, None, b"x", None).await;
        assert_eq!(status, 400, "{media_type}");
        assert_eq!(error_text(&body), "media type is invalid");
    }
    let (status, body, _) =
        call(&gateway, "GET", "/api/uploads/not-a-uuid", Vec::new(), None).await;
    assert_eq!(status, 400);
    assert_eq!(error_text(&body), "invalid upload id");
    let (status, body, _) = call(
        &gateway,
        "GET",
        "/api/uploads/00000000-0000-4000-8000-000000000000",
        Vec::new(),
        None,
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(error_text(&body), "unknown upload");
}

#[tokio::test]
async fn uploads_are_private_to_the_owner_and_authorize_sessions() {
    let root = tempfile::tempdir().expect("root");
    let gateway = opened(root.path(), 1024, Duration::from_secs(60));
    gateway
        .sessions()
        .claim(&user("owner"), "chat-1")
        .expect("claim");
    let (status, body) = post_upload(
        &gateway,
        "notes.md",
        "text/plain",
        Some("chat-1"),
        b"owner-bytes",
        None,
    )
    .await;
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&body));
    let upload = created(&body);
    assert_eq!(upload.session.as_deref(), Some("chat-1"));
    let (status, body) = post_upload(
        &gateway,
        "notes.md",
        "text/plain",
        Some("chat-1"),
        b"alice",
        Some("alice-test-token"),
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(error_text(&body), "session is owned by another user");
    let (status, body) = post_upload(
        &gateway,
        "notes.md",
        "text/plain",
        Some("missing"),
        b"alice",
        None,
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(error_text(&body), "unknown session");
}

#[tokio::test]
async fn uploads_hide_another_users_bytes() {
    let root = tempfile::tempdir().expect("root");
    let gateway = opened(root.path(), 1024, Duration::from_secs(60));
    gateway
        .sessions()
        .claim(&user("owner"), "chat-1")
        .expect("claim");
    let (status, body) = post_upload(
        &gateway,
        "notes.md",
        "text/plain",
        Some("chat-1"),
        b"owner-bytes",
        None,
    )
    .await;
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&body));
    let upload = created(&body);
    let (status, body, _) = call(
        &gateway,
        "GET",
        &format!("/api/uploads/{}", upload.id),
        Vec::new(),
        Some("alice-test-token"),
    )
    .await;
    assert_eq!(status, 404);
    assert!(
        !body
            .windows(b"owner-bytes".len())
            .any(|window| window == b"owner-bytes")
    );
    let (status, body, _) = call(
        &gateway,
        "DELETE",
        &format!("/api/uploads/{}", upload.id),
        Vec::new(),
        Some("alice-test-token"),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(error_text(&body), "unknown upload");
    let (status, listed, _) = call(
        &gateway,
        "GET",
        "/api/uploads?session=chat-1",
        Vec::new(),
        Some("alice-test-token"),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_slice::<UploadList>(&listed)
            .expect("list")
            .uploads,
        Vec::new()
    );
    let (status, body, _) = call(
        &gateway,
        "DELETE",
        &format!("/api/uploads/{}", upload.id),
        Vec::new(),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        serde_json::from_slice::<Accepted>(&body).expect("accepted"),
        Accepted { ok: true }
    );
    let (status, _, _) = call(
        &gateway,
        "GET",
        &format!("/api/uploads/{}", upload.id),
        Vec::new(),
        None,
    )
    .await;
    assert_eq!(status, 404);
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Accepted {
    ok: bool,
}

#[tokio::test]
async fn upload_paths_refuse_symlinks_siblings_and_stray_entries() {
    let parent = tempfile::tempdir().expect("parent");
    let root = parent.path().join("uploads");
    std::fs::create_dir(&root).expect("root");
    let sibling = parent.path().join("uploads-secret");
    std::fs::create_dir(&sibling).expect("sibling");
    std::fs::write(sibling.join("secret"), b"sibling-secret").expect("secret");
    let gateway = opened(&root, 1024, Duration::from_secs(60));
    let (status, body) = post_upload(
        &gateway,
        "notes.md",
        "text/plain",
        None,
        b"inside-root",
        None,
    )
    .await;
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&body));
    let upload = created(&body);
    let bin = find_one(&root, ".md");
    std::fs::remove_file(&bin).expect("remove");
    std::os::unix::fs::symlink(sibling.join("secret"), &bin).expect("symlink");
    let (status, body, _) = call(
        &gateway,
        "GET",
        &format!("/api/uploads/{}", upload.id),
        Vec::new(),
        None,
    )
    .await;
    assert_eq!(status, 403);
    assert_eq!(error_text(&body), "upload path escapes the upload root");
    assert!(
        !body
            .windows(b"sibling-secret".len())
            .any(|window| window == b"sibling-secret")
    );

    let moved = sibling.join("moved");
    let user_dir = bin.parent().expect("user dir").to_path_buf();
    std::fs::remove_file(&bin).expect("unlink");
    std::fs::write(&bin, b"inside-root").expect("restore");
    std::fs::rename(&user_dir, &moved).expect("move");
    std::os::unix::fs::symlink(&moved, &user_dir).expect("dir symlink");
    let (status, body, _) = call(
        &gateway,
        "GET",
        &format!("/api/uploads/{}", upload.id),
        Vec::new(),
        None,
    )
    .await;
    assert_eq!(status, 403);
    assert!(
        !body
            .windows(b"inside-root".len())
            .any(|window| window == b"inside-root")
    );

    let stray = tempfile::tempdir().expect("stray");
    std::fs::write(stray.path().join("notes.txt"), b"nope").expect("stray file");
    let gateway = opened(stray.path(), 1024, Duration::from_secs(60));
    let (status, body, _) = call(&gateway, "GET", "/api/uploads", Vec::new(), None).await;
    assert_eq!(status, 403);
    assert_eq!(error_text(&body), "upload path escapes the upload root");

    let linked = tempfile::tempdir().expect("linked");
    let real = linked.path().join("real");
    std::fs::create_dir(&real).expect("real");
    let via = linked.path().join("via");
    std::os::unix::fs::symlink(&real, &via).expect("root symlink");
    let gateway = opened(&via, 1024, Duration::from_secs(60));
    let (status, body) = post_upload(&gateway, "notes.md", "text/plain", None, b"x", None).await;
    assert_eq!(status, 403);
    assert_eq!(error_text(&body), "upload path escapes the upload root");
}

#[tokio::test]
async fn upload_bodies_above_the_default_http_limit_are_accepted_until_the_configured_max() {
    let root = tempfile::tempdir().expect("root");
    let max = 3 * 1024 * 1024;
    let gateway = opened(root.path(), max, Duration::from_secs(60));
    let body = vec![b'x'; 2_500_000];
    let (status, response) =
        post_upload(&gateway, "shot.png", "image/png", None, &body, None).await;
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&response));
    let upload = created(&response);
    assert_eq!(upload.bytes, 2_500_000);
    let (status, bytes, _) = call(
        &gateway,
        "GET",
        &format!("/api/uploads/{}", upload.id),
        Vec::new(),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(bytes.len(), 2_500_000);
    assert!(bytes.iter().all(|byte| *byte == b'x'));
}

#[tokio::test]
async fn expired_uploads_disappear_without_sleeping_the_thread() {
    let root = tempfile::tempdir().expect("root");
    let gateway = opened(root.path(), 1024, Duration::from_millis(40));
    let (status, body) = post_upload(&gateway, "notes.md", "text/plain", None, b"temp", None).await;
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&body));
    let upload = created(&body);
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let (status, _, _) = call(
                &gateway,
                "GET",
                &format!("/api/uploads/{}", upload.id),
                Vec::new(),
                None,
            )
            .await;
            if status == 404 {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("upload expired");
}

#[tokio::test]
async fn opening_the_store_purges_an_expired_record() {
    let root = tempfile::tempdir().expect("root");
    let gateway = opened(root.path(), 1024, Duration::from_secs(3_600));
    let (status, body) = post_upload(&gateway, "notes.md", "text/plain", None, b"old", None).await;
    assert_eq!(status, 201, "{}", String::from_utf8_lossy(&body));
    let upload = created(&body);
    let meta_path = find_one(root.path(), ".meta.json");
    let mut meta: DiskMeta =
        serde_json::from_slice(&std::fs::read(&meta_path).expect("meta")).expect("meta");
    meta.expires_at = chrono::Utc::now() - chrono::Duration::hours(1);
    std::fs::write(&meta_path, serde_json::to_vec(&meta).expect("encode")).expect("rewrite");
    drop(gateway);
    let gateway = opened(root.path(), 1024, Duration::from_secs(3_600));
    let (status, body, _) = call(
        &gateway,
        "GET",
        &format!("/api/uploads/{}", upload.id),
        Vec::new(),
        None,
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(error_text(&body), "unknown upload");
    assert!(!meta_path.exists());
}

#[derive(Debug, Serialize, Deserialize)]
struct DiskMeta {
    owner: String,
    name: String,
    #[serde(rename = "mediaType")]
    media_type: String,
    bytes: u64,
    #[serde(rename = "expiresAt")]
    expires_at: chrono::DateTime<chrono::Utc>,
    #[serde(default)]
    session: Option<String>,
}

#[tokio::test]
async fn zero_limits_and_a_missing_store_fail_closed() {
    let root = tempfile::tempdir().expect("root");
    let gateway = opened(root.path(), 1024, Duration::ZERO);
    let (status, body) = post_upload(&gateway, "notes.md", "text/plain", None, b"x", None).await;
    assert_eq!(status, 400);
    assert_eq!(error_text(&body), "upload limits must be positive");
    let gateway = bare();
    let (status, body) = post_upload(&gateway, "notes.md", "text/plain", None, b"x", None).await;
    assert_eq!(status, 503);
    assert_eq!(error_text(&body), "uploads are not configured");
    let (status, body, _) = call(&gateway, "GET", "/api/harnesses", Vec::new(), None).await;
    assert_eq!(status, 503);
    assert_eq!(error_text(&body), "terminals disabled");
}

#[tokio::test]
async fn harness_list_uses_the_adapter_and_marks_image_support_from_the_store() {
    let with_store = harness_gateway(true).await;
    let (status, body, _) = call(
        &with_store.gateway,
        "GET",
        "/api/harnesses",
        Vec::new(),
        None,
    )
    .await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let listed: HarnessOfferingList = serde_json::from_slice(&body).expect("offerings");
    assert_eq!(listed.harnesses.len(), 1);
    let offering = listed.harnesses.first().expect("offering");
    assert_eq!(offering.harness_id, HarnessId::ClaudeCode);
    assert_eq!(offering.capabilities.image_attachments, Some(true));

    let without_store = harness_gateway(false).await;
    let (status, body, _) = call(
        &without_store.gateway,
        "GET",
        "/api/harnesses",
        Vec::new(),
        None,
    )
    .await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let listed: HarnessOfferingList = serde_json::from_slice(&body).expect("offerings");
    let offering = listed.harnesses.first().expect("offering");
    assert_eq!(offering.capabilities.image_attachments, Some(false));

    let fixture = Fixture::new().await;
    let (status, body, _) = call(&fixture.gateway, "GET", "/api/harnesses", Vec::new(), None).await;
    assert_eq!(status, 503);
    assert_eq!(error_text(&body), "chat harness unavailable");
}

struct HarnessSetup {
    gateway: Arc<Gateway>,
    _fixture: Fixture,
    _root: Option<tempfile::TempDir>,
}

async fn harness_gateway(uploads: bool) -> HarnessSetup {
    let harness = Arc::new(testkit::FakeHarness::new(Vec::new()));
    harness.set_offerings(vec![sample_offering()]);
    let fixture = Fixture::with_harness(Some(harness)).await;
    let root = uploads.then(|| tempfile::tempdir().expect("uploads"));
    let mut gateway = Gateway::new(Arc::new(testkit::FakeMemoryStore::default()), settings())
        .with_tenancy(tenancy())
        .with_sessions(Arc::clone(&fixture.registry))
        .with_terminal(Arc::clone(&fixture.service));
    if let Some(root) = &root {
        gateway = gateway.with_uploads(UploadService::new(UploadOptions {
            root: root.path().to_path_buf(),
            max_bytes: 1024,
            ttl: Duration::from_secs(60),
        }));
    }
    HarnessSetup {
        gateway: Arc::new(gateway),
        _fixture: fixture,
        _root: root,
    }
}

fn sample_offering() -> HarnessOffering {
    HarnessOffering {
        harness_id: HarnessId::ClaudeCode,
        chat: true,
        capabilities: HarnessCapabilities {
            control: HarnessControlCapabilities {
                interrupt: true,
                resume: true,
                approvals: true,
                steering: None,
            },
            observation: HarnessObservationCapabilities {
                live_stream: true,
                list_sessions: false,
                drive: Some(true),
            },
            turn_options: Some(true),
            launch_model: Some(true),
            image_attachments: Some(false),
            models: Some(vec![HarnessModelOption {
                input_modalities: None,
                id: "sonnet".to_owned(),
                label: "sonnet".to_owned(),
                is_default: None,
                efforts: None,
            }]),
            models_source: Some(HarnessModelsSource::Static),
            efforts: Some(vec![EffortOption {
                id: "low".to_owned(),
                label: "low".to_owned(),
                is_default: None,
            }]),
            model_flag: Some("--model".to_owned()),
            effort_flag: Some("--effort".to_owned()),
        },
    }
}

fn find_one(root: &Path, suffix: &str) -> PathBuf {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(root).expect("root") {
        let entry = entry.expect("entry");
        if !entry.path().is_dir() {
            continue;
        }
        for file in std::fs::read_dir(entry.path()).expect("user") {
            let file = file.expect("file");
            if file.file_name().to_string_lossy().ends_with(suffix) {
                found.push(file.path());
            }
        }
    }
    assert_eq!(found.len(), 1, "one {suffix}");
    found.pop().expect("path")
}

const CLAIMS: &[&str] = &[];

const SKIPS: &[(&str, &str)] = &[
    (
        "vendor-model-cache",
        "inventory 1.4 names vendor cache paths but not their schemas, so sheets use the static rows and tasks.harnesses lists",
    ),
    (
        "harness-ws-upgrade",
        "inventory 17 describes the websocket upgrade and the session socket already owns it",
    ),
    (
        "codex-app-server-sheet",
        "the codex app-server driver is not built in this slice",
    ),
    (
        "pty-driver-events",
        "inventory 1.5 pty and driver events are already mapped by harness-acp, not this http family",
    ),
    (
        "upload-constant-values",
        "inventory 17 names the ceiling variables but not DEFAULT_UPLOAD values, so the Rust defaults are explicit and overridable",
    ),
];

#[test]
fn recorded_harness_requests_are_claimed_or_explicitly_skipped() {
    let mut names = std::collections::BTreeSet::new();
    for (id, reason) in SKIPS {
        assert_ne!(reason.trim(), "", "{id}");
        assert!(names.insert(*id), "duplicate skip {id}");
        assert!(!CLAIMS.contains(id), "{id} is both claimed and skipped");
    }
    for relative in [
        "../testkit/tests/fixtures/gateway/http-plain.json",
        "../wire/tests/golden/gateway/http-tls.json",
    ] {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
        let cases = testkit::http::cases(&path).expect("cases");
        for case in cases {
            let head = case.request.head.as_str();
            if !head.contains("/api/harnesses") && !head.contains("/api/uploads") {
                continue;
            }
            let claimed = CLAIMS.contains(&case.id.as_str());
            let skipped = SKIPS.iter().any(|(id, _)| *id == case.id.as_str());
            assert!(
                claimed || skipped,
                "unaccounted harness recording {}",
                case.id
            );
        }
    }
}
