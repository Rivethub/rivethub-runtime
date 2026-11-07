use std::process::Stdio;
use std::time::Duration;

use sqlx::ConnectOptions as _;
use sqlx::PgPool;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;
use wire::gateway::agents::AgentListResponse;
use wire::gateway::agents::AgentStoreBackend;
use wire::gateway::devices::PhonePairingAvailability;

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;

use super::{TestResult, interrupt, provider_config, run_rh, stderr, terminate, write_state};

const GATEWAY_URL: &str = "http://127.0.0.1:9";

#[sqlx::test(migrations = false)]
async fn gateway_with_a_missing_certificate_authority_root_refuses_to_start(
    pool: PgPool,
) -> TestResult {
    migrate::postgres_pool(&pool).await?;
    let url = pool.connect_options().to_url_lossy();
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    let root = home.path().join("missing-root.pem");
    let password = home.path().join("provisioner.password");
    std::fs::write(&password, "secret\n")?;
    write_state(
        home.path(),
        &provider_config(
            home.path(),
            &url,
            &format!(
                "gateway:\n  host: 127.0.0.1\n  port: {port}\n  certificate_authority:\n    url: https://ca.example.test\n    root: {}\n    provisioner_password_file: {}\n",
                root.display(),
                password.display()
            ),
        ),
    )?;
    let output = run_rh(home.path(), &["gateway"]).await?;
    let text = stderr(&output);
    assert_eq!(output.status.code(), Some(1), "{text}");
    assert!(text.contains("certificate authority"), "{text}");
    Ok(())
}

struct Started {
    child: tokio::process::Child,
    logs: tokio::task::JoinHandle<std::io::Result<String>>,
}

async fn start_gateway(home: &std::path::Path) -> TestResult<Started> {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rh"))
        .arg("gateway")
        .env_clear()
        .env("HOME", home)
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let stderr = child.stderr.take().ok_or("missing stderr")?;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let logs = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut text = String::new();
        let mut ready = Some(ready_tx);
        while let Some(line) = lines.next_line().await? {
            if line.contains("gateway listening on ")
                && let Some(sender) = ready.take()
                && sender.send(()).is_err()
            {
                return Err(std::io::Error::other("gateway readiness listener dropped"));
            }
            text.push_str(&line);
            text.push('\n');
        }
        Ok(text)
    });
    if tokio::time::timeout(Duration::from_secs(30), ready_rx).await == Ok(Ok(())) {
        Ok(Started { child, logs })
    } else {
        if child.try_wait()?.is_none()
            && let Some(pid) = child.id()
        {
            terminate(pid)?;
        }
        let text = tokio::time::timeout(Duration::from_secs(30), logs).await???;
        Err(format!("gateway did not listen: {text}").into())
    }
}

#[sqlx::test(migrations = false)]
async fn phone_pairing_without_an_authority_stays_unavailable_and_agents_come_from_the_memory_database(
    pool: PgPool,
) -> TestResult {
    migrate::postgres_pool(&pool).await?;
    let url = pool.connect_options().to_url_lossy();
    let home = tempfile::tempdir()?;
    let port = testkit::ports::claim()?;
    write_state(
        home.path(),
        &provider_config(
            home.path(),
            &url,
            &format!(
                "gateway:\n  host: 127.0.0.1\n  port: {port}\n  devices:\n    gateway_url: {GATEWAY_URL}\n"
            ),
        ),
    )?;
    let Started { mut child, logs } = start_gateway(home.path()).await?;
    let client = reqwest::Client::builder().no_proxy().build()?;
    let base = format!("http://127.0.0.1:{port}");
    let pairing = client
        .get(format!("{base}/api/phone-pairing"))
        .send()
        .await?
        .error_for_status()?
        .json::<PhonePairingAvailability>()
        .await?;
    assert!(!pairing.available, "{pairing:?}");
    assert_eq!(
        pairing.reason.as_deref(),
        Some("device certificate authority is not usable")
    );
    assert_eq!(pairing.gateway, None);
    let agents = client
        .get(format!("{base}/api/agents"))
        .send()
        .await?
        .error_for_status()?
        .json::<AgentListResponse>()
        .await?;
    assert_eq!(agents.backend, Some(AgentStoreBackend::Postgres));
    let pid = child.id().ok_or("gateway already stopped")?;
    terminate(pid)?;
    let status = tokio::time::timeout(Duration::from_secs(30), child.wait()).await??;
    let text = tokio::time::timeout(Duration::from_secs(30), logs).await???;
    assert!(status.success(), "gateway exit: {status}\n{text}");
    assert!(text.contains("SIGTERM received — shutting down"), "{text}");
    assert!(text.contains("gateway stopped"), "{text}");
    Ok(())
}

#[sqlx::test(migrations = false)]
async fn gateway_with_a_partial_request_exits_within_the_grace_on_sigint(
    pool: PgPool,
) -> TestResult {
    migrate::postgres_pool(&pool).await?;
    let url = pool.connect_options().to_url_lossy();
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
    let Started { mut child, logs } = start_gateway(home.path()).await?;
    let mut socket = TcpStream::connect(("127.0.0.1", port.number())).await?;
    socket
        .write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n")
        .await?;
    let pid = child.id().ok_or("gateway already stopped")?;
    let started = tokio::time::Instant::now();
    interrupt(pid)?;
    let bound = kernel::shutdown::SHUTDOWN_GRACE + Duration::from_secs(2);
    let waited = tokio::time::timeout(bound, child.wait()).await;
    let elapsed = started.elapsed();
    let Ok(status) = waited else {
        child.start_kill()?;
        let text = logs.await??;
        return Err(format!("gateway still running after {elapsed:?}\n{text}").into());
    };
    let status = status?;
    let text = tokio::time::timeout(Duration::from_secs(5), logs).await???;
    assert!(elapsed < bound, "elapsed {elapsed:?}\n{text}");
    assert!(text.contains("SIGINT received — shutting down"), "{text}");
    if status.success() {
        assert!(elapsed < kernel::shutdown::SHUTDOWN_GRACE, "{text}");
        assert!(text.contains("gateway stopped"), "{text}");
    } else {
        assert!(
            text.contains("shutdown grace of 10s elapsed; still running:"),
            "{status}\n{text}"
        );
        assert!(text.contains("gateway connection"), "{text}");
    }
    drop(socket);
    Ok(())
}
