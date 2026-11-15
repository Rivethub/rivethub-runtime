use crate::process::guard::SpawnGuarded;
use std::process::Stdio;
use std::time::Duration;

use kernel::memory::{MemoryEntry, MemoryStore as _};
use rmcp::ServiceExt as _;
use rmcp::model::{CallToolRequestParams, ClientConfig};
use rmcp::service::{RoleClient, RunningService};
use tokio::process::Command;
use wire::message::Role;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

const ANSWER: &str = "Marguerite logged the copper lantern inventory.";

fn entry(role: Role, content: &str) -> MemoryEntry {
    MemoryEntry {
        provenance: None,
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

async fn call(
    client: &RunningService<RoleClient, ClientConfig>,
    name: &str,
    arguments: &str,
) -> TestResult<String> {
    let mut request = CallToolRequestParams::new(name.to_owned());
    request.arguments = Some(serde_json::from_str(arguments)?);
    let result = client.call_tool(request).await?;
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
        .spawn_guarded()?;
    let stdout = child.stdout.take().ok_or("missing stdout")?;
    let stdin = child.stdin.take().ok_or("missing stdin")?;
    let client = tokio::time::timeout(
        Duration::from_secs(30),
        ClientConfig::default().serve((stdout, stdin)),
    )
    .await?
    .map_err(|error| format!("mcp connect failed: {error}"))?;
    let search = call(&client, "memory_search", r#"{"query":"copper lantern"}"#).await?;
    assert!(search.contains(ANSWER), "{search}");
    let browse = call(&client, "memory_browse", r#"{"limit":5}"#).await?;
    assert!(browse.contains(ANSWER), "{browse}");
    let full = call(&client, "memory_get_full", &format!(r#"{{"id":"{id}"}}"#)).await?;
    assert!(full.contains(ANSWER), "{full}");
    drop(client);
    child.kill().await?;
    Ok(())
}
