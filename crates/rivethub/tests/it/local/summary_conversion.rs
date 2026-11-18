use jiff::Timestamp;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use wire::time::TimestampFormat as _;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt as _, BufReader};

use super::{TestResult, model_stub, provider_at, start, stop, write_state};

async fn control(home: &Path, args: &[&str]) -> TestResult<Value> {
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_rh"))
        .args(["memory", "resummarize"])
        .args(args)
        .arg("--json")
        .env_clear()
        .env("HOME", home)
        .env("TOKIO_WORKER_THREADS", "2")
        .output()
        .await?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

async fn first_watch_snapshot(home: &Path) -> TestResult<Value> {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_rh"))
        .args(["memory", "resummarize", "status", "--watch", "--json"])
        .env_clear()
        .env("HOME", home)
        .env("TOKIO_WORKER_THREADS", "2")
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let mut output = BufReader::new(child.stdout.take().ok_or("watch output pipe")?).lines();
    let first = tokio::time::timeout(Duration::from_secs(5), output.next_line())
        .await??
        .ok_or("first JSON watch snapshot")?;
    child.kill().await?;
    Ok(serde_json::from_str(&first)?)
}

#[tokio::test]
async fn top_down_conversion_local_cli_and_worker() -> TestResult {
    let home = tempfile::tempdir()?;
    let database = home.path().join(".rivethub/memory.sqlite");
    let stub = model_stub().await?;
    let port = testkit::ports::claim()?;
    write_state(
        home.path(),
        &provider_at(
            home.path(),
            &format!("http://{}", stub.addr),
            &format!(
                "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\n    workers: true\ngateway:\n  host: 127.0.0.1\n  port: {port}\n",
                database.display()
            ),
        ),
    )?;
    migrate::sqlite(&database)?;
    let conversation = uuid::Uuid::new_v4().to_string();
    let message = uuid::Uuid::new_v4().to_string();
    let old = (Timestamp::now() - wire::time::elapsed_days(3)).rfc3339_offset();
    {
        let conn = rusqlite::Connection::open(&database)?;
        conn.execute(
            "INSERT INTO ros_conversations (id, session_key, agent, created_at, updated_at, owner_user_id)
             VALUES (?1, 'conversion-local-fixture', 'rivet', ?2, ?2, 'owner')",
            (&conversation, &old),
        )?;
        conn.execute(
            "INSERT INTO ros_messages (id, conversation_id, agent, channel, role, content, created_at, owner_user_id)
             VALUES (?1, ?2, 'rivet', 'test', 'user', 'Record the invented cobalt number.', ?3, 'owner')",
            (&message, &conversation, &old),
        )?;
    }
    assert_eq!(control(home.path(), &["status"]).await?["enabled"], false);
    assert_eq!(first_watch_snapshot(home.path()).await?["enabled"], false);
    assert_eq!(
        control(home.path(), &["rehearse", &conversation]).await?["sampledConversations"],
        0
    );

    let started = start(
        home.path(),
        &["local", "--no-lan"],
        &["gateway listening on ", "worker running summarizer"],
    )
    .await?;
    let result = async {
        assert_eq!(
            control(home.path(), &["start", "--rate", "300000"]).await?["enabled"],
            true
        );
        assert_eq!(control(home.path(), &["pause"]).await?["enabled"], false);
        assert_eq!(control(home.path(), &["resume"]).await?["rate"], 300_000);
        let progress = tokio::time::timeout(Duration::from_secs(20), async {
            loop {
                let progress = control(home.path(), &["status"]).await?;
                if progress["complete"] == 1 || progress["failed"] != 0 {
                    break Ok::<_, Box<dyn std::error::Error>>(progress);
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .await??;
        assert_eq!(progress["complete"], 1, "{progress}");
        assert_eq!(progress["inventoryComplete"], true);
        let conn = rusqlite::Connection::open(&database)?;
        let indexed: bool = conn.query_row(
            "SELECT (embedding IS NOT NULL AND NOT index_needs_repair) FROM ros_summaries
             WHERE conversation_id = ?1 AND kind = 'session'",
            [&conversation],
            |row| row.get(0),
        )?;
        assert!(indexed);
        Ok::<_, Box<dyn std::error::Error>>(())
    }
    .await;
    stop(started).await?;
    stub.close().await?;
    result
}
