use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use kernel::memory::MemoryStore as _;
use kernel::memory::SessionDigest;
use kernel::memory::share::DecideMode;
use kernel::memory::share::DecisionSummary;
use kernel::memory::share::FlagPlace;
use kernel::memory::share::FlagSummary;
use kernel::memory::share::ListedSession;
use kernel::memory::share::OpenedBundle;
use kernel::memory::share::SanitizeJob;
use kernel::memory::share::SanitizeSummary;
use kernel::memory::share::SanitizedBundle;
use kernel::memory::share::SessionShare;
use kernel::memory::share::ShareFailure;
use kernel::owner::Owner;
use mcp::Server;
use mcp::sessions::Sessions;
use mcp::sessions::ShareFiles;
use rmcp::model::CallToolResult;
use serde::Deserialize;
use serde::Serialize;
use tempfile::TempDir;
use testkit::FakeMemoryStore;
use testkit::MemoryRequest;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;
use wire::capture::BundledSession;
use wire::capture::CaptureBatchRequest;
use wire::capture::CaptureMessage;
use wire::capture::LOCAL_SUFFIX;
use wire::capture::REPORT_SUFFIX;
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

#[derive(Debug, Serialize, Deserialize)]
struct Stub {
    raw: bool,
    withheld: usize,
    sessions: Vec<BundledSession>,
}

#[derive(Debug)]
pub(crate) struct ScriptedShare {
    sessions: Vec<BundledSession>,
    decide: Option<DecideMode>,
}

fn write(path: &Path, stub: &Stub) {
    std::fs::write(path, serde_json::to_vec(stub).expect("stub")).expect("write stub");
}

fn read(path: &Path) -> Result<Stub, ShareFailure> {
    std::fs::read(path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .ok_or_else(|| ShareFailure::NotBundle(path.to_path_buf()))
}

fn beside(out: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!(
        "{}{suffix}",
        out.display().to_string().trim_end_matches(".json")
    ))
}

fn kernel_batch(session: &BundledSession) -> kernel::memory::CaptureBatchRequest {
    let batch = session.import_batch().expect("import batch");
    serde_json::from_value(serde_json::to_value(batch).expect("batch")).expect("kernel batch")
}

pub(crate) fn flagged() -> FlagSummary {
    FlagSummary {
        number: 1,
        score: 0.88,
        place: Some(FlagPlace {
            session: 0,
            message: Some(0),
            field: "content".to_owned(),
            more: 0,
        }),
        report_line: Some(12),
        chars: RISKY.chars().count(),
    }
}

#[async_trait]
impl SessionShare for ScriptedShare {
    async fn digests(&self, keys: &[String]) -> Result<Vec<SessionDigest>, ShareFailure> {
        Ok(self
            .sessions
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

    async fn export(&self, ids: &[Uuid], out: PathBuf) -> Result<usize, ShareFailure> {
        let mut chosen = Vec::new();
        for id in ids {
            match self
                .sessions
                .iter()
                .find(|session| session.origin.conversation_id == id.to_string())
            {
                Some(session) => chosen.push(session.clone()),
                None => return Err(ShareFailure::UnknownSessions(id.to_string())),
            }
        }
        let count = chosen.len();
        write(
            &out,
            &Stub {
                raw: true,
                withheld: 0,
                sessions: chosen,
            },
        );
        Ok(count)
    }

    async fn sanitize(&self, job: SanitizeJob) -> Result<SanitizeSummary, ShareFailure> {
        let mode = match (job.decide, self.decide) {
            (false, _) => None,
            (true, None) => return Err(ShareFailure::DecideUnconfigured),
            (true, Some(mode)) => Some(mode),
        };
        let raw = read(&job.input)?;
        let count = raw.sessions.len();
        let report = beside(&job.out, REPORT_SUFFIX);
        let local = mode.map(|_| beside(&job.out, LOCAL_SUFFIX));
        write(
            &job.out,
            &Stub {
                raw: false,
                withheld: usize::from(mode == Some(DecideMode::Block)),
                sessions: raw.sessions,
            },
        );
        std::fs::write(&report, "report").expect("report");
        Ok(SanitizeSummary {
            sessions: count,
            totals: "1 replaced, 0 hashed".to_owned(),
            rule_counts: "mapped 1".to_owned(),
            not_scanned: "not scanned: dates".to_owned(),
            best_effort: "best effort".to_owned(),
            bundle: job.out,
            report,
            local,
            decision: mode.map(|mode| DecisionSummary {
                model: "stub-model".to_owned(),
                scored: 2,
                calls: 1,
                trivial: 0,
                mode,
                flags: vec![flagged()],
            }),
        })
    }

    async fn open(&self, path: PathBuf) -> Result<OpenedBundle, ShareFailure> {
        let stub = read(&path)?;
        if stub.raw {
            return Ok(OpenedBundle::Raw);
        }
        Ok(OpenedBundle::Sanitized(SanitizedBundle {
            secrets: None,
            withheld: stub.withheld,
            unreviewed: 0,
            sessions: stub
                .sessions
                .iter()
                .map(|session| ListedSession {
                    key: session.origin.session_key.clone(),
                    agent: session.origin.agent.clone(),
                    messages: session.batch.messages.len(),
                    author: session.origin.author.clone(),
                    dates: session.origin.created_at.zip(session.origin.updated_at),
                })
                .collect(),
            authors: vec!["phil".to_owned()],
            batches: Ok(stub.sessions.iter().map(kernel_batch).collect()),
        }))
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

pub(crate) async fn node(
    owner: &str,
    sessions: Vec<BundledSession>,
    decide: Option<DecideMode>,
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
        share: Arc::new(ScriptedShare { sessions, decide }),
        files: ShareFiles::new(share.clone(), home.clone()),
        owner,
        node: "desk".to_owned(),
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
