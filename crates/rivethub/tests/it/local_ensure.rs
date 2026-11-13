use std::path::{Path, PathBuf};
use std::process::Output;
use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use wire::gateway::HealthzResponse;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
enum Summary {
    Started(Running),
    AlreadyRunning(Running),
    Failed(Failure),
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Running {
    origin: String,
    port: u16,
    pid: Option<i32>,
    node: String,
    version: Option<String>,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Failure {
    reason: Reason,
    error: String,
    origin: Option<String>,
    port: Option<u16>,
}

#[derive(Debug, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Reason {
    PortInUse,
    NotReady,
    Error,
}

struct Node {
    home: tempfile::TempDir,
}

impl Node {
    fn new() -> TestResult<Self> {
        Ok(Self {
            home: tempfile::tempdir()?,
        })
    }

    fn path(&self) -> &Path {
        self.home.path()
    }

    fn pid_file(&self) -> PathBuf {
        self.path().join(".rivethub/rivethub.pid")
    }

    fn log_file(&self) -> PathBuf {
        self.path().join(".rivethub/logs/local.log")
    }

    async fn rh(&self, args: &[&str]) -> TestResult<Output> {
        let output = Command::new(env!("CARGO_BIN_EXE_rh"))
            .args(args)
            .env_clear()
            .env("HOME", self.path())
            .env("TOKIO_WORKER_THREADS", "2")
            .current_dir(self.path())
            .kill_on_drop(true)
            .output();
        Ok(tokio::time::timeout(Duration::from_secs(60), output).await??)
    }

    async fn ensure(&self, port: u16, wait: &str) -> TestResult<Output> {
        let port = port.to_string();
        self.rh(&[
            "local", "--ensure", "--port", &port, "--no-lan", "--wait", wait,
        ])
        .await
    }

    async fn ensure_json(&self, port: u16, wait: &str) -> TestResult<(Output, Summary)> {
        let port = port.to_string();
        let output = self
            .rh(&[
                "local", "--ensure", "--port", &port, "--no-lan", "--wait", wait, "--json",
            ])
            .await?;
        let line = text(&output.stdout)?;
        assert_eq!(line.lines().count(), 1, "{line}");
        let value = serde_json::from_str(&line)?;
        Ok((output, value))
    }

    fn pid(&self) -> TestResult<i32> {
        Ok(std::fs::read_to_string(self.pid_file())?.trim().parse()?)
    }

    async fn stop(&self) -> TestResult {
        let stopped = self.rh(&["stop"]).await?;
        assert!(stopped.status.success(), "{stopped:?}");
        let mut ticks = tokio::time::interval(Duration::from_millis(50));
        let gone = async {
            while self.pid_file().exists() {
                ticks.tick().await;
            }
        };
        tokio::time::timeout(Duration::from_secs(30), gone).await?;
        Ok(())
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        if let Ok(pid) = self.pid() {
            match nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGTERM,
            ) {
                Ok(()) | Err(_) => {}
            }
        }
    }
}

fn text(bytes: &[u8]) -> TestResult<String> {
    Ok(String::from_utf8(bytes.to_vec())?)
}

async fn healthz(port: u16) -> TestResult<HealthzResponse> {
    Ok(reqwest::Client::builder()
        .no_proxy()
        .build()?
        .get(format!("http://127.0.0.1:{port}/healthz"))
        .send()
        .await?
        .json()
        .await?)
}

#[tokio::test]
async fn ensure_starts_one_detached_node_and_reuses_it() -> TestResult {
    let node = Node::new()?;
    let claim = testkit::ports::claim()?;
    let port = claim.number();
    let first = node.ensure(port, "30").await?;
    assert!(first.status.success(), "{first:?}");
    assert_eq!(
        text(&first.stdout)?,
        format!("Started RivetHub local node at http://127.0.0.1:{port}\n")
    );
    assert!(healthz(port).await?.ok);
    let pid = node.pid()?;
    let second = node.ensure(port, "30").await?;
    assert!(second.status.success(), "{second:?}");
    assert_eq!(
        text(&second.stdout)?,
        format!("RivetHub local node is running at http://127.0.0.1:{port}\n")
    );
    assert_eq!(node.pid()?, pid);
    assert!(std::fs::read_to_string(node.log_file())?.contains("127.0.0.1"));
    node.stop().await
}

