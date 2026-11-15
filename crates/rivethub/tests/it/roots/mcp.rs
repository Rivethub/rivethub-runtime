use crate::process::guard::SpawnGuarded;
use std::collections::BTreeSet;
use std::process::Stdio;
use std::time::Duration;

use rmcp::ServiceExt as _;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::{TestResult, provider_config, terminate, write_state};

#[tokio::test]
async fn mcp_lists_its_tools_over_stdio_and_stops_on_sigterm() -> TestResult {
    let db = crate::pgdb::TestDb::bare().await?;
    let pool = db.pool();
    migrate::postgres_pool(&pool).await?;
    let url = db.served_url().await?;
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
        rmcp::model::ClientConfig::default().serve((stdout, stdin)),
    )
    .await?
    .map_err(|error| format!("mcp connect failed: {error}"))?;
    let tools = tokio::time::timeout(Duration::from_secs(30), client.list_all_tools())
        .await?
        .map_err(|error| format!("tools/list failed: {error}"))?;
    let names: BTreeSet<_> = tools
        .into_iter()
        .map(|tool| tool.name.into_owned())
        .collect();
    for name in [
        "memory_search",
        "memory_team",
        "session_find",
        "session_sanitize",
        "session_import",
    ] {
        assert!(names.contains(name), "{names:?}");
    }
    let pid = child.id().ok_or("mcp already stopped")?;
    let started = tokio::time::Instant::now();
    terminate(pid)?;
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
    assert!(text.contains("SIGTERM received — shutting down"), "{text}");
    assert!(text.contains("mcp stopped"), "{text}");
    drop(client);
    Ok(())
}
