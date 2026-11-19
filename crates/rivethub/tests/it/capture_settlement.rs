use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::{Body, to_bytes};
use axum::extract::ConnectInfo;
use axum::http::{Request, header};
use axum::response::Response;
use capture::hook::ClaudeHook;
use capture::spool::CaptureSpool;
use capture::writer::{CaptureTransport, CaptureWriter, PostError};
use gateway::Gateway;
use gateway::tenancy::{Tenancy, UserId};
use jiff::Timestamp;
use kernel::memory::{CaptureBatchRequest, MemoryStore, SummaryStore};
use kernel::owner::Owner;
use kernel::tenancy::{Scope, Viewer};
use parking_lot::Mutex;
use serde::de::DeserializeOwned;
use testkit::settlement::{batch, message, observed};
use tower::ServiceExt as _;
use uuid::Uuid;
use wire::capture::CaptureResult;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const CLEO: &str = "cleo-capture-token";

const BATCH: &str = r#"{"session_key":"cli:owned","agent":"rivet","channel":"cli","messages":[{"event_id":"e1","role":"user","content":"hello"}]}"#;

async fn post(app: &Router, path: &str, body: &str, token: Option<&str>) -> TestResult<Response> {
    let mut request = Request::post(path)
        .header(header::HOST, "127.0.0.1:5174")
        .header(header::CONTENT_TYPE, "application/json")
        .extension(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 41000))));
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    Ok(app
        .clone()
        .oneshot(request.body(Body::from(body.to_owned()))?)
        .await?)
}

async fn read_json<T: DeserializeOwned>(response: Response) -> TestResult<T> {
    let body = to_bytes(response.into_body(), 4096).await?;
    Ok(serde_json::from_slice(&body)?)
}

#[tokio::test]
async fn top_down_settle_requires_the_owner_and_persists_the_reply() -> TestResult {
    use sha2::{Digest, Sha256};
    use wire::gateway::users::{ScopedToken, TokenScope, UserToken};
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("settlement.sqlite");
    migrate::sqlite(&path)?;
    let store = Arc::new(memory_sqlite::SqliteStore::open(&path)?);
    let dana = UserId::new("dana".to_owned())?;
    let drops = UserToken::Scoped(ScopedToken {
        digest: hex::encode(Sha256::digest("dana-drops-only")),
        scopes: vec![TokenScope::DropsWrite],
    });
    let tenancy = Tenancy::local(dana.clone())
        .with_token("dana-full", dana.clone())?
        .with_token(CLEO, UserId::new("cleo".to_owned())?)?
        .with_users_file_token(&drops, dana)?;
    let settings = gateway::Settings {
        hostname: "gold-host".to_owned(),
        node: "golden-node".to_owned(),
        allowed_origins: vec!["https://hub.example.test".to_owned()],
        allowed_hosts: vec!["den.example.test".to_owned()],
        static_dir: None,
        root_redirect: None,
        state_dir: dir.path().join("state"),
        private_ca: None,
        request_deadline: gateway::REQUEST_DEADLINE,
        policy: None,
    };
    let app = Arc::new(Gateway::new(store.clone(), settings).with_tenancy(tenancy)).router(false);
    let captured = post(&app, "/api/capture", BATCH, Some("dana-full")).await?;
    assert_eq!(captured.status(), 200);
    let capture: wire::capture::CaptureResult = read_json(captured).await?;
    let url = format!(
        "/api/memory/conversations/{}/settle",
        capture.conversation_id
    );
    for (token, status) in [
        (None, 401),
        (Some(CLEO), 404),
        (Some("dana-drops-only"), 403),
    ] {
        let response = post(&app, &url, "", token).await?;
        assert_eq!(response.status(), status);
    }
    let accepted = post(&app, &url, "", Some("dana-full")).await?;
    assert_eq!(accepted.status(), 202);
    let accepted: kernel::memory::settlement::SettleResult = read_json(accepted).await?;
    let id = accepted.conversation_id;
    assert_eq!(
        accepted.status,
        kernel::memory::settlement::SettleStatus::Settled
    );
    assert_eq!(accepted.input_revision, 1);
    let retry = post(&app, &url, "", Some("dana-full")).await?;
    assert_eq!(retry.status(), 202);
    let retry: kernel::memory::settlement::SettleResult = read_json(retry).await?;
    assert_eq!(
        retry.status,
        kernel::memory::settlement::SettleStatus::AlreadySettled
    );
    assert_eq!(accepted.settled_at, retry.settled_at);
    let reopened = memory_sqlite::SqliteStore::open(&path)?;
    assert!(reopened.summary_eligible(id, 1).await?);
    Ok(())
}

const PARENT: &str = "claude-code:fixture-parent";
const CHILD: &str = "claude-code:fixture-child";
const HARVESTED: &str = "claude-code:fixture-harvested";
const FIRST: &str = "2026-01-02T00:00:00Z";
const SECOND: &str = "2026-01-02T00:01:00Z";

