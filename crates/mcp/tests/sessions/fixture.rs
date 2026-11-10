use std::num::NonZeroUsize;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use capture::sanitize::decide::Endpoint;
use capture::sanitize::decide::Mode;
use capture::sanitize::decide::Review;
use capture::share::Decider;
use capture::share::SetupError;
use chrono::DateTime;
use chrono::Utc;
use kernel::memory::MemoryStore as _;
use kernel::memory::MemoryStoreError;
use kernel::memory::SessionArchive;
use kernel::memory::SessionDigest;
use kernel::owner::Owner;
use mcp::Server;
use mcp::sessions::Sessions;
use mcp::sessions::ShareFiles;
use rmcp::model::CallToolResult;
use tempfile::TempDir;
use testkit::FakeMemoryStore;
use testkit::MemoryRequest;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use wire::capture::BundleAuthor;
use wire::capture::BundledSession;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::SessionOrigin;
use wire::message::Role;

pub(crate) const RISKY: &str = "Acme Corp";

pub(crate) fn at(text: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(text)
        .expect("time")
        .with_timezone(&Utc)
}

pub(crate) fn id(n: u8) -> Uuid {
    Uuid::parse_str(&format!("00000000-0000-4000-8000-{n:012}")).expect("uuid")
}

fn said(event_id: &str, content: &str, created_at: &str) -> CaptureMessage {
    CaptureMessage {
        event_id: event_id.to_owned(),
        role: Role::User,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: Some(at(created_at)),
    }
}

fn session(
    n: u8,
    key: &str,
    title: &str,
    agent: &str,
    messages: Vec<CaptureMessage>,
) -> BundledSession {
    let first = messages.first().and_then(|message| message.created_at);
    let last = messages.last().and_then(|message| message.created_at);
    BundledSession {
        origin: SessionOrigin {
            author: "phil".to_owned(),
            node: "desk".to_owned(),
            conversation_id: id(n).to_string(),
            session_key: key.to_owned(),
            agent: agent.to_owned(),
            created_at: first,
            updated_at: last,
            identity: None,
        },
        batch: CaptureBatchRequest {
            session_key: key.to_owned(),
            agent: agent.to_owned(),
            channel: None,
            title: Some(title.to_owned()),
            settings: None,
            task_id: None,
            finalize: None,
            created_at: first,
            updated_at: last,
            messages,
        },
    }
}

pub(crate) fn sessions() -> Vec<BundledSession> {
    vec![
        session(
            1,
            "claude-code:alpha",
            "SQLite bundle v1 to v2",
            "claude-code",
            vec![
                said(
                    "evt-a1",
                    "migrate the sqlite bundle reader",
                    "2026-10-12T09:00:00Z",
                ),
                said(
                    "evt-a2",
                    "the bundle reader now reads v1",
                    "2026-10-13T10:00:00Z",
                ),
            ],
        ),
        session(
            2,
            "codex:beta",
            "Deploy notes",
            "codex",
            vec![said(
                "evt-b1",
                "bundle deploy checklist",
                "2026-10-14T08:00:00Z",
            )],
        ),
        session(
            3,
            "claude-code:gamma",
            "Rollout pairing",
            "claude-code",
            vec![said(
                "evt-c1",
                &format!("we paired with {RISKY} on the rollout"),
                "2026-10-15T09:00:00Z",
            )],
        ),
    ]
}

#[derive(Debug)]
pub(crate) struct FixtureArchive(Vec<BundledSession>);

#[async_trait]
impl SessionArchive for FixtureArchive {
    type Session = BundledSession;

