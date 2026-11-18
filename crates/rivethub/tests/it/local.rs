#[path = "local/backfill.rs"]
mod backfill;
#[path = "local/capture.rs"]
mod capture;
#[path = "local/consolidate.rs"]
mod consolidate;
#[path = "local/embed.rs"]
mod embed;
#[path = "local/import.rs"]
mod import;
#[path = "local/memory_http.rs"]
mod memory_http;
#[path = "local/summarize.rs"]
mod summarize;

use std::path::Path;
use std::time::Duration;

use kernel::memory::MemoryEntry;
use tokio::process::Command;
use wire::gateway::HealthzResponse;
use wire::gateway::memory::MemoryBrowseResponse;
use wire::message::Role;

use crate::process::{Started, TestResult, provider_at, stop, write_state};

const SUMMARY_ID: &str = "0199e2a0-7b3c-7a00-8000-000000000001";
const WIKI_PATCH: &str = concat!(
    r#"[{"action":"create","slug":"acmekit-deploys","title":"AcmeKit deploys","#,
    r#""summary":"AcmeKit deploys on Fridays.","aliases":[],"tags":[],"entities":[]}]"#
);
const REPLY: &str = "the ledger is finished";

async fn start(home: &Path, args: &[&str], needles: &'static [&str]) -> TestResult<Started> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rh"));
    command
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("TOKIO_WORKER_THREADS", "2")
        .current_dir(home);
    crate::process::start(command, needles).await
}

fn provider(home: &Path, extra: &str) -> String {
    provider_at(home, "http://127.0.0.1:1", extra)
}

#[tokio::test]
async fn local_refuses_workers_when_no_model_is_configured() -> TestResult {
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    let database = home.path().join(".rivethub").join("memory.sqlite");
    write_state(
        home.path(),
        &format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n    workers: true\ngateway:\n  host: 127.0.0.1\n  port: {port}\n",
            database.display()
        ),
    )?;
    let started = Command::new(env!("CARGO_BIN_EXE_rh"))
        .args(["local", "--no-lan"])
        .env_clear()
        .env("HOME", home.path())
        .env("TOKIO_WORKER_THREADS", "2")
        .output()
        .await?;
    let stderr = String::from_utf8(started.stderr)?;
    assert_eq!(
        started.status.code(),
        Some(1),
        "{stderr}{}",
        String::from_utf8_lossy(&started.stdout)
    );
    assert_eq!(stderr, format!("Error: {}\n", config::NO_MODEL_NOTE));
    assert_eq!(started.stdout, b"");
    assert!(!home.path().join(".rivethub/rivethub.pid").exists());
    Ok(())
}

#[tokio::test]
async fn local_serves_health_and_sqlite_memory_then_stops_on_sigterm() -> TestResult {
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    let port_text = port.to_string();
    let started = start(
        home.path(),
        &["local", "--no-lan", "--port", &port_text],
        &["gateway listening on "],
    )
    .await?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .default_headers(crate::owner::home_headers(home.path())?)
        .build()?;
    let base = format!("http://127.0.0.1:{port}");
    let health = client
        .get(format!("{base}/healthz"))
        .send()
        .await?
        .error_for_status()?
        .json::<HealthzResponse>()
        .await?;
    assert!(health.ok);
    let database = home.path().join(".rivethub").join("memory.sqlite");
    let marker = "local-sqlite-marker";
    {
        let store = memory_sqlite::SqliteStore::open(&database)?;
        let now = chrono::Utc::now();
        store
            .append_entries(
                vec![MemoryEntry {
                    provenance: None,
                    id: None,
                    session_id: "local-e2e".to_owned(),
                    agent: "main".to_owned(),
                    channel: "test".to_owned(),
                    role: Role::User,
                    content: marker.to_owned(),
                    tool_name: None,
                    tool_args: None,
                    tool_result: None,
                    metadata: None,
                    created_at: Some(now),
                }],
                &testkit::owner()?,
                now,
            )
            .await?;
    }
    let browse = client
        .get(format!("{base}/api/memory/browse"))
        .send()
        .await?
        .error_for_status()?
        .json::<MemoryBrowseResponse>()
        .await?;
    assert!(
        browse
            .messages
            .iter()
            .any(|message| message.content == marker),
        "{browse:?}"
    );
    crate::store_routes::none_unavailable(&client, &base).await?;
    let text = std::fs::read_to_string(home.path().join(".rivethub/config.yaml"))?;
    assert!(text.contains("backend: sqlite"), "{text}");
    assert!(!text.contains("agents"), "{text}");
    assert!(!text.contains("providers"), "{text}");
    assert!(!text.contains("runtime"), "{text}");
    let logs = stop(started).await?;
    assert!(logs.contains("SIGTERM received — shutting down"), "{logs}");
    assert!(logs.contains("gateway stopped"), "{logs}");
    assert!(!home.path().join(".rivethub/rivethub.pid").exists());
    Ok(())
}