struct HookStore<'a> {
    store: &'a dyn MemoryStore,
    owner: &'a Owner,
    captures: &'a Mutex<Vec<(String, CaptureResult)>>,
}

#[async_trait::async_trait]
impl CaptureTransport for HookStore<'_> {
    async fn post(&self, body: String) -> Result<CaptureResult, PostError> {
        let error = |message| PostError::Network { message };
        let batch: CaptureBatchRequest =
            serde_json::from_str(&body).map_err(|err| error(err.to_string()))?;
        let key = batch.session_key.clone();
        let result = self
            .store
            .capture(batch, self.owner, None)
            .await
            .map_err(|err| error(err.to_string()))?;
        let reply = CaptureResult {
            ok: result.ok,
            conversation_id: result.conversation_id,
            inserted: result.inserted,
            skipped: result.skipped,
        };
        self.captures.lock().push((key, reply.clone()));
        Ok(reply)
    }
}

struct SessionFiles(tempfile::TempDir);

impl SessionFiles {
    fn new() -> TestResult<Self> {
        let files = Self(tempfile::tempdir()?);
        std::fs::create_dir_all(files.0.path().join("fixture-parent/subagents"))?;
        let row = format!(
            r#"{{"type":"user","uuid":"one","timestamp":"{FIRST}","message":{{"role":"user","content":"Sort the violet buttons."}}}}"#
        );
        for path in [files.parent(), files.child("child")] {
            std::fs::write(path, &row)?;
        }
        Ok(files)
    }

    fn parent(&self) -> std::path::PathBuf {
        self.0.path().join("fixture-parent.jsonl")
    }

    fn child(&self, id: &str) -> std::path::PathBuf {
        self.0
            .path()
            .join(format!("fixture-parent/subagents/agent-fixture-{id}.jsonl"))
    }

    async fn run(
        &self,
        store: &dyn MemoryStore,
        owner: &Owner,
        event: &str,
        child: &str,
        at: &str,
    ) -> TestResult<Vec<(String, CaptureResult)>> {
        let captures = Mutex::default();
        let transport = HookStore {
            store,
            owner,
            captures: &captures,
        };
        let home = self.0.path();
        let env = config::Environment::from_vars([("HOME".to_owned(), home.display().to_string())]);
        let hook = ClaudeHook::new(
            CaptureWriter::new(transport, CaptureSpool::new(home.join("spool")), None, None),
            &config::Paths::new(&env)?,
        );
        let input = format!(
            r#"{{"hook_event_name":"{event}","session_id":"fixture-parent","transcript_path":"{}","agent_id":"fixture-{child}","agent_transcript_path":"{}","event_id":"{event}/{at}"}}"#,
            self.parent().display(),
            self.child(child).display()
        );
        let result = hook.run(&input, at.parse()?, "fixture").await;
        assert_eq!((result.exit, result.stderr.as_str()), (0, ""));
        drop(hook);
        Ok(captures.into_inner())
    }
}

fn captured(posts: &[(String, CaptureResult)], session: &str) -> TestResult<Uuid> {
    let (_, result) = posts
        .iter()
        .find(|(key, _)| key == session)
        .ok_or("capture")?;
    Ok(result.conversation_id.parse()?)
}

async fn child_stop(store: &(impl MemoryStore + SummaryStore), owner: &Owner) -> TestResult {
    let files = SessionFiles::new()?;
    let initial = files.run(store, owner, "Stop", "child", FIRST).await?;
    let parent = captured(&initial, PARENT)?;
    let child = captured(&initial, CHILD)?;
    let before = store.summary_state(parent).await?;
    assert!(!store.summary_eligible(child, 1).await?);

    std::fs::write(files.child("child"), "")?;
    let empty = files
        .run(store, owner, "SubagentStop", "child", SECOND)
        .await?;
    assert_eq!(empty.len(), 1);
    assert_eq!(captured(&empty, CHILD)?, child);
    assert_eq!(empty.first().ok_or("empty stop")?.1.inserted, 0);
    assert!(store.summary_eligible(child, 1).await?);
    assert_eq!(store.summary_state(parent).await?, before);
    let settled = store.summary_state(child).await?.ok_or("settled child")?;
    assert_eq!(
        settled.last_observed_end_at,
        Some(SECOND.parse::<Timestamp>()?)
    );

    std::fs::write(files.child("ghost"), "")?;
    let ghost = files
        .run(store, owner, "SubagentStop", "ghost", SECOND)
        .await?;
    assert_eq!(ghost.len(), 1);
    assert_eq!(ghost.first().ok_or("ghost stop")?.1.conversation_id, "");

    std::fs::copy(files.parent(), files.child("harvested"))?;
    let posts = files
        .run(store, owner, "SessionEnd", "child", SECOND)
        .await?;
    assert!(store.summary_eligible(parent, 1).await?);
    let harvested = captured(&posts, HARVESTED)?;
    assert!(!store.summary_eligible(harvested, 1).await?);
    let finished = files
        .run(store, owner, "SubagentStop", "harvested", SECOND)
        .await?;
    assert_eq!(captured(&finished, HARVESTED)?, harvested);
    assert!(store.summary_eligible(harvested, 1).await?);

    std::fs::copy(files.parent(), files.child("new"))?;
    let finished = files
        .run(store, owner, "SubagentStop", "new", SECOND)
        .await?;
    assert_eq!(finished.first().ok_or("new child")?.1.inserted, 1);
    let id = captured(&finished, "claude-code:fixture-new")?;
    assert!(store.summary_eligible(id, 1).await?);
    Ok(())
}

