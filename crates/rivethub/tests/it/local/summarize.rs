use std::path::Path;
use std::time::Duration;

use rusqlite::OptionalExtension as _;

use super::{REPLY, TestResult, model_stub, provider_at, start, stop, write_state};

const SUMMARY_WAIT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(100);

fn summary(path: &Path, conversation: &str) -> TestResult<Option<String>> {
    let conn = rusqlite::Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(conn
        .query_row(
            "SELECT s.content FROM ros_summaries s
               JOIN ros_summary_sources src ON src.summary_id = s.id
               JOIN ros_messages m ON m.id = src.message_id
              WHERE m.conversation_id = ?1 AND s.kind = 'session'
                AND s.index_needs_repair = 0 AND s.embedding IS NOT NULL
              LIMIT 1",
            [conversation],
            |row| row.get(0),
        )
        .optional()?)
}

async fn wait_summary(path: &Path, conversation: &str) -> TestResult<Option<String>> {
    let mut ticks = tokio::time::interval(POLL);
    let waited = tokio::time::timeout(SUMMARY_WAIT, async {
        loop {
            ticks.tick().await;
            match summary(path, conversation) {
                Ok(Some(content)) => return Ok(content),
                Ok(None) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
    })
    .await;
    match waited {
        Ok(inner) => inner.map(Some).map_err(Into::into),
        Err(_) => Ok(None),
    }
}

#[tokio::test]
async fn top_down_live_capture_settle_and_publish_on_sqlite() -> TestResult {
    local_publish(false).await
}

#[tokio::test]
async fn top_down_live_multipart_and_exact_tokenizer_on_sqlite() -> TestResult {
    local_publish(true).await
}

async fn local_publish(multipart: bool) -> TestResult {
    let dir = tempfile::tempdir()?;
    let home = dir.path();
    let database = home.join(".rivethub").join("memory.sqlite");
    let stub = model_stub().await?;
    let port = testkit::ports::claim()?;
    let mut config = provider_at(
        home,
        &format!("http://{}", stub.addr),
        &format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n    workers: true\ngateway:\n  host: 127.0.0.1\n  port: {port}\n",
            database.display()
        ),
    );
    if multipart {
        config = config.replace(
            "    type: openai-compatible",
            "    type: vllm\n    context_window: 32768\n    probe_models: false",
        );
    }
    write_state(home, &config)?;
    std::fs::create_dir_all(home.join(".rivethub"))?;
    migrate::sqlite(&database)?;
    let store = memory_sqlite::SqliteStore::open(&database)?;
    let now = jiff::Timestamp::now();
    let messages = (0..if multipart { 6 } else { 1 })
        .map(|position| kernel::memory::MemoryEntry {
            id: None,
            session_id: "live-ledger".into(),
            agent: "rivet".into(),
            channel: "test".into(),
            role: wire::message::Role::User,
            content: if multipart {
                format!("Deposit {position}: {}", "hazelnut ".repeat(1750))
            } else {
                "Please reconcile the blue ledger.".into()
            },
            tool_name: None,
            tool_args: None,
            tool_result: None,
            metadata: None,
            created_at: Some(now + jiff::SignedDuration::from_millis(position)),
            provenance: None,
        })
        .collect();
    store
        .append_entries(messages, &testkit::owner()?, now)
        .await?;
    drop(store);
    let started = start(
        home,
        &["local", "--no-lan"],
        &["gateway listening on ", "worker running summarizer"],
    )
    .await?;
    let settled = tokio::process::Command::new(env!("CARGO_BIN_EXE_rh"))
        .args(["memory", "settle", "live-ledger", "--json"])
        .env_clear()
        .env("HOME", home)
        .output()
        .await?;
    assert!(
        settled.status.success(),
        "{}",
        String::from_utf8_lossy(&settled.stderr)
    );
    let state: serde_json::Value = serde_json::from_slice(&settled.stdout)?;
    let conversation = state["conversationId"]
        .as_str()
        .ok_or("missing conversation ID")?;
    let summarized = wait_summary(&database, conversation).await;
    let logs = stop(started).await;
    let closed = stub.close().await;
    let logs = logs?;
    closed?;
    assert_eq!(summarized?.as_deref(), Some(REPLY), "{logs}");
    let conn = rusqlite::Connection::open(&database)?;
    let part_count: i64 = conn.query_row(
        "SELECT count(*) FROM ros_summaries WHERE conversation_id = ?1 AND kind = 'part'",
        [conversation],
        |row| row.get(0),
    )?;
    if multipart {
        assert!(part_count >= 3);
    } else {
        assert_eq!(part_count, 0);
    }
    Ok(())
}
