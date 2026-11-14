#![cfg(test)]
use crate::session_delete::Stores;
use crate::session_support;

use std::sync::Arc;

use chrono::{DateTime, Utc};
use gateway::sessions::{SessionRegistry, owners_file};
use kernel::memory::{MemoryState, SessionRecord, SessionTopic};
use kernel::topics::TopicId;
use session_support::{Fixture, http, user};
use testkit::{FakeSessionCatalog, FakeSessionDeletion};
use wire::gateway::session_catalog::{SessionDetail, SessionSummaryPage, SharedWith};
use wire::harness::HarnessId;
use wire::session_id::{SessionId, encode_segment};

struct Setup {
    fixture: Fixture,
    deletion: Arc<FakeSessionDeletion>,
    _state: tempfile::TempDir,
}

fn claude(native: &str) -> SessionId {
    SessionId::new(HarnessId::ClaudeCode, native)
}

fn record(native: &str, title: &str, minute: i64) -> SessionRecord {
    let started: DateTime<Utc> =
        DateTime::from_timestamp(1_790_000_000 + minute * 60, 0).unwrap_or(DateTime::UNIX_EPOCH);
    SessionRecord {
        owner: None,
        id: claude(native),
        title: title.to_owned(),
        started_at: started,
        ended_at: Some(started),
        message_count: 4,
        topic: None,
        kept: false,
        trashed: false,
        memory: MemoryState::Summarized,
        gist: Some("gist".to_owned()),
        cwd: Some("/home/user/app".to_owned()),
        origin: None,
    }
}

fn setup(stores: Stores, records: Vec<SessionRecord>) -> Setup {
    let state = tempfile::tempdir().expect("state");
    let path = owners_file(state.path());
    std::fs::create_dir_all(path.parent().expect("owners directory")).expect("owners directory");
    std::fs::write(&path, "{}").expect("owners");
    let registry = SessionRegistry::default()
        .with_owners_file(path)
        .with_node_owner(user("owner"));
    let deletion = Arc::new(FakeSessionDeletion::default());
    let catalog = records.into_iter().fold(
        FakeSessionCatalog::new(deletion.clone()),
        FakeSessionCatalog::with_record,
    );
    let fixture = Fixture::with_catalog(
        registry,
        Some(Arc::new(stores)),
        deletion.clone(),
        Arc::new(catalog),
    );
    Setup {
        fixture,
        deletion,
        _state: state,
    }
}

async fn call(setup: &Setup, method: &str, path: &str) -> (u16, Vec<u8>) {
    http(&setup.fixture.gateway, method, path, Vec::new(), None).await
}

fn page(body: &[u8]) -> SessionSummaryPage {
    serde_json::from_slice(body)
        .unwrap_or_else(|error| panic!("{error}: {}", String::from_utf8_lossy(body)))
}

fn titles(body: &[u8]) -> Vec<String> {
    page(body).items.into_iter().map(|row| row.title).collect()
}

fn store_row(native: &str) -> kernel::transcript::StoreSession {
    kernel::transcript::StoreSession::new(HarnessId::ClaudeCode, native, 1)
}

fn seeded() -> (Stores, Vec<SessionRecord>) {
    let mut child = store_row("child");
    child.parent_session_id = Some("parent".to_owned());
    let stores = Stores {
        rows: vec![store_row("parent"), child, store_row("solo")],
        ..Stores::default()
    };
    let mut filed = record("solo", "Solo", 3);
    filed.topic = Some(SessionTopic {
        id: TopicId::random(),
        path: vec!["Platform".to_owned(), "Deploys".to_owned()],
    });
    let records = vec![
        record("parent", "Parent", 1),
        record("child", "Child", 2),
        filed,
    ];
    (stores, records)
}

#[tokio::test]
async fn the_list_leaves_child_sessions_out_unless_asked_and_counts_them_on_the_parent() {
    let (stores, records) = seeded();
    let setup = setup(stores, records);
    let (status, body) = call(&setup, "GET", "/api/sessions").await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    assert_eq!(titles(&body), ["Solo", "Parent"]);
    let listed = page(&body);
    let parent = listed
        .items
        .iter()
        .find(|row| row.title == "Parent")
        .expect("parent row");
    assert_eq!(parent.child_count, Some(1));
    let (_, body) = call(&setup, "GET", "/api/sessions?includeChildren=true").await;
    assert_eq!(titles(&body), ["Solo", "Child", "Parent"]);
    let with_children = page(&body);
    let child = with_children
        .items
        .iter()
        .find(|row| row.title == "Child")
        .expect("child row");
    assert_eq!(child.parent_id, Some(claude("parent")));
}