#[tokio::test]
async fn subagent_stops_settle_on_sqlite() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("settlement.sqlite");
    migrate::sqlite(&path)?;
    let store = memory_sqlite::SqliteStore::open(&path)?;
    child_stop(&store, &testkit::owner()?).await
}

#[tokio::test]
async fn subagent_stops_settle_on_postgres_with_owner_scope() -> TestResult {
    let db = crate::pgdb::TestDb::migrated().await?;
    let pool = memory_postgres::scoped(sqlx::postgres::PgPoolOptions::new())
        .connect(&db.served_url().await?)
        .await?;
    let store = memory_postgres::PostgresMemory::new(pool);
    let owner = testkit::owner()?;
    let scope = Scope::Viewer(Viewer::new(owner.clone(), Vec::new()));
    scope.clone().run(child_stop(&store, &owner)).await?;
    assert_owner_only(&store, &owner, scope).await?;
    store.close().await?;
    Ok(())
}

async fn assert_owner_only(
    store: &(impl MemoryStore + SummaryStore),
    owner: &Owner,
    scope: Scope,
) -> TestResult {
    let key = "claude-code:fixture-owned";
    let initial = scope
        .clone()
        .run(store.capture(batch(key, vec![message("seed", FIRST)?]), owner, None))
        .await?;
    let id = initial.conversation_id.parse()?;
    let before = scope.clone().run(store.summary_state(id)).await?;
    let mut end = observed(batch(key, vec![]), "SubagentStop", SECOND, true)?;
    end.settings
        .as_mut()
        .ok_or("settings")?
        .insert("parent_session_id", "fixture-parent")?;
    let other = Owner::new("unrelated".to_owned())?;
    let foreign = Scope::Viewer(Viewer::new(other.clone(), Vec::new()));
    for claimed_owner in [&other, owner] {
        let attempt = foreign
            .clone()
            .run(store.capture(end.clone(), claimed_owner, None))
            .await?;
        assert_eq!(attempt.conversation_id, "");
    }
    assert_eq!(scope.clone().run(store.summary_state(id)).await?, before);
    scope.clone().run(store.capture(end, owner, None)).await?;
    assert!(scope.run(store.summary_eligible(id, 1)).await?);
    Ok(())
}

#[tokio::test]
async fn transcript_readers_agree_on_sqlite() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let store = memory_sqlite::SqliteStore::open(path)?;
    testkit::settlement::readers_agree(&store, &testkit::owner()?, async |id: Uuid| {
        store
            .with_conn(move |conn| {
                conn.execute(
                    "UPDATE ros_messages SET id = substr(conversation_id, 1, 35) ||
                    CASE json_extract(metadata, '$.event_id')
                    WHEN 'alpha' THEN '3' WHEN 'beta' THEN '2' ELSE '1' END
                 WHERE conversation_id = ?1",
                    [id.to_string()],
                )
                .map_err(|source| memory_sqlite::SqliteError::Query {
                    operation: "test order ties",
                    source,
                })?;
                Ok(())
            })
            .await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn transcript_readers_agree_on_postgres() -> TestResult {
    let db = crate::pgdb::TestDb::migrated().await?;
    let store = memory_postgres::PostgresMemory::new(db.pool());
    testkit::settlement::readers_agree(&store, &testkit::owner()?, async |id| {
        sqlx::query(
            "UPDATE rh_messages SET created_at = '2026-01-03T01:00:00Z' WHERE conversation_id = $1",
        )
        .bind(id)
        .execute(&*db)
        .await?;
        Ok(())
    })
    .await
}

#[tokio::test]
async fn opencode_resume_signals_do_not_settle_sqlite() -> TestResult {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("memory.sqlite");
    migrate::sqlite(&path)?;
    let store = memory_sqlite::SqliteStore::open(path)?;
    testkit::settlement::child_resume_signals(&store, &testkit::owner()?).await
}

#[tokio::test]
async fn opencode_resume_signals_do_not_settle_postgres() -> TestResult {
    let db = crate::pgdb::TestDb::migrated().await?;
    let store = memory_postgres::PostgresMemory::new(db.pool());
    testkit::settlement::child_resume_signals(&store, &testkit::owner()?).await
}
