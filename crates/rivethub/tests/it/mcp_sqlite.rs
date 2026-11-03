use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use kernel::memory::{MemoryEntry, MemoryStore as _};
use kernel::tool::Tool;
use tokio::process::Command;
use wire::message::Role;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const ANSWER: &str = "Marguerite logged the copper lantern inventory.";

fn entry(role: Role, content: &str) -> MemoryEntry {
    MemoryEntry {
        id: None,
        session_id: "claude-code:44444444-4444-4444-8444-444444444444".to_owned(),
        agent: "rivet-claude".to_owned(),
        channel: "claude-code".to_owned(),
        role,
        content: content.to_owned(),
        tool_name: None,
        tool_args: None,
        tool_result: None,
        metadata: None,
        created_at: None,
    }
}

async fn call(tools: &[Arc<dyn Tool>], name: &str, arguments: &str) -> TestResult<String> {
    let tool = tools
        .iter()
        .find(|tool| tool.definition().name.as_ref() == name)
        .ok_or_else(|| format!("missing tool {name}"))?;
    let context = kernel::ToolContext {
        agent_id: None,
        working_dir: None,
        session: None,
    };
    let result = tool
        .call(Some(serde_json::from_str(arguments)?), context)
        .await?;
    assert_ne!(result.is_error, Some(true), "{name}: {result:?}");
    Ok(result
        .content
        .iter()
        .filter_map(|content| content.as_text().map(|text| text.text.clone()))
        .collect())
}

#[tokio::test]
async fn stdio_mcp_reads_the_sqlite_store_of_a_local_node() -> TestResult {
    let home = tempfile::tempdir()?;
    let state = home.path().join(".rivethub");
    std::fs::create_dir_all(&state)?;
    let database = state.join("memory.sqlite");
    migrate::sqlite(&database)?;
    let store = memory_sqlite::SqliteStore::open(&database)?;
    store
        .append(
            &testkit::owner()?,
            vec![
                entry(Role::User, "Who logged the copper lantern inventory?"),
                entry(Role::Assistant, ANSWER),
            ],
        )
        .await?;
    drop(store);
    let id: String = rusqlite::Connection::open(&database)?.query_row(
        "SELECT id FROM ros_messages WHERE role = 'assistant'",
        [],
        |row| row.get(0),
    )?;
    std::fs::write(
        state.join("config.yaml"),
        format!(
            "memory:\n  backend: sqlite\n  sqlite:\n    path: {}\ngateway:\n  host: 127.0.0.1\n  port: 9\n",
            database.display()
        ),
    )?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_rh"))
        .arg("mcp")
        .env_clear()
        .env("HOME", home.path())
        .env("RIVETHUB_SHARED_DIR", home.path().join("shared"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let stdout = child.stdout.take().ok_or("missing stdout")?;
    let stdin = child.stdin.take().ok_or("missing stdin")?;
    let client = tokio::time::timeout(
        Duration::from_secs(30),
        mcp::client::Client::connect((stdout, stdin), tokio_util::sync::CancellationToken::new()),
    )
    .await?
    .map_err(|error| format!("mcp connect failed: {error}"))?;
    let tools = client.tools().await.map_err(|error| format!("{error}"))?;
    let search = call(&tools, "memory_search", r#"{"query":"copper lantern"}"#).await?;
    assert!(search.contains(ANSWER), "{search}");
    let browse = call(&tools, "memory_browse", r#"{"limit":5}"#).await?;
    assert!(browse.contains(ANSWER), "{browse}");
    let full = call(&tools, "memory_get_full", &format!(r#"{{"id":"{id}"}}"#)).await?;
    assert!(full.contains(ANSWER), "{full}");
    drop(client);
    child.kill().await?;
    Ok(())
}
