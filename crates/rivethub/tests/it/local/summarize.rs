use jiff::SignedDuration;
use jiff::Timestamp;
use std::path::Path;
use std::time::Duration;
use wire::time::TimestampFormat as _;

use rusqlite::OptionalExtension as _;

use super::{REPLY, TestResult, model_stub, provider_at, start, stop, write_state};

const CONVERSATION: &str = "0199e2a0-7b3c-7a00-8000-0000000000e1";
const MESSAGES: u32 = 20;
const SUMMARY_WAIT: Duration = Duration::from_mins(1);
const POLL: Duration = Duration::from_millis(100);

fn stamp(at: Timestamp) -> String {
    at.rfc3339_millis()
}

fn seed_idle_conversation(path: &Path) -> TestResult {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    migrate::sqlite(path)?;
    let conn = rusqlite::Connection::open(path)?;
    let started = Timestamp::now() - SignedDuration::from_hours(2);
    conn.execute(
        "INSERT INTO ros_conversations (id, session_key, agent, created_at, updated_at)
         VALUES (?1, 'idle-session', 'rivet', ?2, ?2)",
        (CONVERSATION, stamp(started)),
    )?;
    for index in 0..MESSAGES {
        let role = if index % 2 == 0 { "user" } else { "assistant" };
        conn.execute(
            "INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, created_at)
             VALUES (?1, ?2, 'rivet', 'test', ?3, ?4, ?5)",
            (
                format!("0199e2a0-7b3c-7a00-8001-{index:012}"),
                CONVERSATION,
                role,
                format!("ledger step {index}"),
                stamp(started + SignedDuration::from_secs(i64::from(index))),
            ),
        )?;
    }
    Ok(())
}

fn summary(path: &Path, conversation: &str) -> TestResult<Option<String>> {
    let conn = rusqlite::Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    Ok(conn
        .query_row(
            "SELECT s.content FROM ros_summaries s
               JOIN ros_summary_sources src ON src.summary_id = s.id
               JOIN ros_messages m ON m.id = src.message_id
              WHERE m.conversation_id = ?1
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
async fn an_idle_sqlite_conversation_is_summarized_within_one_idle_tick() -> TestResult {
    let dir = tempfile::tempdir()?;
    let home = dir.path();
    let database = home.join(".rivethub").join("memory.sqlite");
    seed_idle_conversation(&database)?;
    let stub = model_stub().await?;
    let port = testkit::ports::claim()?;
    write_state(
        home,
        &provider_at(
            home,
            &format!("http://{}", stub.addr),
            &format!(
                "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n    workers: true\ngateway:\n  host: 127.0.0.1\n  port: {port}\n",
                database.display()
            ),
        ),
    )?;
    let started = start(
        home,
        &["local", "--no-lan"],
        &["gateway listening on ", "worker running summarizer"],
    )
    .await?;
    let summarized = wait_summary(&database, CONVERSATION).await;
    let logs = stop(started).await;
    let closed = stub.close().await;
    let logs = logs?;
    closed?;
    assert_eq!(summarized?.as_deref(), Some(REPLY), "{logs}");
    Ok(())
}