#[tokio::test]
async fn concurrent_ensures_start_a_single_node() -> TestResult {
    let node = Node::new()?;
    let claim = testkit::ports::claim()?;
    let port = claim.number();
    let (left, right) = tokio::join!(node.ensure(port, "30"), node.ensure(port, "30"));
    let outputs = [left?, right?];
    let mut started = 0;
    for output in &outputs {
        assert!(output.status.success(), "{output:?}");
        if text(&output.stdout)?.starts_with("Started ") {
            started += 1;
        }
    }
    assert_eq!(started, 1, "{outputs:?}");
    assert!(healthz(port).await?.ok);
    node.stop().await
}

#[tokio::test]
async fn ensure_json_reports_started_then_already_running() -> TestResult {
    let node = Node::new()?;
    let claim = testkit::ports::claim()?;
    let port = claim.number();
    let (first, started) = node.ensure_json(port, "30").await?;
    assert!(first.status.success(), "{first:?}");
    let health = healthz(port).await?;
    let pid = node.pid()?;
    let expected = Running {
        origin: format!("http://127.0.0.1:{port}"),
        port,
        pid: Some(pid),
        node: health.node.clone(),
        version: health.version.clone(),
    };
    assert_eq!(started, Summary::Started(expected.clone()));
    assert!(health.version.is_some());
    let (second, running) = node.ensure_json(port, "30").await?;
    assert!(second.status.success(), "{second:?}");
    assert_eq!(running, Summary::AlreadyRunning(expected));
    node.stop().await
}

async fn foreign_server() -> TestResult<(u16, tokio::task::JoinHandle<std::io::Result<()>>)> {
    answering(b"HTTP/1.0 200 OK\r\ncontent-type: text/html\r\ncontent-length: 13\r\nconnection: close\r\n\r\n<html></html>").await
}

fn reply(status: &str, body: &str) -> &'static [u8] {
    format!(
        "HTTP/1.0 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )
    .leak()
    .as_bytes()
}

async fn answering(
    response: &'static [u8],
) -> TestResult<(u16, tokio::task::JoinHandle<std::io::Result<()>>)> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = listener.local_addr()?.port();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await?;
            let mut request = [0_u8; 1024];
            let read = stream.read(&mut request).await?;
            assert!(read > 0);
            stream.write_all(response).await?;
        }
    });
    Ok((port, server))
}

#[tokio::test]
async fn ensure_refuses_a_port_held_by_something_that_is_not_rivethub() -> TestResult {
    let node = Node::new()?;
    let (port, server) = foreign_server().await?;
    let message = format!(
        "port {port} is in use by something that is not a RivetHub node (http://127.0.0.1:{port}/healthz answered 200 OK without a RivetHub health body); stop it or use --port"
    );
    let output = node.ensure(port, "2").await?;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(text(&output.stdout)?, "");
    assert_eq!(text(&output.stderr)?, format!("Error: {message}\n"));
    let (json, failed) = node.ensure_json(port, "30").await?;
    assert_eq!(json.status.code(), Some(1), "{json:?}");
    assert_eq!(
        failed,
        Summary::Failed(Failure {
            reason: Reason::PortInUse,
            error: message,
            origin: Some(format!("http://127.0.0.1:{port}")),
            port: Some(port),
        })
    );
    assert!(!node.pid_file().exists());
    assert!(!node.log_file().exists());
    server.abort();
    Ok(())
}

async fn json_outcome(response: &'static [u8]) -> TestResult<(Output, Summary, u16)> {
    let node = Node::new()?;
    let (port, server) = answering(response).await?;
    let (output, summary) = node.ensure_json(port, "2").await?;
    server.abort();
    assert!(!node.pid_file().exists());
    Ok((output, summary, port))
}

