use jiff::Timestamp;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use wire::time::TimestampFormat as _;

use kernel::memory::summary_conversion::ConversionProgress;
use tokio::io::{AsyncBufReadExt as _, BufReader};

use super::{TestResult, model_stub, provider_at, start, stop, write_state};

async fn control<T: serde::de::DeserializeOwned>(home: &Path, args: &[&str]) -> TestResult<T> {
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

async fn first_watch_snapshot(home: &Path) -> TestResult<ConversionProgress> {
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
    let status: ConversionProgress = control(home.path(), &["status"]).await?;
    assert!(!status.enabled);
    assert!(!first_watch_snapshot(home.path()).await?.enabled);
    assert_empty_rehearsal(home.path(), &conversation).await?;

    let started = start(
        home.path(),
        &["local", "--no-lan"],
        &["gateway listening on ", "worker running summarizer"],
    )
    .await?;
    let result = exercise_running_conversion(home.path(), &database, &conversation).await;
    stop(started).await?;
    stub.close().await?;
    result
}

async fn assert_empty_rehearsal(home: &Path, conversation: &str) -> TestResult {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Rehearsal {
        sampled_conversations: usize,
    }
    let result: Rehearsal = control(home, &["rehearse", conversation]).await?;
    assert_eq!(result.sampled_conversations, 0);
    Ok(())
}

async fn exercise_running_conversion(
    home: &Path,
    database: &Path,
    conversation: &str,
) -> TestResult {
    let started: ConversionProgress = control(home, &["start", "--rate", "300000"]).await?;
    assert!(started.enabled);
    let paused: ConversionProgress = control(home, &["pause"]).await?;
    assert!(!paused.enabled);
    let resumed: ConversionProgress = control(home, &["resume"]).await?;
    assert_eq!(resumed.rate, 300_000);
    let progress = tokio::time::timeout(Duration::from_secs(20), async {
        let mut polls = tokio::time::interval(Duration::from_millis(200));
        loop {
            polls.tick().await;
            let progress: ConversionProgress = control(home, &["status"]).await?;
            if progress.complete == 1 || progress.failed != 0 {
                return Ok::<_, Box<dyn std::error::Error>>(progress);
            }
            polls.reset();
        }
    })
    .await??;
    assert_eq!(progress.complete, 1, "{progress:?}");
    assert!(progress.inventory_complete);
    let conn = rusqlite::Connection::open(database)?;
    let indexed: bool = conn.query_row(
        "SELECT (embedding IS NOT NULL AND NOT index_needs_repair) FROM ros_summaries
             WHERE conversation_id = ?1 AND kind = 'session'",
        [conversation],
        |row| row.get(0),
    )?;
    assert!(indexed);
    Ok(())
}
