mod bulk;
mod decisions;
mod done;
mod entities;
mod filing;
mod models;
mod pages;
mod review;
mod tidy;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use axum::http::StatusCode;
use axum::http::header;
use gateway::Gateway;
use gateway::Settings;
use gateway::sessions::SessionRegistry;
use gateway::tenancy::Tenancy;
use gateway::tenancy::UserId;
use kernel::memory::MemoryStore;
use memory_sqlite::SqliteStore;
use serde::de::DeserializeOwned;
use tower::ServiceExt;
use wire::gateway::GatewayError;
use wire::gateway::topics::PlacedBy;
use wire::gateway::topics::PlacementReply;
use wire::gateway::topics::TopicDirectory;
use wire::gateway::topics::TopicList;
use wire::gateway::topics::TopicPage;
use wire::gateway::topics::TopicRecord;
use wire::gateway::topics::TopicSessionList;
use wire::session_id::SessionId;
use wire::session_id::encode_segment;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const BODY_LIMIT: usize = 1 << 20;

fn settings(root: &Path) -> Settings {
    Settings {
        hostname: "node-a-host".to_owned(),
        node: "node-a".to_owned(),
        allowed_origins: Vec::new(),
        allowed_hosts: Vec::new(),
        files_root: None,
        static_dir: None,
        root_redirect: None,
        state_dir: root.join("state"),
        directory_root: root.join("agents"),
        shared_dir: root.join("shared"),
        allowed_harnesses: None,
        probe_timeout: Duration::from_millis(200),
        private_ca: None,
    }
}

struct Api {
    router: axum::Router,
    sessions: Arc<SessionRegistry>,
    root: tempfile::TempDir,
    store: Arc<SqliteStore>,
}

impl Api {
    fn new(topics: bool) -> TestResult<Self> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("memory.sqlite");
        migrate::sqlite(&path)?;
        let store = Arc::new(SqliteStore::open(&path)?);
        let memory: Arc<dyn MemoryStore> = store.clone();
        let mut gateway = Gateway::new(memory, settings(root.path()));
        if topics {
            gateway = gateway
                .with_tenancy(Tenancy::local(UserId::new("owner".to_owned())?))
                .with_topics(store.clone(), store.clone())
                .with_decisions(store.clone())
                .with_facts(store.clone())
                .with_review(store.clone())
                .with_changes(store.clone(), store.clone())
                .with_session_catalog(store.clone())
                .with_tenancy_store(store.clone())
                .with_topic_pages(store.clone())
                .with_tidy(store.clone(), store.clone());
        }
        let gateway = Arc::new(gateway);
        Ok(Self {
            sessions: Arc::clone(gateway.sessions()),
            router: gateway.router(false),
            root,
            store,
        })
    }

    fn database(&self) -> rusqlite::Result<rusqlite::Connection> {
        rusqlite::Connection::open(self.root.path().join("memory.sqlite"))
    }

    async fn call(
        &self,
        method: &str,
        target: &str,
        body: &str,
    ) -> TestResult<(StatusCode, Vec<u8>)> {
        let mut request = Request::builder()
            .method(method)
            .uri(target)
            .header(header::HOST, "127.0.0.1:5976")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_owned()))?;
        request
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:40000".parse::<SocketAddr>()?));
        let response = self.router.clone().oneshot(request).await?;
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), BODY_LIMIT).await?;
        Ok((status, bytes.to_vec()))
    }

    async fn json<T: DeserializeOwned>(
        &self,
        method: &str,
        target: &str,
        body: &str,
        expected: StatusCode,
    ) -> TestResult<T> {
        let (status, bytes) = self.call(method, target, body).await?;
        if status != expected {
            return Err(format!(
                "{method} {target}: {status} {}",
                String::from_utf8_lossy(&bytes)
            )
            .into());
        }
        Ok(serde_json::from_slice(&bytes)?)
    }

    async fn error(
        &self,
        method: &str,
        target: &str,
        body: &str,
    ) -> TestResult<(StatusCode, String)> {
        let (status, bytes) = self.call(method, target, body).await?;
        Ok((
            status,
            serde_json::from_slice::<GatewayError>(&bytes)?.error,
        ))
    }
}

fn placement_path(session: &SessionId) -> String {
    format!("/api/sessions/{}/placement", encode_segment(session))
}