#[tokio::test]
async fn sqlite_builds_a_wiki_topic_from_a_seeded_summary() -> TestResult {
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    let stub = model_stub().await?;
    let database = home.path().join(".rivethub").join("memory.sqlite");
    let wiki = home.path().join("wiki");
    std::fs::create_dir_all(&wiki)?;
    write_state(
        home.path(),
        &provider_at(
            home.path(),
            &format!("http://{}", stub.addr),
            &format!(
                "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n    wiki_dir: {}\ngateway:\n  host: 127.0.0.1\n  port: {port}\n",
                database.display(),
                wiki.display()
            ),
        ),
    )?;
    seed_summary(&database)?;
    let built = wiki_topic(home.path(), &database).await;
    let closed = stub.close().await;
    built?;
    closed?;
    Ok(())
}

async fn wiki_topic(home: &Path, database: &Path) -> TestResult {
    let started = start(
        home,
        &["local", "--no-lan"],
        &[
            "worker running wiki-builder",
            "wiki topic stored acmekit-deploys",
        ],
    )
    .await?;
    let title = topic_title(database);
    stop(started).await?;
    assert_eq!(title?, "AcmeKit deploys");
    Ok(())
}

#[tokio::test]
async fn sqlite_worker_roles_and_mcp_share_the_process() -> TestResult {
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    let database = home.path().join(".rivethub").join("memory.sqlite");
    let socket = home.path().join("mcp.sock");
    write_state(
        home.path(),
        &provider(
            home.path(),
            &format!(
                "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n    workers: true\n    tagging: true\ngateway:\n  host: 127.0.0.1\n  port: {port}\ntransports:\n  mcp:\n    socket: {}\n",
                database.display(),
                socket.display()
            ),
        ),
    )?;
    let started = start(
        home.path(),
        &["local", "--no-lan"],
        &[
            "gateway listening on ",
            "worker running embedder",
            "worker running summarizer",
            "worker running categorizer",
            "mcp listening on ",
        ],
    )
    .await?;
    let logs = stop(started).await?;
    assert!(logs.contains("SIGTERM received — shutting down"), "{logs}");
    assert!(!socket.exists(), "{logs}");
    assert!(!home.path().join(".rivethub/rivethub.pid").exists());
    Ok(())
}

#[derive(serde::Serialize)]
struct EmbedTargetPayload<'a> {
    #[serde(rename = "targetTable")]
    target_table: &'a str,
    #[serde(rename = "targetId")]
    target_id: &'a str,
}

const MESSAGE_ID: &str = "0199e2a0-7b3c-7a00-8000-0000000000b1";
const CONVERSATION_ID: &str = "0199e2a0-7b3c-7a00-8000-0000000000b2";
const JOB_ID: &str = "0199e2a0-7b3c-7a00-8000-0000000000b3";