    async fn digests(&self, keys: &[String]) -> Result<Vec<SessionDigest>, MemoryStoreError> {
        Ok(self
            .0
            .iter()
            .filter(|session| keys.contains(&session.batch.session_key))
            .map(|session| {
                let times: Vec<DateTime<Utc>> = session
                    .batch
                    .messages
                    .iter()
                    .filter_map(|message| message.created_at)
                    .collect();
                SessionDigest {
                    key: session.batch.session_key.clone(),
                    conversation_id: Uuid::parse_str(&session.origin.conversation_id)
                        .expect("fixture id"),
                    title: session.batch.title.clone(),
                    agent: session.batch.agent.clone(),
                    first: *times.iter().min().expect("dated"),
                    last: *times.iter().max().expect("dated"),
                    messages: session.batch.messages.len() as u64,
                    imported_from: None,
                }
            })
            .collect())
    }

    async fn export(&self, ids: &[Uuid]) -> Result<Vec<BundledSession>, MemoryStoreError> {
        Ok(self
            .0
            .iter()
            .filter(|session| {
                ids.iter()
                    .any(|id| id.to_string() == session.origin.conversation_id)
            })
            .cloned()
            .collect())
    }
}

#[derive(Debug)]
pub(crate) struct Node {
    pub(crate) server: Server,
    pub(crate) store: Arc<FakeMemoryStore>,
    pub(crate) home: PathBuf,
    pub(crate) share: PathBuf,
    pub(crate) outside: PathBuf,
    _directory: TempDir,
}

pub(crate) fn unconfigured() -> Result<Decider, SetupError> {
    Err(SetupError::Unconfigured)
}

pub(crate) fn blocking(url: &str) -> Decider {
    Decider {
        endpoint: Endpoint {
            url: url.to_owned(),
            concurrency: NonZeroUsize::MIN,
        },
        review: Review {
            top: NonZeroUsize::new(5).expect("top"),
            threshold: 0.3,
            mode: Mode::Block,
        },
    }
}

pub(crate) async fn node(
    owner: &str,
    sessions: Vec<BundledSession>,
    decider: Result<Decider, SetupError>,
) -> Node {
    let directory = tempfile::tempdir().expect("directory");
    let home = directory.path().join("home");
    let share = home.join(".local/share/rivethub/share");
    let outside = directory.path().join("outside");
    std::fs::create_dir_all(&home).expect("home");
    std::fs::create_dir_all(&outside).expect("outside");
    let store = Arc::new(FakeMemoryStore::default());
    let owner = Owner::new(owner.to_owned()).expect("owner");
    for session in &sessions {
        let batch: kernel::memory::CaptureBatchRequest =
            serde_json::from_str(&serde_json::to_string(&session.batch).expect("batch"))
                .expect("kernel batch");
        store
            .capture(batch, &owner, Some("desk"))
            .await
            .expect("seed");
    }
    let kit = Sessions {
        archive: Arc::new(FixtureArchive(sessions)),
        files: ShareFiles::new(share.clone(), home.clone()),
        author: BundleAuthor {
            user: owner.as_str().to_owned(),
            node: "desk".to_owned(),
        },
        owner,
        mapping: config::MappingFile::Default(home.join("no-mapping.toml")),
        key: directory.path().join("state/rivethub/sanitize.key"),
        decider,
        redaction: None,
    };
    let server = Server::builder(store.clone(), false, CancellationToken::new())
        .sessions(kit)
        .build()
        .expect("server");
    Node {
        server,
        store,
        home,
        share,
        outside,
        _directory: directory,
    }
}

pub(crate) fn text(result: &CallToolResult) -> String {
    result
        .content
        .first()
        .and_then(|block| block.as_text())
        .expect("text content")
        .text
        .clone()
}

pub(crate) async fn appended(store: &FakeMemoryStore) -> Vec<kernel::memory::MemoryEntry> {
    store
        .requests()
        .await
        .into_iter()
        .filter_map(|request| {
            if let MemoryRequest::Append(_, entries) = request {
                Some(entries)
            } else {
                None
            }
        })
        .flatten()
        .filter(|entry| entry.session_id.starts_with("import:"))
        .collect()
}

pub(crate) fn path_text(path: &Path) -> String {
    path.display().to_string()
}
