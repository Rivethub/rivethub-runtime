use std::net::SocketAddr;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use chrono::Utc;
use gateway::tenancy::{Tenancy, UserId};
use kernel::memory::{CaptureBatchRequest, CaptureMessage};
use kernel::owner::Owner;
use kernel::tenancy::{Space, SpaceId, SpaceKind, TenancyStore as _, Viewer, Visibility};
use memory_postgres::{CaptureOptions, PostgresMemory, PostgresTenancy};
use rmcp::ServiceExt as _;
use sqlx::postgres::PgPoolOptions;
use sqlx::{ConnectOptions as _, PgPool};
use tokio::process::Command;
use tower::ServiceExt as _;
use wire::gateway::memory::{MemoryBrowseResponse, MemorySearchResponse};
use wire::gateway::session_catalog::SessionSummaryPage;
use wire::message::Role;

use crate::process::guard::SpawnGuarded as _;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const PERSON: &str = "rh_team_recall_person";
const READER: &str = "rh_team_recall_reader";
const SHARED: &str = "claude-code:shared";
const OWN: &str = "claude-code:own";
const PRIVATE: &str = "claude-code:private";

fn team() -> TestResult<Space> {
    Ok(Space {
        id: SpaceId::new("team".into())?,
        kind: SpaceKind::Team,
        name: "Team".into(),
        members: ["alex", "phil", "tom"]
            .into_iter()
            .map(|id| Owner::new(id.into()))
            .collect::<Result<_, _>>()?,
    })
}

async fn seed(pool: &PgPool) -> TestResult {
    let tenancy = PostgresTenancy::new(pool.clone());
    tenancy.sync_spaces(&[team()?]).await?;
    let writer = PostgresMemory::new(pool.clone());
    for (owner, key) in [("phil", SHARED), ("alex", OWN), ("tom", PRIVATE)] {
        writer.capture_batch(&CaptureBatchRequest {
            session_key: key.into(), agent: "rivet".into(), channel: Some("claude-code".into()),
            title: Some(format!("{owner} release work")), settings: None, task_id: None, finalize: None,
            created_at: None, updated_at: None,
            messages: vec![CaptureMessage { event_id: key.into(), role: Role::User,
                content: format!("Release planning with {owner}: a synthetic detailed message about the release checklist."),
                tool_name: None, tool_args: None, tool_result: None, metadata: None, created_at: None }],
        }, &CaptureOptions::owned_by(&Owner::new(owner.into())?)).await?;
    }
    let owner = Viewer::new(Owner::new("phil".into())?, vec![team()?.id]);
    tenancy
        .mark_session(
            &owner,
            &SHARED.parse()?,
            Some(&Visibility::Space(team()?.id)),
            Utc::now(),
        )
        .await?;
    Ok(())
}