#[tokio::test]
async fn trash_restore_and_unkeep_move_a_session_between_views() {
    let (stores, records) = seeded();
    let setup = setup(stores, records);
    let solo = encode_segment(&claude("solo"));
    let (status, _) = call(&setup, "PUT", &format!("/api/sessions/{solo}/keep")).await;
    assert_eq!(status, 204);
    let (_, body) = call(&setup, "GET", "/api/sessions?view=kept").await;
    assert_eq!(titles(&body), ["Solo"]);

    let (status, _) = call(&setup, "POST", &format!("/api/sessions/{solo}/trash")).await;
    assert_eq!(status, 204);
    assert!(setup.deletion.is_trashed(claude("solo")));
    let (_, body) = call(&setup, "GET", "/api/sessions").await;
    assert_eq!(titles(&body), ["Parent"]);
    let (_, body) = call(&setup, "GET", "/api/sessions?view=trash").await;
    let trashed = page(&body);
    assert_eq!(trashed.items.len(), 1);
    assert!(trashed.items.iter().all(|row| row.trashed));
    let (_, body) = call(&setup, "GET", "/api/harnesses/claude-code/sessions").await;
    assert!(!String::from_utf8_lossy(&body).contains("solo"));

    let (status, _) = call(&setup, "POST", &format!("/api/sessions/{solo}/restore")).await;
    assert_eq!(status, 204);
    let (_, body) = call(&setup, "GET", "/api/sessions").await;
    assert_eq!(titles(&body), ["Solo", "Parent"]);

    let (status, _) = call(&setup, "DELETE", &format!("/api/sessions/{solo}/keep")).await;
    assert_eq!(status, 204);
    let (_, body) = call(&setup, "GET", "/api/sessions?view=kept").await;
    assert_eq!(page(&body).items.len(), 0);
    let missing = encode_segment(&claude("nowhere"));
    for (method, suffix) in [("POST", "trash"), ("POST", "restore"), ("DELETE", "keep")] {
        let (status, _) = call(&setup, method, &format!("/api/sessions/{missing}/{suffix}")).await;
        assert_eq!(status, 404, "{method} {suffix}");
    }
}

#[tokio::test]
async fn the_detail_adds_cwd_and_a_resume_command() {
    let (stores, records) = seeded();
    let setup = setup(stores, records);
    let solo = encode_segment(&claude("solo"));
    let (status, body) = call(&setup, "GET", &format!("/api/sessions/{solo}")).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    let detail: SessionDetail = serde_json::from_slice(&body).expect("detail");
    assert_eq!(detail.cwd.as_deref(), Some("/home/user/app"));
    assert_eq!(
        detail.resume_command.as_deref(),
        Some("claude --resume solo")
    );
    assert_eq!(detail.row.message_count, 4);
    assert_eq!(
        detail.row.memory_state,
        wire::gateway::session_catalog::SessionMemoryState::Summarized
    );
    let path = detail.row.topic.map(|topic| topic.path);
    assert_eq!(
        path,
        Some(vec!["Platform".to_owned(), "Deploys".to_owned()])
    );
    assert_eq!(detail.row.visibility, SharedWith::Private);
    assert_eq!(detail.changed.len(), 0);
    let (status, _) = call(&setup, "GET", "/api/sessions/bm90aGVyZQ").await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn a_bad_filter_is_a_bad_request() {
    let (stores, records) = seeded();
    let setup = setup(stores, records);
    for query in ["harness=nope", "cursor=garbage", "view=bogus"] {
        let (status, _) = call(&setup, "GET", &format!("/api/sessions?{query}")).await;
        assert_eq!(status, 400, "{query}");
    }
}

#[tokio::test]
async fn teammate_owner_is_carried_by_both_catalog_list_and_detail() {
    let (stores, mut records) = seeded();
    for record in &mut records {
        if record.id == claude("solo") {
            record.owner = Some("alex".into());
        }
    }
    let setup = setup(stores, records);
    let (_, body) = call(&setup, "GET", "/api/sessions").await;
    let page = page(&body);
    let solo = page
        .items
        .iter()
        .find(|row| row.title == "Solo")
        .expect("solo");
    assert_eq!(solo.owner.as_deref(), Some("alex"));
    let parent = page
        .items
        .iter()
        .find(|row| row.title == "Parent")
        .expect("parent");
    assert_eq!(parent.owner, None);
    let key = encode_segment(&claude("solo"));
    let (_, body) = call(&setup, "GET", &format!("/api/sessions/{key}")).await;
    let detail: SessionDetail = serde_json::from_slice(&body).expect("detail");
    assert_eq!(detail.row.owner.as_deref(), Some("alex"));
}