#[tokio::test]
async fn topics_are_created_listed_searched_and_edited() -> TestResult {
    let api = Api::new(true)?;
    let created: TopicRecord = api
        .json(
            "POST",
            "/api/topics",
            r#"{"name":"RivetHub Desktop","aliases":["Tauri"],"directory":{"kind":"local","node":"node-a","path":"/home/user/acmekit"}}"#,
            StatusCode::CREATED,
        )
        .await?;
    assert_eq!(created.slug, "rivethub-desktop");
    assert_eq!(
        created.directory,
        Some(TopicDirectory::Local {
            node: "node-a".into(),
            path: "/home/user/acmekit".into(),
        })
    );
    let child: TopicRecord = api
        .json(
            "POST",
            "/api/topics",
            r#"{"name":"Desktop Menus","parent":"rivethub-desktop"}"#,
            StatusCode::CREATED,
        )
        .await?;
    assert_eq!(child.parent.as_deref(), Some(created.id.as_str()));
    let listed: TopicList = api.json("GET", "/api/topics", "", StatusCode::OK).await?;
    assert_eq!(listed.topics.len(), 2);
    assert!(listed.topics.iter().all(|topic| topic.session_count == 0));
    let hits: TopicList = api
        .json("GET", "/api/topics/search?q=tau", "", StatusCode::OK)
        .await?;
    assert_eq!(
        hits.topics
            .iter()
            .map(|hit| hit.topic.id.as_str())
            .collect::<Vec<_>>(),
        [created.id.as_str()]
    );
    let shown: TopicRecord = api
        .json(
            "GET",
            &format!("/api/topics/{}", created.id),
            "",
            StatusCode::OK,
        )
        .await?;
    assert_eq!(shown, created);
    let renamed: TopicRecord = api
        .json(
            "PATCH",
            "/api/topics/desktop-menus",
            r#"{"name":"Menus","parent":null,"aliases":["menu bar"]}"#,
            StatusCode::OK,
        )
        .await?;
    assert_eq!(renamed.slug, "menus");
    assert_eq!(renamed.parent, None);
    assert_eq!(renamed.aliases, ["menu bar"]);
    Ok(())
}

#[tokio::test]
async fn a_topic_answers_with_its_root_page() -> TestResult {
    let api = Api::new(true)?;
    let root: TopicRecord = api
        .json(
            "POST",
            "/api/topics",
            r#"{"name":"Mesh"}"#,
            StatusCode::CREATED,
        )
        .await?;
    let _child: TopicRecord = api
        .json(
            "POST",
            "/api/topics",
            r#"{"name":"Certs","parent":"mesh"}"#,
            StatusCode::CREATED,
        )
        .await?;
    let database = api.database()?;
    database.execute(
        "INSERT INTO ros_wiki_topics (slug, title, created_at, updated_at)
         VALUES ('mesh-overview', 'Mesh overview', '2026-10-27T00:00:00Z', '2026-10-27T00:00:00Z')",
        [],
    )?;
    database.execute(
        "INSERT INTO rh_topic_pages (topic_id, page_slug) VALUES (?1, 'mesh-overview')",
        [root.id.as_str()],
    )?;
    let own: TopicPage = api
        .json("GET", "/api/topics/mesh/page", "", StatusCode::OK)
        .await?;
    assert_eq!(own.topic, root);
    let page = own.page.ok_or("the root topic must answer with its page")?;
    assert_eq!(page.slug, "mesh-overview");
    assert_eq!(page.topic_id.as_deref(), Some(root.id.as_str()));
    let inherited: TopicPage = api
        .json("GET", "/api/topics/certs/page", "", StatusCode::OK)
        .await?;
    assert_eq!(
        inherited.page.map(|page| page.slug).as_deref(),
        Some("mesh-overview")
    );
    assert_eq!(
        api.error("GET", "/api/topics/nowhere/page", "").await?.0,
        StatusCode::NOT_FOUND
    );
    Ok(())
}

