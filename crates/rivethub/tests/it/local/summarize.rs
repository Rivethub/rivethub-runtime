use std::path::Path;
use std::time::Duration;

use super::summary_index::{IndexedSession, assert_vector_hit, config, index_key, indexed_session};
use super::{REPLY, TestResult, model_stub, start, stop, write_state};

const SUMMARY_WAIT: Duration = Duration::from_secs(20);
const POLL: Duration = Duration::from_millis(100);

async fn wait_summary(
    path: &Path,
    conversation: &str,
    key: [u8; 32],
) -> TestResult<Option<IndexedSession>> {
    let mut ticks = tokio::time::interval(POLL);
    let waited = tokio::time::timeout(SUMMARY_WAIT, async {
        loop {
            ticks.tick().await;
            match indexed_session(path, conversation, key) {
                Ok(Some(content)) => return Ok(content),
                Ok(None) => {}
                Err(error) => return Err(error.to_string()),
            }
        }
    })
    .await;
    match waited {
        Ok(inner) => inner.map(Some).map_err(Into::into),
        Err(_elapsed) => Ok(None),
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
    let endpoint = format!("http://{}", stub.addr);
    let mut config = config(home, &database, &endpoint, port.number());
    if multipart {
        config = config.replace(
            "    type: openai-compatible",
            "    type: vllm\n    context_window: 32768\n    probe_models: false",
        );
    }
    write_state(home, &config)?;
    std::fs::create_dir_all(home.join(".rivethub"))?;
    migrate::sqlite(&database)?;
    seed_transcript(&database, multipart).await?;
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
        .env("PATH", "/usr/bin:/bin")
        .output()
        .await?;
    assert!(
        settled.status.success(),
        "{}",
        String::from_utf8_lossy(&settled.stderr)
    );
    let state: kernel::memory::settlement::SettleResult = serde_json::from_slice(&settled.stdout)?;
    let conversation = state.conversation_id.to_string();
    let summarized = wait_summary(&database, &conversation, index_key(&endpoint)).await;
    let searched = if let Ok(Some(summary)) = &summarized {
        assert_vector_hit(home, port.number(), summary).await
    } else {
        Ok(())
    };
    let logs = stop(started).await;
    let closed = stub.close().await;
    let logs = logs?;
    closed?;
    assert_eq!(
        summarized?.as_ref().map(|summary| summary.content.as_str()),
        Some(REPLY),
        "{logs}"
    );
    searched?;
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

async fn seed_transcript(database: &std::path::Path, multipart: bool) -> TestResult {
    let store = memory_sqlite::SqliteStore::open(database)?;
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
    Ok(())
}
