use crate::process::guard::SpawnGuarded;
use std::collections::BTreeSet;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use serde::Deserialize;
use sqlx::ConnectOptions as _;
use sqlx::PgPool;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio_util::sync::CancellationToken;

use super::{TestResult, interrupt, provider_config, terminate, write_state};

#[derive(Deserialize)]
struct Catalog {
    tools: Vec<NamedTool>,
}

#[derive(Deserialize)]
struct NamedTool {
    name: String,
}

fn expected_tool_names() -> TestResult<BTreeSet<String>> {
    let catalog: Catalog =
        serde_json::from_slice(&std::fs::read(testkit::recording("mcp/tools-list.json"))?)?;
    let omitted: BTreeSet<&str> = super::OMITTED
        .iter()
        .map(|(name, _)| *name)
        .filter(|name| *name != "categorizer" && *name != "wiki-builder")
        .collect();
    let recorded: BTreeSet<String> = catalog.tools.into_iter().map(|tool| tool.name).collect();
    assert!(
        omitted.iter().all(|name| recorded.contains(*name)),
        "recording is missing an omitted tool"
    );
    Ok(recorded
        .into_iter()
        .filter(|name| !omitted.contains(name.as_str()))
        .chain(std::iter::once("memory_team".to_owned()))
        .chain(STDIO_ONLY.iter().map(|name| (*name).to_owned()))
        .collect())
}

const STDIO_ONLY: [&str; 3] = ["session_find", "session_sanitize", "session_import"];

fn served_names(tools: &[Arc<dyn kernel::tool::Tool>]) -> BTreeSet<String> {
    tools
        .iter()
        .map(|tool| tool.definition().name.into_owned())
        .collect()
}

#[tokio::test]
async fn mcp_lists_recorded_tools_over_stdio_and_stops_on_sigterm() -> TestResult {
    let db = crate::pgdb::TestDb::bare().await?;
    let pool = db.pool();
    stop_mcp(pool, false).await
}

#[tokio::test]
async fn mcp_with_a_stdio_client_exits_within_the_grace_on_sigint() -> TestResult {
    let db = crate::pgdb::TestDb::bare().await?;
    let pool = db.pool();
    stop_mcp(pool, true).await
}

async fn stop_mcp(pool: PgPool, interrupt_process: bool) -> TestResult {
    migrate::postgres_pool(&pool).await?;
    let url = pool.connect_options().to_url_lossy();
    let home = tempfile::tempdir()?;
    write_state(home.path(), &provider_config(home.path(), &url, ""))?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_rh"))
        .args(["mcp"])
        .env_clear()
        .env("HOME", home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn_guarded()?;
    let stderr = child.stderr.take().ok_or("missing stderr")?;
    let logs = tokio::spawn(async move {
        let mut stderr = stderr;
        let mut text = String::new();
        stderr.read_to_string(&mut text).await?;
        Ok::<_, std::io::Error>(text)
    });
    let stdout = child.stdout.take().ok_or("missing stdout")?;
    let stdin = child.stdin.take().ok_or("missing stdin")?;
    let client = tokio::time::timeout(
        Duration::from_secs(30),
        mcp::client::Client::connect((stdout, stdin), CancellationToken::new()),
    )
    .await?
    .map_err(|error| format!("mcp connect failed: {error}"))?;
    let tools = tokio::time::timeout(Duration::from_secs(30), client.tools())
        .await?
        .map_err(|error| format!("tools/list failed: {error}"))?;
    assert_eq!(served_names(&tools), expected_tool_names()?);
    let pid = child.id().ok_or("mcp already stopped")?;
    let started = tokio::time::Instant::now();
    if interrupt_process {
        interrupt(pid)?;
    } else {
        terminate(pid)?;
    }
    let waited = tokio::time::timeout(kernel::shutdown::SHUTDOWN_GRACE, child.wait()).await;
    let elapsed = started.elapsed();
    let Ok(status) = waited else {
        child.start_kill()?;
        let text = logs.await??;
        return Err(format!("mcp still running after {elapsed:?}\n{text}").into());
    };
    let status = status?;
    let text = tokio::time::timeout(Duration::from_secs(5), logs).await???;
    assert!(
        elapsed < kernel::shutdown::SHUTDOWN_GRACE,
        "{elapsed:?}\n{text}"
    );
    assert!(status.success(), "mcp exit: {status}\n{text}");
    let phrase = if interrupt_process {
        "SIGINT received — shutting down"
    } else {
        "SIGTERM received — shutting down"
    };
    assert!(text.contains(phrase), "{text}");
    assert!(text.contains("mcp stopped"), "{text}");
    drop(client);
    Ok(())
}