#[tokio::test]
async fn taken_names_unknown_topics_and_empty_names_are_refused() -> TestResult {
    let api = Api::new(true)?;
    let _created: TopicRecord = api
        .json(
            "POST",
            "/api/topics",
            r#"{"name":"RivetHub Desktop","aliases":["Tauri"]}"#,
            StatusCode::CREATED,
        )
        .await?;
    assert_eq!(
        api.error("POST", "/api/topics", r#"{"name":"rivethub desktop"}"#)
            .await?,
        (
            StatusCode::CONFLICT,
            "There's already a topic called \"rivethub desktop\".".to_owned()
        )
    );
    assert_eq!(
        api.error(
            "POST",
            "/api/topics",
            r#"{"name":"Other","aliases":["tauri"]}"#
        )
        .await?,
        (
            StatusCode::CONFLICT,
            "\"Tauri\" is already another topic's name.".to_owned()
        )
    );
    assert_eq!(
        api.error("GET", "/api/topics/no-such-topic", "").await?.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        api.error("POST", "/api/topics", r#"{"name":"  "}"#)
            .await?
            .0,
        StatusCode::BAD_REQUEST
    );
    Ok(())
}

#[tokio::test]
async fn sessions_are_placed_moved_unplaced_and_follow_merges() -> TestResult {
    let api = Api::new(true)?;
    let first: TopicRecord = api
        .json(
            "POST",
            "/api/topics",
            r#"{"name":"Desktop App"}"#,
            StatusCode::CREATED,
        )
        .await?;
    let second: TopicRecord = api
        .json(
            "POST",
            "/api/topics",
            r#"{"name":"Clients"}"#,
            StatusCode::CREATED,
        )
        .await?;
    let session: SessionId = "claude-code:00000000-0000-4000-8000-000000000021".parse()?;
    let path = placement_path(&session);
    let forming: PlacementReply = api.json("GET", &path, "", StatusCode::OK).await?;
    assert_eq!(forming.placement, None);
    let placed: PlacementReply = api
        .json(
            "PUT",
            &path,
            &format!(r#"{{"topicId":"{}"}}"#, first.id),
            StatusCode::OK,
        )
        .await?;
    let placement = placed.placement.ok_or("no placement")?;
    assert_eq!(placement.topic_id, first.id);
    assert_eq!(placement.by, PlacedBy::You);
    assert_eq!(placement.session_id, session);
    let members: TopicSessionList = api
        .json(
            "GET",
            "/api/topics/desktop-app/sessions",
            "",
            StatusCode::OK,
        )
        .await?;
    assert_eq!(members.sessions.len(), 1);
    let merged: TopicRecord = api
        .json(
            "POST",
            "/api/topics/desktop-app/merge",
            r#"{"into":"clients"}"#,
            StatusCode::OK,
        )
        .await?;
    assert_eq!(merged.id, second.id);
    let moved: PlacementReply = api.json("GET", &path, "", StatusCode::OK).await?;
    assert_eq!(
        moved.placement.map(|placement| placement.topic_id),
        Some(second.id.clone())
    );
    let redirected: TopicRecord = api
        .json("GET", "/api/topics/desktop-app", "", StatusCode::OK)
        .await?;
    assert_eq!(redirected.id, second.id);
    let listed: TopicList = api.json("GET", "/api/topics", "", StatusCode::OK).await?;
    assert_eq!(
        listed
            .topics
            .iter()
            .map(|topic| (topic.topic.slug.as_str(), topic.session_count))
            .collect::<Vec<_>>(),
        [("clients", 1)]
    );
    let removed: PlacementReply = api.json("DELETE", &path, "", StatusCode::OK).await?;
    assert_eq!(removed.placement, None);
    let forming: PlacementReply = api.json("GET", &path, "", StatusCode::OK).await?;
    assert_eq!(forming.placement, None);
    Ok(())
}

#[tokio::test]
async fn placements_need_a_canonical_session_id() -> TestResult {
    let api = Api::new(true)?;
    assert_eq!(
        api.error(
            "PUT",
            "/api/sessions/00000000-0000-4000-8000-000000000021/placement",
            r#"{"topicId":"clients"}"#
        )
        .await?,
        (
            StatusCode::BAD_REQUEST,
            "canonical session required".to_owned()
        )
    );
    Ok(())
}

#[tokio::test]
async fn a_gateway_without_a_topic_store_answers_unavailable() -> TestResult {
    let api = Api::new(false)?;
    assert_eq!(
        api.error("GET", "/api/topics", "").await?,
        (
            StatusCode::SERVICE_UNAVAILABLE,
            "topic store unavailable".to_owned()
        )
    );
    Ok(())
}
