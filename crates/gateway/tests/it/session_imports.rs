#![cfg(test)]
use crate::session_delete::Stores;
use crate::session_support::{Fixture, http, user};

use std::sync::Arc;

use chrono::{DateTime, Utc};
use gateway::sessions::{SessionRegistry, owners_file};
use kernel::memory::share::receive::Verification;
use kernel::memory::{ImportOrigin, ImportReceipt, MemoryState, SessionActivity, SessionRecord};
use testkit::{FakeSessionCatalog, FakeSessionDeletion};
use wire::capture::ReceiptVerification;
use wire::gateway::session_catalog::{
    SessionDetail, SessionOriginLabel, SessionSummaryPage, SharedWith,
};
use wire::gateway::session_delete::{
    SessionDeleteError, SessionDeleteResult, SessionDeleteResults, SessionDeleted,
};
use wire::harness::HarnessId;
use wire::session_id::{SessionId, SessionKey};

const ALEX: Option<&str> = Some("alice-test-token");
const DIGEST: &str = "9f2c0000000000000000000000000000000000000000000000000000000000e1";
const UNKNOWN_DIGEST: &str = "ab00000000000000000000000000000000000000000000000000000000000012";

struct Setup {
    fixture: Fixture,
    deletion: Arc<FakeSessionDeletion>,
    _state: tempfile::TempDir,
}

fn import_key(digest: &str) -> SessionKey {
    format!("import:{digest}").parse().expect("import key")
}

fn encoded(key: &SessionKey) -> String {
    key.as_str().replace(':', "%3A")
}

fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .expect("time")
        .with_timezone(&Utc)
}

fn imported() -> SessionRecord<SessionKey> {
    SessionRecord {
        owner: None,
        id: import_key(DIGEST),
        title: "Fix the flaky test".to_owned(),
        started_at: at("2026-11-02T09:14:00Z"),
        ended_at: Some(at("2026-11-02T10:01:00Z")),
        message_count: 42,
        topic: None,
        kept: false,
        trashed: false,
        memory: MemoryState::Summarized,
        gist: None,
        cwd: None,
        origin: Some(ImportOrigin {
            author: Some("alex".to_owned()),
            created_at: Some(at("2026-11-02T09:14:00Z")),
            updated_at: Some(at("2026-11-02T10:01:00Z")),
            receipt: Some(ImportReceipt {
                bundle_id: "bundle-1".to_owned(),
                verification: Verification::Verified,
            }),
        }),
    }
}

fn native() -> SessionRecord {
    SessionRecord {
        owner: None,
        id: SessionId::new(HarnessId::ClaudeCode, "solo"),
        title: "Solo".to_owned(),
        started_at: at("2026-11-03T09:00:00Z"),
        ended_at: None,
        message_count: 2,
        topic: None,
        kept: false,
        trashed: false,
        memory: MemoryState::Summarized,
        gist: None,
        cwd: None,
        origin: None,
    }
}

fn setup() -> Setup {
    let state = tempfile::tempdir().expect("state");
    let path = owners_file(state.path());
    std::fs::create_dir_all(path.parent().expect("owners directory")).expect("owners directory");
    std::fs::write(&path, "{}").expect("owners");
    let registry = SessionRegistry::default()
        .with_owners_file(path)
        .with_node_owner(user("owner"));
    let deletion = Arc::new(FakeSessionDeletion::default().with_activity(
        import_key(DIGEST),
        SessionActivity {
            conversations: 1,
            messages: 42,
            ..SessionActivity::default()
        },
    ));
    let catalog = FakeSessionCatalog::new(deletion.clone())
        .with_entry(imported())
        .with_record(native());
    let fixture = Fixture::with_catalog(
        registry,
        Some(Arc::new(Stores::default())),
        deletion.clone(),
        Arc::new(catalog),
    );
    Setup {
        fixture,
        deletion,
        _state: state,
    }
}

async fn call(setup: &Setup, method: &str, path: &str, body: &str) -> (u16, Vec<u8>) {
    http(
        &setup.fixture.gateway,
        method,
        path,
        body.as_bytes().to_vec(),
        ALEX,
    )
    .await
}

fn page(body: &[u8]) -> SessionSummaryPage {
    serde_json::from_slice(body)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(body)))
}

fn ids(setup_body: &[u8]) -> Vec<String> {
    page(setup_body)
        .items
        .into_iter()
        .map(|row| row.id.to_string())
        .collect()
}

async fn view(setup: &Setup, name: &str) -> Vec<String> {
    let (status, body) = call(setup, "GET", &format!("/api/sessions?view={name}"), "").await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    ids(&body)
}

