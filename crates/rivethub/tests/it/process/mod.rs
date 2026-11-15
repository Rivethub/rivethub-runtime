pub type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub mod guard;
pub mod provider;

use std::net::SocketAddr;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use nix::sys::signal::Signal;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

use guard::{Guarded, SpawnGuarded};

pub const DEAD_POSTGRES: &str = "postgres://127.0.0.1:1/none";

pub fn provider_at(home: &Path, base: &str, extra: &str) -> String {
    format!(
        "runtime:\n  workspace: {home}\n  default_agent: main\nagents:\n  main:\n    provider: p\n    model: m\nproviders:\n  p:\n    type: openai-compatible\n    base_url: {base}\n    model: m\n{extra}",
        home = home.display()
    )
}

pub fn provider_config(home: &Path, postgres: impl std::fmt::Display, extra: &str) -> String {
    provider_at(
        home,
        "http://127.0.0.1:1",
        &format!("memory:\n  postgres:\n    connection_string: {postgres}\n{extra}"),
    )
}

pub fn write_state(home: &Path, body: &str) -> TestResult {
    let state = home.join(".rivethub");
    std::fs::create_dir_all(&state)?;
    std::fs::write(state.join("config.yaml"), body)?;
    Ok(())
}

pub fn signal(pid: u32, signal: Signal) -> TestResult {
    nix::sys::signal::kill(nix::unistd::Pid::from_raw(i32::try_from(pid)?), signal)?;
    Ok(())
}

pub fn terminate(pid: u32) -> TestResult {
    signal(pid, Signal::SIGTERM)
}

#[derive(Debug)]
pub struct Blackhole {
    pub address: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Blackhole {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn blackhole() -> TestResult<Blackhole> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let address = listener.local_addr()?;
    let task = tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = [0_u8; 1024];
                while matches!(stream.read(&mut buf).await, Ok(read) if read > 0) {}
            });
        }
    });
    Ok(Blackhole { address, task })
}

#[derive(Debug)]
pub struct Started {
    pub child: Guarded,
    pub logs: tokio::task::JoinHandle<std::io::Result<String>>,
}

pub async fn start(mut command: Command, needles: &'static [&str]) -> TestResult<Started> {
    let mut child = command
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn_guarded()?;
    let stderr = child.stderr.take().ok_or("missing stderr")?;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let logs = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        let mut text = String::new();
        let mut pending: Vec<&str> = needles.to_vec();
        let mut ready = Some(ready_tx);
        while let Some(line) = lines.next_line().await? {
            if let Some(index) = pending.iter().position(|needle| line.contains(needle)) {
                pending.swap_remove(index);
            }
            if pending.is_empty()
                && let Some(sender) = ready.take()
                && sender.send(()).is_err()
            {
                return Err(std::io::Error::other("readiness listener dropped"));
            }
            text.push_str(&line);
            text.push('\n');
        }
        Ok(text)
    });
    if tokio::time::timeout(Duration::from_secs(30), ready_rx).await == Ok(Ok(())) {
        return Ok(Started { child, logs });
    }
    if child.try_wait()?.is_none()
        && let Some(pid) = child.id()
    {
        terminate(pid)?;
    }
    let text = tokio::time::timeout(Duration::from_secs(30), logs).await???;
    Err(format!("{needles:?} did not appear: {text}").into())
}

pub async fn stop(started: Started) -> TestResult<String> {
    let Started { mut child, logs } = started;
    terminate(child.id().ok_or("process already stopped")?)?;
    let status = tokio::time::timeout(Duration::from_secs(30), child.wait()).await??;
    let text = tokio::time::timeout(Duration::from_secs(30), logs).await???;
    if status.success() {
        Ok(text)
    } else {
        Err(format!("exit {status}\n{text}").into())
    }
}
