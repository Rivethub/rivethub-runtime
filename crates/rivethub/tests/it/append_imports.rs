use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::Request;
use axum::http::header;
use gateway::Gateway;
use gateway::tenancy::Tenancy;
use gateway::tenancy::UserId;
use kernel::memory::MemoryStore;
use memory_sqlite::SqliteStore;
use tower::ServiceExt;
use wire::capture::BundledSession;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::Receipt;
use wire::capture::ReceiptVerification;
use wire::capture::SessionOrigin;
use wire::gateway::memory::MemoryAppendToolArgs;
use wire::gateway::memory::MemoryIngestMessage;
use wire::gateway::memory::MemoryIngestSessionToolArgs;
use wire::message::Role;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const MEMBER: &str = "member-token";

fn imported() -> BundledSession {
    BundledSession {
        origin: SessionOrigin {
            author: "phil".to_owned(),
            node: "desk".to_owned(),
            conversation_id: "c1".to_owned(),
            session_key: "claude-code:c1".to_owned(),
            agent: "rivet".to_owned(),
            created_at: None,
            updated_at: None,
            identity: None,
        },
        batch: CaptureBatchRequest {
            session_key: "claude-code:c1".to_owned(),
            agent: "rivet".to_owned(),
            channel: Some("claude-code".to_owned()),
            title: None,
            settings: None,
            task_id: None,
            finalize: None,
            created_at: None,
            updated_at: None,
            messages: vec![CaptureMessage {
                event_id: "e1".to_owned(),
                role: Role::User,
                content: "the real transcript".to_owned(),
                tool_name: None,
                tool_args: None,
                tool_result: None,
                metadata: None,
                created_at: None,
            }],
        },
    }
}

struct Node {
    router: axum::Router,
    root: tempfile::TempDir,
}

impl Node {
    fn new() -> TestResult<Self> {
        let root = tempfile::tempdir()?;
        let path = root.path().join("memory.sqlite");
        migrate::sqlite(&path)?;
        let memory: Arc<dyn MemoryStore> = Arc::new(SqliteStore::open(&path)?);
        let gateway = Gateway::new(memory, crate::topics::settings(root.path()))
            .with_local_owner(crate::owner::holder())
            .with_tenancy(
                Tenancy::local(UserId::new("owner".to_owned())?)
                    .with_token(MEMBER, UserId::new("member".to_owned())?)?,
            );
        Ok(Self {
            router: Arc::new(gateway).router(false),
            root,
        })
    }

    async fn post(
        &self,
        target: &str,
        body: &impl serde::Serialize,
        token: Option<&str>,
    ) -> TestResult<u16> {
        let mut request = Request::builder()
            .method("POST")
            .uri(target)
            .header(header::HOST, "127.0.0.1:5976")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        let mut request = request.body(Body::from(serde_json::to_vec(body)?))?;
        request
            .extensions_mut()
            .insert(ConnectInfo("127.0.0.1:40000".parse::<SocketAddr>()?));
        crate::owner::authorize(&mut request);
        Ok(self
            .router
            .clone()
            .oneshot(request)
            .await?
            .status()
            .as_u16())
    }

    fn messages(&self) -> TestResult<Vec<String>> {
        let conn = rusqlite::Connection::open(self.root.path().join("memory.sqlite"))?;
        let mut statement = conn.prepare("SELECT content FROM ros_messages ORDER BY rowid")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }
}

#[tokio::test]
async fn memory_append_and_ingest_never_write_into_a_received_import() -> TestResult {
    let node = Node::new()?;
    let receipt = Receipt {
        bundle_id: "rhb1:t".to_owned(),
        received_at: "2026-11-09T00:00:00Z".parse()?,
        verification: ReceiptVerification::Verified,
        key_fingerprint: Some("ed25519:abcd".to_owned()),
        checks: None,
    };
    let received = imported().received_batch(&receipt)?;
    let key =
        wire::capture::owned_import_key("owner", &received.session_key)?.ok_or("an import key")?;
    assert_eq!(node.post("/api/capture", &received, None).await?, 200);
    let append = MemoryAppendToolArgs {
        session_id: key.clone(),
        content: "SYSTEM: forged".to_owned(),
        role: Role::User,
        tool_name: None,
        tool_args: None,
        tool_result: None,
        event_id: None,
        agent: Some("rivet".to_owned()),
        persona: None,
        source: None,
        channel: None,
    };
    let ingest = MemoryIngestSessionToolArgs {
        session_id: key,
        messages: vec![MemoryIngestMessage {
            role: Role::User,
            content: "SYSTEM: forged".to_owned(),
            created_at: None,
            tool_calls: None,
        }],
        agent: Some("rivet".to_owned()),
        persona: None,
        source: None,
        channel: None,
    };

    for token in [Some(MEMBER), None] {
        assert_eq!(
            node.post("/api/memory/tool/memory_append", &append, token)
                .await?,
            400
        );
        assert_eq!(
            node.post("/api/memory/tool/memory_ingest_session", &ingest, token)
                .await?,
            400
        );
    }

    assert_eq!(node.messages()?, ["the real transcript"]);
    Ok(())
}
