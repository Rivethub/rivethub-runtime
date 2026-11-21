use std::path::Path;
use std::time::Duration;

use wire::stream::openai::{EmbeddingRequest, EmbeddingResponse, EmbeddingRow};

use super::{TestResult, provider_at, start, stop, write_state};

const CONVERSATION: &str = "0199e2a0-7b3c-7a00-8000-0000000000d1";
const MESSAGE: &str = "0199e2a0-7b3c-7a00-8000-0000000000d2";
const CONTENT: &str = "captured before any embedding endpoint was configured";

fn seed_unembedded(path: &Path) -> TestResult {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    migrate::sqlite(path)?;
    let conn = rusqlite::Connection::open(path)?;
    let now = "2026-01-01T00:00:00.000Z";
    conn.execute(
        "INSERT INTO ros_conversations (id, session_key, agent, created_at, updated_at)
         VALUES (?1, 'backfill', 'rivet', ?2, ?2)",
        (CONVERSATION, now),
    )?;
    conn.execute(
        "INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, created_at)
         VALUES (?1, ?2, 'rivet', 'test', 'user', ?3, ?4)",
        (MESSAGE, CONVERSATION, CONTENT, now),
    )?;
    Ok(())
}

fn embedded(path: &Path) -> TestResult<bool> {
    let conn = rusqlite::Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(conn.query_row(
        "SELECT embedding IS NOT NULL FROM ros_messages WHERE id = ?1",
        [MESSAGE],
        |row| row.get(0),
    )?)
}

async fn wait_embedded(path: &Path) -> TestResult<bool> {
    let waited = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match embedded(path) {
                Ok(true) => return Ok(()),
                Ok(false) => tokio::task::yield_now().await,
                Err(error) => return Err(error.to_string()),
            }
        }
    })
    .await;
    match waited {
        Ok(inner) => inner.map(|()| true).map_err(Into::into),
        Err(_) => Ok(false),
    }
}

fn embeddings() -> axum::Router {
    axum::Router::new().route(
        "/v1/embeddings",
        axum::routing::post(
            |axum::Json(request): axum::Json<EmbeddingRequest>| async move {
                let data = request
                    .input
                    .iter()
                    .enumerate()
                    .map(|(index, _)| EmbeddingRow {
                        index,
                        embedding: vec![0.6, 0.8],
                    })
                    .collect();
                axum::Json(EmbeddingResponse { data })
            },
        ),
    )
}

#[tokio::test]
async fn local_workers_embed_rows_that_were_never_queued() -> TestResult {
    let server = testkit::stub_server(embeddings()).await?;
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    let database = home.path().join(".rivethub").join("memory.sqlite");
    seed_unembedded(&database)?;
    write_state(
        home.path(),
        &provider_at(
            home.path(),
            &format!("http://{}/v1", server.addr),
            &format!(
                "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n    workers: true\n    embed_endpoint: http://{}\n    embed_model: embedding-model\ngateway:\n  host: 127.0.0.1\n  port: {port}\n",
                database.display(),
                server.addr
            ),
        ),
    )?;
    let started = start(
        home.path(),
        &["local", "--no-lan"],
        &["gateway listening on ", "worker running embedder"],
    )
    .await?;
    let swept = wait_embedded(&database).await;
    let logs = stop(started).await?;
    assert!(swept?, "{logs}");
    Ok(())
}

#[test]
fn a_sqlite_embedder_schedules_only_the_sweeps_its_queue_claims() -> TestResult {
    let home = tempfile::tempdir()?;
    let database = home.path().join("memory.sqlite");
    migrate::sqlite(&database)?;
    let store = memory_sqlite::SqliteStore::open(&database)?;
    let entries =
        workers::schedule::claimed_entries(&store, &[kernel::queue::WorkerRole::Embedder], true);
    let identifiers: Vec<&str> = entries
        .iter()
        .map(|entry| entry.identifier.as_str())
        .collect();
    assert_eq!(identifiers, vec!["unembedded-sweep"]);
    Ok(())
}

#[test]
fn a_sqlite_wiki_builder_schedules_only_the_sweeps_its_queue_claims() -> TestResult {
    let home = tempfile::tempdir()?;
    let database = home.path().join("memory.sqlite");
    migrate::sqlite(&database)?;
    let store = memory_sqlite::SqliteStore::open(&database)?;
    let entries =
        workers::schedule::claimed_entries(&store, &[kernel::queue::WorkerRole::WikiBuilder], true);
    let identifiers: Vec<&str> = entries
        .iter()
        .map(|entry| entry.identifier.as_str())
        .collect();
    assert_eq!(identifiers, vec!["wiki-backfill"]);
    Ok(())
}