#[tokio::test]
async fn a_team_user_lists_an_import_labelled_with_its_author_date_and_verification() {
    let setup = setup();
    let (status, body) = call(&setup, "GET", "/api/sessions", "").await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let listed = page(&body);
    assert_eq!(
        listed.items.len(),
        1,
        "the registry hides the harness session"
    );
    let row = listed.items.first().expect("the import row");
    assert_eq!(row.id, import_key(DIGEST));
    assert_eq!(row.harness, None);
    assert_eq!(
        row.origin,
        Some(SessionOriginLabel {
            author: Some("alex".to_owned()),
            created_at: Some(at("2026-11-02T09:14:00Z")),
            updated_at: Some(at("2026-11-02T10:01:00Z")),
            verification: Some(ReceiptVerification::Verified),
            bundle_id: Some("bundle-1".to_owned()),
        })
    );
    assert_eq!(row.visibility, SharedWith::Private);
    assert!(!row.trashed);
    assert_eq!(row.message_count, 42);
    assert!(
        !String::from_utf8_lossy(&body).contains("\"harness\""),
        "an import row has no harness key"
    );
    let (_, body) = call(&setup, "GET", "/api/sessions?harness=claude-code", "").await;
    assert_eq!(
        ids(&body),
        Vec::<String>::new(),
        "a harness filter leaves imports out"
    );
    let cursor = format!("1790000000000.import:{UNKNOWN_DIGEST}");
    let (status, _) = call(&setup, "GET", &format!("/api/sessions?cursor={cursor}"), "").await;
    assert_eq!(status, 200, "an import key is a cursor key");
}

#[tokio::test]
async fn an_import_moves_between_the_views_by_its_encoded_key() {
    let setup = setup();
    let key = import_key(DIGEST);
    let path = format!("/api/sessions/{}", encoded(&key));
    let listed = vec![key.to_string()];

    let (status, _) = call(&setup, "PUT", &format!("{path}/keep"), "").await;
    assert_eq!(status, 204);
    assert_eq!(view(&setup, "kept").await, listed);
    let (status, _) = call(&setup, "DELETE", &format!("{path}/keep"), "").await;
    assert_eq!(status, 204);
    assert_eq!(view(&setup, "kept").await, Vec::<String>::new());

    let (status, _) = call(&setup, "POST", &format!("{path}/trash"), "").await;
    assert_eq!(status, 204);
    assert!(setup.deletion.is_trashed(key.clone()));
    assert_eq!(view(&setup, "all").await, Vec::<String>::new());
    assert_eq!(view(&setup, "trash").await, listed);

    let (status, _) = call(&setup, "POST", &format!("{path}/restore"), "").await;
    assert_eq!(status, 204);
    assert_eq!(view(&setup, "all").await, listed);
    assert_eq!(view(&setup, "trash").await, Vec::<String>::new());

    let (status, body) = call(&setup, "DELETE", &path, "").await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let deleted: SessionDeleted = serde_json::from_slice(&body).expect("deleted");
    assert_eq!(deleted.session, key);
    assert!(setup.deletion.deleted_by(key).is_some());
}

#[tokio::test]
async fn the_bulk_endpoint_deletes_an_import_and_refuses_what_is_not_the_users() {
    let setup = setup();
    let body = format!(
        r#"{{"sessions":["import:{DIGEST}","import:{UNKNOWN_DIGEST}","claude-code:solo"]}}"#
    );
    let (status, reply) = call(&setup, "POST", "/api/sessions/delete", &body).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&reply));
    let results: SessionDeleteResults = serde_json::from_slice(&reply).expect("results");
    assert!(matches!(
        results.results.first(),
        Some(SessionDeleteResult::Deleted(deleted)) if deleted.session == import_key(DIGEST)
    ));
    let (status, _) = call(
        &setup,
        "POST",
        "/api/sessions/delete",
        r#"{"sessions":["import:xyz"]}"#,
    )
    .await;
    assert_eq!(status, 400);
    for refused in results.results.iter().skip(1) {
        assert!(matches!(
            refused,
            SessionDeleteResult::Refused(refused) if refused.error == SessionDeleteError::NotFound
        ));
    }
}

#[tokio::test]
async fn an_unknown_import_is_not_found_and_a_malformed_one_is_a_bad_request() {
    let setup = setup();
    let unknown = encoded(&import_key(UNKNOWN_DIGEST));
    for (method, suffix) in [
        ("POST", "/trash"),
        ("POST", "/restore"),
        ("PUT", "/keep"),
        ("DELETE", "/keep"),
        ("DELETE", ""),
        ("GET", ""),
    ] {
        let (status, _) = call(
            &setup,
            method,
            &format!("/api/sessions/{unknown}{suffix}"),
            "",
        )
        .await;
        assert_eq!(status, 404, "{method} {suffix}");
    }
    assert!(!setup.deletion.is_trashed(import_key(UNKNOWN_DIGEST)));
    for (method, suffix) in [
        ("POST", "/trash"),
        ("POST", "/restore"),
        ("PUT", "/keep"),
        ("DELETE", "/keep"),
        ("DELETE", ""),
        ("GET", ""),
    ] {
        let (status, body) = call(
            &setup,
            method,
            &format!("/api/sessions/import%3Adeadbeef{suffix}"),
            "",
        )
        .await;
        assert_eq!(status, 400, "{method} {suffix}");
        assert!(
            String::from_utf8_lossy(&body).contains("import:deadbeef"),
            "{}",
            String::from_utf8_lossy(&body)
        );
    }
}

#[tokio::test]
async fn an_import_detail_has_its_label_and_no_resume_command() {
    let setup = setup();
    let path = format!("/api/sessions/{}", encoded(&import_key(DIGEST)));
    let (status, body) = call(&setup, "GET", &path, "").await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let detail: SessionDetail = serde_json::from_slice(&body).expect("detail");
    assert_eq!(detail.resume_command, None);
    assert_eq!(detail.row.harness, None);
    assert_eq!(
        detail
            .row
            .origin
            .and_then(|origin| origin.author)
            .as_deref(),
        Some("alex")
    );
}
