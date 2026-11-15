use std::time::Duration;

use tokio::process::Command;

use crate::process::{Started, start};

use super::{TestResult, interrupt, provider_config, write_state};

#[tokio::test]
async fn start_stops_its_children_within_the_grace_on_sigint() -> TestResult {
    let db = crate::pgdb::TestDb::bare().await?;
    migrate::postgres_pool(&db.pool()).await?;
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
    let mut command = Command::new(env!("CARGO_BIN_EXE_rh"));
    command.arg("start").env_clear().env("HOME", home.path());
    let Started { mut child, logs } = start(command, &["gateway listening on "]).await?;
    let started = tokio::time::Instant::now();
    interrupt(child.id().ok_or("rh start already stopped")?)?;
    let waited = tokio::time::timeout(kernel::shutdown::SHUTDOWN_GRACE, child.wait()).await;
    let elapsed = started.elapsed();
    let Ok(status) = waited else {
        child.start_kill()?;
        let text = logs.await??;
        return Err(format!("rh start still running after {elapsed:?}\n{text}").into());
    };
    let status = status?;
    let text = tokio::time::timeout(Duration::from_secs(5), logs).await???;
    assert!(status.success(), "rh start exit: {status}\n{text}");
    assert!(text.contains("SIGTERM received — shutting down"), "{text}");
    Ok(())
}
