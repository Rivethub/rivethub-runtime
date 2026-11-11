use crate::process::guard::SpawnGuarded;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use sqlx::ConnectOptions as _;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

const CONFIGURED: &str = "rr-configured-node";
const FROM_ENV: &str = "from-env";
const FROM_HOSTNAME_ENV: &str = "from-hostname-env";
const DEAD_POSTGRES: &str = "postgres://127.0.0.1:1/none";

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Started {
    child: crate::process::guard::Guarded,
    logs: tokio::task::JoinHandle<std::io::Result<String>>,
}

fn provider_config(home: &Path, postgres: impl std::fmt::Display, extra: &str) -> String {
    format!(
        "runtime:\n  workspace: {}\n  default_agent: main\nagents:\n  main:\n    provider: p\n    model: m\nproviders:\n  p:\n    type: openai-compatible\n    base_url: http://127.0.0.1:1\n    model: m\nmemory:\n  postgres:\n    connection_string: {postgres}\n{extra}",
        home.display()
    )
}

fn write_state(home: &Path, body: &str) -> TestResult {
    let state = home.join(".rivethub");
    std::fs::create_dir_all(&state)?;
    std::fs::write(state.join("config.yaml"), body)?;
    Ok(())
}

fn node_line(root: &str) -> String {
    format!("service node {root} {CONFIGURED} hostname ")
}

fn logged_host(text: &str, root: &str) -> Result<String, String> {
    let marker = node_line(root);
    let Some(rest) = text.split(&marker).nth(1) else {
        return Err(format!("missing node line: {text}"));
    };
    let host = rest.lines().next().unwrap_or(rest).trim();
    if host.is_empty() || host == CONFIGURED || host == FROM_ENV || host == FROM_HOSTNAME_ENV {
        return Err(format!("unexpected hostname {host:?} in {text}"));
    }
    Ok(host.to_owned())
}

fn command(home: &Path, args: &[&str]) -> Command {
    let mut child = Command::new(env!("CARGO_BIN_EXE_rh"));
    child
        .args(args)
        .env_clear()
        .env("HOME", home)
        .env("RIVETHUB_NODE_NAME", FROM_ENV)
        .env("RIVETHUB_NODE_ID", "from-id")
        .env("HOSTNAME", FROM_HOSTNAME_ENV);
    child
}

async fn run_checked(home: &Path, args: &[&str]) -> TestResult<std::process::Output> {
    Ok(tokio::time::timeout(Duration::from_secs(30), command(home, args).output()).await??)
}

fn assert_checked(output: &std::process::Output, root: &str, ready: &str) -> TestResult {
    let text = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{text}");
    assert!(text.contains(ready), "{text}");
    logged_host(&text, root)?;
    Ok(())
}

async fn start_logged(home: &Path, args: &[&str], ready: &str) -> TestResult<Started> {
    let mut child = command(home, args);
    child.stderr(Stdio::piped()).kill_on_drop(true);
    let mut child = child.spawn_guarded()?;
    let stderr = child.stderr.take().ok_or("missing stderr")?;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let needle = ready.to_owned();
    let logs = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut text = String::new();
        let mut sender = Some(ready_tx);
        while let Some(line) = lines.next_line().await? {
            if line.contains(&needle)
                && let Some(sender) = sender.take()
                && sender.send(()).is_err()
            {
                return Err(std::io::Error::other("readiness listener dropped"));
            }
            text.push_str(&line);
            text.push('\n');
        }
        Ok(text)
    });
    if tokio::time::timeout(Duration::from_secs(30), ready_rx)
        .await
        .is_ok_and(|result| result.is_ok())
    {
        return Ok(Started { child, logs });
    }
    stop_early(&mut child)?;
    let text = tokio::time::timeout(Duration::from_secs(30), logs).await???;
    Err(format!("{ready} did not appear: {text}").into())
}

fn stop_early(child: &mut tokio::process::Child) -> TestResult {
    if child.try_wait()?.is_none()
        && let Some(pid) = child.id()
    {
        terminate(pid)?;
    }
    Ok(())
}

fn terminate(pid: u32) -> TestResult {
    let pid = i32::try_from(pid)?;
    nix::sys::signal::kill(
        nix::unistd::Pid::from_raw(pid),
        nix::sys::signal::Signal::SIGTERM,
    )?;
    Ok(())
}

async fn finish(started: Started) -> TestResult<String> {
    let Started { mut child, logs } = started;
    let pid = child.id().ok_or("process already stopped")?;
    terminate(pid)?;
    let status = tokio::time::timeout(Duration::from_secs(30), child.wait()).await??;
    let text = tokio::time::timeout(Duration::from_secs(30), logs).await???;
    assert!(status.success(), "exit: {status}\n{text}");
    Ok(text)
}

#[tokio::test]
async fn worker_check_uses_the_configured_node_name() -> TestResult {
    let home = tempfile::tempdir()?;
    write_state(
        home.path(),
        &provider_config(
            home.path(),
            DEAD_POSTGRES,
            "mesh:\n  node_name: rr-configured-node\n",
        ),
    )?;
    let output = run_checked(home.path(), &["worker", "--role", "summarizer", "--check"]).await?;
    assert_checked(&output, "worker", "worker configuration valid")
}

#[tokio::test]
async fn mcp_check_uses_the_configured_node_name() -> TestResult {
    let home = tempfile::tempdir()?;
    write_state(
        home.path(),
        &provider_config(
            home.path(),
            DEAD_POSTGRES,
            "mesh:\n  node_name: rr-configured-node\n",
        ),
    )?;
    let output = run_checked(home.path(), &["mcp", "--check"]).await?;
    assert_checked(&output, "mcp", "mcp configuration valid")
}

#[tokio::test]
async fn gateway_healthz_reports_the_configured_node() -> TestResult {
    let db = crate::pgdb::TestDb::bare().await?;
    let pool = db.pool();
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
                "gateway:\n  host: 127.0.0.1\n  port: {port}\nmesh:\n  node_name: \" {CONFIGURED} \"\n"
            ),
        ),
    )?;
    let started = start_logged(home.path(), &["gateway"], "gateway listening on ").await?;
    let health: wire::gateway::HealthzResponse = reqwest::Client::builder()
        .no_proxy()
        .build()?
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    assert!(health.ok);
    assert_eq!(health.node, CONFIGURED);
    assert_ne!(health.name, health.node);
    assert_ne!(health.name, FROM_ENV);
    assert_ne!(health.name, FROM_HOSTNAME_ENV);
    let text = finish(started).await?;
    assert!(
        text.contains(&format!("{}{}", node_line("gateway"), health.name)),
        "{text}"
    );
    Ok(())
}