async fn roles(pool: &PgPool) -> TestResult {
    sqlx::raw_sql("DO $$ BEGIN CREATE ROLE rh_team_recall_person LOGIN PASSWORD 'rh_team_recall_person'; EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$;
        DO $$ BEGIN CREATE ROLE rh_team_recall_reader NOLOGIN; EXCEPTION WHEN duplicate_object OR unique_violation THEN NULL; END $$;
        GRANT USAGE ON SCHEMA public TO rh_team_recall_person, rh_team_recall_reader;
        GRANT SELECT ON ALL TABLES IN SCHEMA public TO rh_team_recall_person, rh_team_recall_reader;")
        .execute(pool).await?;
    sqlx::query("INSERT INTO rh_role_principals(role_name, user_id) VALUES ($1, 'alex')")
        .bind(PERSON)
        .execute(pool)
        .await?;
    Ok(())
}

#[tokio::test]
async fn pinned_mcp_introduces_its_database_person_without_a_spaces_users_file() -> TestResult {
    let db = crate::pgdb::TestDb::migrated().await?;
    seed(&db).await?;
    roles(&db).await?;
    let options = db
        .connect_options()
        .as_ref()
        .clone()
        .username(PERSON)
        .password(PERSON);
    let instructions = introduction(options, "wrong-fallback").await?;
    assert!(
        instructions.starts_with("You are working with alex on a shared hub with phil, tom."),
        "{instructions}"
    );
    assert!(!instructions.contains("wrong-fallback"));
    Ok(())
}

#[tokio::test]
async fn unpinned_mcp_uses_the_users_owner_and_store_memberships() -> TestResult {
    let db = crate::pgdb::TestDb::migrated().await?;
    seed(&db).await?;
    let instructions = introduction(db.connect_options().as_ref().clone(), "alex").await?;
    assert!(instructions.starts_with("You are working with alex on a shared hub with phil, tom."));
    Ok(())
}

async fn introduction(
    options: sqlx::postgres::PgConnectOptions,
    owner: &str,
) -> TestResult<String> {
    let home = tempfile::tempdir()?;
    let state = home.path().join(".rivethub");
    std::fs::create_dir_all(&state)?;
    std::fs::write(
        state.join("users.json"),
        format!(
            r#"{{"ownerUserId":"{owner}","unmappedIsOwner":false,"users":{{"{owner}":{{"tokens":[]}}}}}}"#
        ),
    )?;
    let url = options.to_url_lossy();
    std::fs::write(
        state.join("config.yaml"),
        format!(
            "runtime:\n  workspace: {}\n  default_agent: none\nagents:\n  none: {{provider: none}}\nproviders:\n  none: {{type: openai-compatible, base_url: 'http://127.0.0.1:9/v1', model: none}}\nmemory:\n  postgres:\n    connection_string: {url}\ngateway:\n  users_file: {}\n",
            home.path().display(),
            state.join("users.json").display()
        ),
    )?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_rh"))
        .arg("mcp")
        .env_clear()
        .env("HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn_guarded()?;
    let stdout = child.stdout.take().ok_or("stdout")?;
    let stdin = child.stdin.take().ok_or("stdin")?;
    let client = tokio::time::timeout(
        Duration::from_secs(30),
        rmcp::model::ClientConfig::default().serve((stdout, stdin)),
    )
    .await??;
    let info = client.peer_info().ok_or("initialize")?;
    let instructions = info
        .instructions
        .clone()
        .ok_or("shared instructions missing")?;
    client.cancel().await?;
    child.start_kill()?;
    child.wait().await?;
    Ok(instructions)
}

struct Api {
    router: axum::Router,
    _root: tempfile::TempDir,
}

impl Api {
    async fn new(pool: &PgPool) -> TestResult<Self> {
        seed(pool).await?;
        roles(pool).await?;
        let options = pool
            .connect_options()
            .as_ref()
            .clone()
            .options([("role", READER)]);
        let reader = memory_postgres::scoped(PgPoolOptions::new().max_connections(2))
            .connect_with(options)
            .await?;
        let store = Arc::new(PostgresMemory::new(reader.clone()));
        let tenancy = Tenancy::local(UserId::new("node-owner".into())?)
            .with_spaces(vec![team()?])
            .with_token("alex-token", UserId::new("alex".into())?)?;
        let root = tempfile::tempdir()?;
        let gateway = gateway::Gateway::new(store.clone(), crate::topics::settings(root.path()))
            .with_local_owner(crate::owner::holder())
            .with_tenancy(tenancy)
            .with_tenancy_store(Arc::new(PostgresTenancy::new(reader)))
            .with_session_catalog(store.clone())
            .with_session_deletion(store);
        Ok(Self {
            router: Arc::new(gateway).router(true),
            _root: root,
        })
    }

    async fn get(&self, path: &str) -> TestResult<Vec<u8>> {
        let mut request = Request::builder()
            .uri(path)
            .header(header::HOST, "127.0.0.1:5975")
            .header(header::AUTHORIZATION, "Bearer alex-token")
            .body(Body::empty())?;
        request
            .extensions_mut()
            .insert(ConnectInfo("192.0.2.7:40000".parse::<SocketAddr>()?));
        let response = self.router.clone().oneshot(request).await?;
        assert_eq!(response.status().as_u16(), 200, "{path}");
        Ok(to_bytes(response.into_body(), 1024 * 1024).await?.to_vec())
    }
}

async fn memory_owners(api: &Api) -> TestResult {
    let raw = api.get("/api/memory/search?q=release&mode=fts").await?;
    assert_eq!(
        String::from_utf8_lossy(&raw).matches("\"owner\":").count(),
        1
    );
    let search: MemorySearchResponse = serde_json::from_slice(&raw)?;
    assert_eq!(search.results.len(), 2);
    for key in [SHARED, OWN] {
        let hit = search
            .results
            .iter()
            .find(|hit| hit.session_id.as_deref() == Some(key))
            .ok_or("search hit")?;
        assert_eq!(hit.owner.as_deref(), (key == SHARED).then_some("phil"));
    }
    let raw = api.get("/api/memory/browse").await?;
    assert_eq!(
        String::from_utf8_lossy(&raw).matches("\"owner\":").count(),
        1
    );
    let browse: MemoryBrowseResponse = serde_json::from_slice(&raw)?;
    assert_eq!(browse.messages.len(), 2);
    for key in [SHARED, OWN] {
        let hit = browse
            .messages
            .iter()
            .find(|hit| hit.session_id.as_deref() == Some(key))
            .ok_or("browse hit")?;
        assert_eq!(hit.owner.as_deref(), (key == SHARED).then_some("phil"));
    }
    Ok(())
}

#[tokio::test]
async fn non_owner_gateway_memory_rows_name_teammates_and_omit_the_viewer_owner() -> TestResult {
    let db = crate::pgdb::TestDb::migrated().await?;
    memory_owners(&Api::new(&db).await?).await
}

#[tokio::test]
#[ignore = "requires fix/team-hub-sessions: catalog must use the scoped store instead of the gateway owners file"]
async fn non_owner_gateway_owner_fields_cover_search_browse_and_session_catalog() -> TestResult {
    let db = crate::pgdb::TestDb::migrated().await?;
    let api = Api::new(&db).await?;
    memory_owners(&api).await?;
    let raw = api.get("/api/sessions").await?;
    assert_eq!(
        String::from_utf8_lossy(&raw).matches("\"owner\":").count(),
        1
    );
    let catalog: SessionSummaryPage = serde_json::from_slice(&raw)?;
    assert_eq!(catalog.items.len(), 2);
    for key in [SHARED, OWN] {
        let row = catalog
            .items
            .iter()
            .find(|row| row.id.to_string() == key)
            .ok_or("catalog row")?;
        assert_eq!(row.owner.as_deref(), (key == SHARED).then_some("phil"));
    }
    Ok(())
}