fn seed_embed_job(path: &Path) -> TestResult {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    migrate::sqlite(path)?;
    let conn = rusqlite::Connection::open(path)?;
    let now = "2020-01-01T00:00:00.000Z";
    conn.execute(
        "INSERT INTO ros_conversations (id, session_key, agent, created_at, updated_at)
         VALUES (?1, 'shutdown', 'rivet', ?2, ?2)",
        (CONVERSATION_ID, now),
    )?;
    conn.execute(
        "INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, created_at)
         VALUES (?1, ?2, 'rivet', 'test', 'user', 'hello ledger', ?3)",
        (MESSAGE_ID, CONVERSATION_ID, now),
    )?;
    let payload = serde_json::to_string(&wire::JsonObject::from_serialize(&EmbedTargetPayload {
        target_table: "ros_messages",
        target_id: MESSAGE_ID,
    })?)?;
    conn.execute(
        "INSERT INTO ros_jobs (
            id, task, job_key, payload, run_at, attempts, max_attempts, state, created_at, updated_at
         ) VALUES (?1, 'embed-target', ?2, ?3, ?4, 0, 5, 'queued', ?4, ?4)",
        (
            JOB_ID,
            format!("embed-ros_messages-{MESSAGE_ID}"),
            payload,
            now,
        ),
    )?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
struct SqliteJob {
    state: String,
    attempts: i64,
    locked_at: Option<String>,
    last_error: Option<String>,
}

fn sqlite_job(path: &Path) -> TestResult<SqliteJob> {
    let conn = rusqlite::Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    let row = conn.query_row(
        "SELECT state, attempts, locked_at, last_error FROM ros_jobs WHERE id = ?1",
        [JOB_ID],
        |row| {
            Ok(SqliteJob {
                state: row.get(0)?,
                attempts: row.get(1)?,
                locked_at: row.get(2)?,
                last_error: row.get(3)?,
            })
        },
    )?;
    Ok(row)
}

async fn wait_running(database: &Path) -> Result<(), String> {
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match sqlite_job(database) {
                Ok(job) if job.state == "running" && job.attempts == 1 => return Ok(()),
                Ok(_) => tokio::task::yield_now().await,
                Err(error) => return Err(error.to_string()),
            }
        }
    })
    .await;
    match result {
        Ok(inner) => inner,
        Err(_) => Err("embed job was not claimed".to_owned()),
    }
}

#[tokio::test]
async fn local_blocked_on_a_provider_releases_the_job_on_sigint() -> TestResult {
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    let provider = crate::process::blackhole().await?;
    let database = home.path().join(".rivethub").join("memory.sqlite");
    write_state(
        home.path(),
        &provider_at(
            home.path(),
            &format!("http://{}", provider.address),
            &format!(
                "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n    workers: true\ngateway:\n  host: 127.0.0.1\n  port: {port}\n",
                database.display()
            ),
        ),
    )?;
    seed_embed_job(&database)?;
    let started = start(
        home.path(),
        &["local", "--no-lan"],
        &["gateway listening on ", "worker running embedder"],
    )
    .await?;
    let claimed = wait_running(&database).await;
    let Started { mut child, logs } = started;
    if let Err(error) = claimed {
        if let Some(pid) = child.id() {
            crate::process::signal(pid, nix::sys::signal::Signal::SIGINT)?;
        }
        let text = tokio::time::timeout(Duration::from_secs(5), logs).await???;
        return Err(format!("{error}\n{text}").into());
    }
    let pid = child.id().ok_or("rh local already stopped")?;
    let began = tokio::time::Instant::now();
    crate::process::signal(pid, nix::sys::signal::Signal::SIGINT)?;
    let waited = tokio::time::timeout(kernel::shutdown::SHUTDOWN_GRACE, child.wait()).await;
    let elapsed = began.elapsed();
    let Ok(status) = waited else {
        child.start_kill()?;
        let text = logs.await??;
        return Err(format!("rh local still running after {elapsed:?}\n{text}").into());
    };
    let status = status?;
    let text = tokio::time::timeout(Duration::from_secs(5), logs).await???;
    assert!(
        elapsed < kernel::shutdown::SHUTDOWN_GRACE,
        "{elapsed:?}\n{text}"
    );
    assert!(status.success(), "rh local exit: {status}\n{text}");
    assert!(text.contains("SIGINT received — shutting down"), "{text}");
    assert_eq!(
        sqlite_job(&database)?,
        SqliteJob {
            state: "queued".to_owned(),
            attempts: 0,
            locked_at: None,
            last_error: None,
        }
    );
    Ok(())
}