#[tokio::test]
async fn ensure_refuses_a_redirect_even_with_a_health_body() -> TestResult {
    let body = r#"{"ok":true,"sessions":0,"name":"desk","node":"desk"}"#;
    let (output, summary, port) = json_outcome(reply(
        "302 Found\r\nlocation: http://127.0.0.1:9/healthz",
        body,
    ))
    .await?;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let Summary::Failed(failure) = summary else {
        return Err(format!("expected a failure, got {summary:?}").into());
    };
    assert_eq!(failure.reason, Reason::PortInUse);
    assert_eq!(failure.port, Some(port));
    Ok(())
}

#[tokio::test]
async fn ensure_refuses_a_health_body_that_names_no_node() -> TestResult {
    let body = r#"{"ok":true,"sessions":0,"name":"","node":""}"#;
    let (output, summary, _) = json_outcome(reply("200 OK", body)).await?;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(
        matches!(
            summary,
            Summary::Failed(Failure {
                reason: Reason::PortInUse,
                ..
            })
        ),
        "{summary:?}"
    );
    Ok(())
}

#[tokio::test]
async fn ensure_adopts_a_newer_degraded_node_it_cannot_fully_read() -> TestResult {
    let body = r#"{"ok":false,"sessions":3,"name":"desk","node":"future","version":"9.9.9","degraded":"embedder down","degradedProviders":[{"recovery":"rebooting","provider":"x"}],"hubPolicy":{"cloud":"brand-new-value"}}"#;
    let (output, summary, port) = json_outcome(reply("503 Service Unavailable", body)).await?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        summary,
        Summary::AlreadyRunning(Running {
            origin: format!("http://127.0.0.1:{port}"),
            port,
            pid: None,
            node: "future".to_owned(),
            version: Some("9.9.9".to_owned()),
        })
    );
    Ok(())
}

#[tokio::test]
async fn ensure_reports_a_running_node_when_its_pid_file_is_unreadable() -> TestResult {
    let node = Node::new()?;
    let claim = testkit::ports::claim()?;
    let port = claim.number();
    let (first, _) = node.ensure_json(port, "30").await?;
    assert!(first.status.success(), "{first:?}");
    let pid = std::fs::read(node.pid_file())?;
    std::fs::write(node.pid_file(), b"garbage")?;
    let (second, summary) = node.ensure_json(port, "30").await?;
    std::fs::write(node.pid_file(), pid)?;
    assert!(second.status.success(), "{second:?}");
    assert!(
        matches!(summary, Summary::AlreadyRunning(Running { pid: None, .. })),
        "{summary:?}"
    );
    node.stop().await
}

#[tokio::test]
async fn ensure_fails_with_the_log_path_when_the_node_cannot_serve() -> TestResult {
    let node = Node::new()?;
    let silent = std::net::TcpListener::bind(("127.0.0.1", 0))?;
    let port = silent.local_addr()?.port();
    let output = node.ensure(port, "2").await?;
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = text(&output.stderr)?;
    assert!(
        stderr.contains(&format!(
            "did not answer at http://127.0.0.1:{port} within 2s; its output is in {}",
            node.log_file().display()
        )),
        "{stderr}"
    );
    assert!(node.log_file().is_file());
    let (json, failed) = node.ensure_json(port, "1").await?;
    assert_eq!(json.status.code(), Some(1), "{json:?}");
    assert_eq!(
        failed,
        Summary::Failed(Failure {
            reason: Reason::NotReady,
            error: format!(
                "RivetHub local node did not answer at http://127.0.0.1:{port} within 1s; its output is in {}",
                node.log_file().display()
            ),
            origin: Some(format!("http://127.0.0.1:{port}")),
            port: Some(port),
        })
    );
    drop(silent);
    Ok(())
}

#[tokio::test]
async fn ensure_adopts_legacy_state_before_creating_its_own() -> TestResult {
    let node = Node::new()?;
    std::fs::create_dir(node.path().join(".rivetos"))?;
    let claim = testkit::ports::claim()?;
    let port = claim.number();
    let output = node.ensure(port, "30").await?;
    assert!(output.status.success(), "{output:?}");
    assert!(
        text(&output.stderr)?.contains("moved legacy state"),
        "{output:?}"
    );
    assert!(!node.path().join(".rivetos").exists());
    assert!(healthz(port).await?.ok);
    node.stop().await
}
