use std::path::Path;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::process::Command;

use crate::process::{Started, start, stop};

use super::{TestResult, interrupt, provider_config, write_state};

async fn start_gateway(home: &Path) -> TestResult<Started> {
    let mut command = Command::new(env!("CARGO_BIN_EXE_rh"));
    command.arg("gateway").env_clear().env("HOME", home);
    start(command, &["gateway listening on "]).await
}

async fn configure(home: &Path, gateway: &str) -> TestResult<crate::pgdb::TestDb> {
    let db = crate::pgdb::TestDb::bare().await?;
    migrate::postgres_pool(&db.pool()).await?;
    let url = db.served_url().await?;
    write_state(home, &provider_config(home, &url, gateway))?;
    Ok(db)
}

#[tokio::test]
async fn a_retired_certificate_authority_section_only_warns_and_the_gateway_serves() -> TestResult {
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    let _db = configure(
        home.path(),
        &format!(
            "gateway:\n  host: 127.0.0.1\n  port: {port}\n  certificate_authority:\n    url: https://ca.example.test\n    root: /missing/root.pem\n"
        ),
    )
    .await?;
    let text = stop(start_gateway(home.path()).await?).await?;
    assert!(
        text.contains("\"gateway.certificate_authority\" is no longer used"),
        "{text}"
    );
    Ok(())
}

#[tokio::test]
async fn gateway_with_a_partial_request_exits_within_the_grace_on_sigint() -> TestResult {
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    let _db = configure(
        home.path(),
        &format!("gateway:\n  host: 127.0.0.1\n  port: {port}\n"),
    )
    .await?;
    let Started { mut child, logs } = start_gateway(home.path()).await?;
    let mut socket = TcpStream::connect(("127.0.0.1", port.number())).await?;
    socket
        .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n")
        .await?;
    let started = tokio::time::Instant::now();
    interrupt(child.id().ok_or("gateway already stopped")?)?;
    let waited = tokio::time::timeout(kernel::shutdown::SHUTDOWN_GRACE, child.wait()).await;
    let elapsed = started.elapsed();
    let Ok(status) = waited else {
        child.start_kill()?;
        let text = logs.await??;
        return Err(format!("gateway still running after {elapsed:?}\n{text}").into());
    };
    let status = status?;
    let text = tokio::time::timeout(Duration::from_secs(5), logs).await???;
    assert!(status.success(), "{status}\n{text}");
    assert!(text.contains("SIGINT received — shutting down"), "{text}");
    assert!(text.contains("gateway stopped"), "{text}");
    drop(socket);
    Ok(())
}