fn seed_summary(path: &Path) -> TestResult {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    migrate::sqlite(path)?;
    let conn = rusqlite::Connection::open(path)?;
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let conversation = "00000000-0000-4000-8000-0000000000c7";
    let session = format!("claude-code:{conversation}");
    let topic = "00000000-0000-4000-8000-0000000000e7";
    conn.execute(
        "INSERT INTO ros_conversations (id, session_key, agent, created_at, updated_at)
         VALUES (?1, ?2, 'rivet', ?3, ?3)",
        (conversation, &session, &now),
    )?;
    conn.execute(
        "INSERT INTO ros_summaries (id, conversation_id, content, created_at) VALUES (?1, ?2, ?3, ?4)",
        (SUMMARY_ID, conversation, "AcmeKit deploys on Fridays.", &now),
    )?;
    conn.execute(
        "INSERT INTO rh_topics (id, kind, slug, name, created_at, updated_at)
         VALUES (?1, 'work', 'acmekit-deploys', 'AcmeKit deploys', ?2, ?2)",
        (topic, &now),
    )?;
    conn.execute(
        "INSERT INTO rh_session_placements (session_id, topic_id, placed_by, placed_at, owner_user_id)
         VALUES (?1, ?2, 'you', ?3, 'owner')",
        (&session, topic, &now),
    )?;
    Ok(())
}

fn topic_title(path: &Path) -> TestResult<String> {
    let conn = rusqlite::Connection::open(path)?;
    let title = conn.query_row(
        "SELECT title FROM ros_wiki_topics WHERE slug = 'acmekit-deploys'",
        [],
        |row| row.get(0),
    )?;
    Ok(title)
}

async fn model_stub() -> std::io::Result<testkit::Stub> {
    testkit::stub_server(
        axum::Router::new()
            .route("/v1/chat/completions", axum::routing::post(complete))
            .route("/v1/embeddings", axum::routing::post(embed_model_stub)),
    )
    .await
}

async fn complete(
    body: axum::body::Bytes,
) -> Result<([(axum::http::HeaderName, &'static str); 1], String), (axum::http::StatusCode, String)>
{
    let text = std::str::from_utf8(&body)
        .map_err(|error| (axum::http::StatusCode::BAD_REQUEST, error.to_string()))?;
    let content = if text.contains("Extract durable") {
        WIKI_PATCH
    } else {
        REPLY
    };
    let payload = crate::process::provider::event_stream(content, None).map_err(|error| {
        (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            error.to_string(),
        )
    })?;
    Ok((
        [(axum::http::header::CONTENT_TYPE, "text/event-stream")],
        payload,
    ))
}

async fn embed_model_stub(
    axum::Json(request): axum::Json<wire::stream::openai::EmbeddingRequest>,
) -> axum::Json<wire::stream::openai::EmbeddingResponse> {
    use wire::stream::openai::{EmbeddingResponse, EmbeddingRow};
    axum::Json(EmbeddingResponse {
        data: request
            .input
            .iter()
            .enumerate()
            .map(|(index, _)| EmbeddingRow {
                index,
                embedding: vec![0.25; 1024],
            })
            .collect(),
    })
}
