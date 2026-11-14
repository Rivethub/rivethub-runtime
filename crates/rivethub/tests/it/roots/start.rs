use crate::process::guard::SpawnGuarded;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use super::{TestResult, interrupt, provider_config, write_state};

#[tokio::test]
async fn start_stops_its_children_within_the_grace_on_sigint() -> TestResult {
    let db = crate::pgdb::TestDb::bare().await?;
    let pool = db.pool();
    migrate::postgres_pool(&pool).await?;
    let url = db.served_url().await?;
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    write_state(
        home.path(),
        &provider_config(
            home.path(),
            &url,
            &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
        ),
    )?;
    let mut child = Command::new(env!("CARGO_BIN_EXE_rh"))
        .arg("start")
        .env_clear()
        .env("HOME", home.path())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn_guarded()?;
    let stderr = child.stderr.take().ok_or("missing stderr")?;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let logs = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut text = String::new();
        let mut pending = vec!["gateway listening on "];
        let mut ready = Some(ready_tx);
        while let Some(line) = lines.next_line().await? {
            if let Some(index) = pending.iter().position(|needle| line.contains(needle)) {
                pending.swap_remove(index);
            }
            if pending.is_empty()
                && let Some(sender) = ready.take()
                && sender.send(()).is_err()
            {
                return Err(std::io::Error::other("start readiness listener dropped"));
            }
            text.push_str(&line);
            text.push('\n');
        }
        Ok(text)
    });
    if tokio::time::timeout(Duration::from_secs(30), ready_rx).await != Ok(Ok(())) {
        if let Some(pid) = child.id() {
            interrupt(pid)?;
        }
        let text = tokio::time::timeout(Duration::from_secs(5), logs).await???;
        return Err(format!("rh start did not become ready: {text}").into());
    }
    let pid = child.id().ok_or("rh start already stopped")?;
    let started = tokio::time::Instant::now();
    interrupt(pid)?;
    let waited = tokio::time::timeout(kernel::shutdown::SHUTDOWN_GRACE, child.wait()).await;
    let elapsed = started.elapsed();
    let Ok(status) = waited else {
        child.start_kill()?;
        let text = logs.await??;
        return Err(format!("rh start still running after {elapsed:?}\n{text}").into());
    };
    let status = status?;
    let text = tokio::time::timeout(Duration::from_secs(5), logs).await???;
    assert!(
        elapsed < kernel::shutdown::SHUTDOWN_GRACE,
        "{elapsed:?}\n{text}"
    );
    assert!(status.success(), "rh start exit: {status}\n{text}");
    assert!(text.contains("SIGTERM received — shutting down"), "{text}");
    Ok(())
}
