use std::fs::{DirBuilder, File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use config::model::GatewaySection;
use config::{Config, Environment, Paths, Profile};
use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use nix::unistd::Pid;
use serde::Serialize;
use tokio::time::Instant;
use wire::gateway::HealthzResponse;

use super::{apply_bind, bind_host, bind_port};
use crate::exit::Exit;
use crate::status::{Endpoint, endpoint};
use crate::{Error, Report, process};

const LOCK_DIRECTORY: &str = "run";
const LOCK_FILE: &str = "local-start.lock";
const LOG_FILE: &str = "local.log";
const PROBE_TIMEOUT: Duration = Duration::from_millis(500);
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy)]
pub(crate) struct Ensure {
    pub(crate) port: Option<u16>,
    pub(crate) no_lan: bool,
    pub(crate) wait_seconds: u64,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum Outcome {
    AlreadyRunning,
    Started,
}

pub(crate) struct Ready {
    outcome: Outcome,
    origin: String,
    port: u16,
    pid: Option<i32>,
    health: HealthzResponse,
    adopted: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Refusal {
    #[error(
        "port {port} is in use by something that is not a RivetHub node ({origin}/healthz answered {status} without a RivetHub health body); stop it or use --port"
    )]
    PortInUse {
        origin: String,
        port: u16,
        status: reqwest::StatusCode,
        source: serde_json::Error,
    },
    #[error(
        "RivetHub local node did not answer at {origin} within {seconds}s; its output is in {}",
        log.display()
    )]
    NotReady {
        origin: String,
        port: u16,
        seconds: u64,
        log: PathBuf,
    },
}

enum Probe {
    Silent,
    Node(Box<HealthzResponse>),
    Foreign {
        status: reqwest::StatusCode,
        source: serde_json::Error,
    },
}

struct Target {
    endpoint: Endpoint,
    origin: String,
    port: u16,
}

impl Target {
    fn foreign(&self, status: reqwest::StatusCode, source: serde_json::Error) -> Refusal {
        Refusal::PortInUse {
            origin: self.origin.clone(),
            port: self.port,
            status,
            source,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
enum Summary<'a> {
    Started(Running<'a>),
    AlreadyRunning(Running<'a>),
    Failed(Failure<'a>),
}

#[derive(Serialize)]
struct Running<'a> {
    origin: &'a str,
    port: u16,
    pid: Option<i32>,
    node: &'a str,
    version: Option<&'a str>,
}

#[derive(Serialize)]
struct Failure<'a> {
    reason: FailureReason,
    error: String,
    origin: Option<&'a str>,
    port: Option<u16>,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum FailureReason {
    PortInUse,
    NotReady,
    Error,
}

pub(crate) async fn ensure(
    env: &Environment,
    request: Ensure,
) -> Result<Result<Ready, Refusal>, Error> {
    if request.port == Some(0) {
        return Err(Error::GatewayPort);
    }
    let (config, adopted) = bound_config(env, request.port, request.no_lan).await?;
    let endpoint = endpoint(env, &config).await?;
    let origin = endpoint.url.origin().ascii_serialization();
    let port = endpoint.url.port_or_known_default().ok_or(Error::Address)?;
    let target = Target {
        endpoint,
        origin,
        port,
    };
    let (outcome, health) = match probe(&target, PROBE_TIMEOUT).await? {
        Probe::Node(health) => (Outcome::AlreadyRunning, *health),
        Probe::Foreign { status, source } => return Ok(Err(target.foreign(status, source))),
        Probe::Silent => match start(env, request, &target).await? {
            Ok(ready) => ready,
            Err(refusal) => return Ok(Err(refusal)),
        },
    };
    let pid = process::read_pid(env).await?.map(Pid::as_raw);
    Ok(Ok(Ready {
        outcome,
        origin: target.origin,
        port: target.port,
        pid,
        health,
        adopted,
    }))
}

pub(crate) fn render(result: Result<Result<Ready, Refusal>, Error>, json: bool) -> Report {
    match (result, json) {
        (Ok(Ok(ready)), false) => human(&ready),
        (Ok(Ok(ready)), true) => {
            let running = Running {
                origin: &ready.origin,
                port: ready.port,
                pid: ready.pid,
                node: &ready.health.node,
                version: ready.health.version.as_deref(),
            };
            let mut report = machine(
                &match ready.outcome {
                    Outcome::Started => Summary::Started(running),
                    Outcome::AlreadyRunning => Summary::AlreadyRunning(running),
                },
                Exit::Ok,
            );
            report.stderr = notice(&ready);
            report
        }
        (Ok(Err(refusal)), false) => Report::failure(&refusal),
        (Ok(Err(refusal)), true) => {
            let (reason, origin, port) = match &refusal {
                Refusal::PortInUse { origin, port, .. } => (FailureReason::PortInUse, origin, port),
                Refusal::NotReady { origin, port, .. } => (FailureReason::NotReady, origin, port),
            };
            failed(Failure {
                reason,
                error: refusal.to_string(),
                origin: Some(origin),
                port: Some(*port),
            })
        }
        (Err(error), false) => Report::failure(&error),
        (Err(error), true) => failed(Failure {
            reason: FailureReason::Error,
            error: error.to_string(),
            origin: None,
            port: None,
        }),
    }
}

fn human(ready: &Ready) -> Report {
    let origin = &ready.origin;
    let mut report = Report::success(match ready.outcome {
        Outcome::AlreadyRunning => format!("RivetHub local node is running at {origin}\n"),
        Outcome::Started => format!("Started RivetHub local node at {origin}\n"),
    });
    report.stderr = notice(ready);
    report
}

fn notice(ready: &Ready) -> String {
    match &ready.adopted {
        Some(notice) => format!("{notice}\n"),
        None => String::new(),
    }
}

fn failed(failure: Failure<'_>) -> Report {
    let stderr = format!("Error: {}\n", failure.error);
    let mut report = machine(&Summary::Failed(failure), Exit::Failure);
    report.stderr = stderr;
    report
}

fn machine(summary: &Summary<'_>, exit: Exit) -> Report {
    match serde_json::to_string(summary) {
        Ok(line) => Report {
            stdout: format!("{line}\n"),
            stderr: String::new(),
            exit,
            stdout_raw: Vec::new(),
        },
        Err(error) => Report::failure(&Error::from(error)),
    }
}

async fn start(
    env: &Environment,
    request: Ensure,
    target: &Target,
) -> Result<Result<(Outcome, HealthzResponse), Refusal>, Error> {
    let paths = Paths::new(env)?;
    let deadline = Instant::now() + Duration::from_secs(request.wait_seconds);
    let run = paths.state_dir().join(LOCK_DIRECTORY);
    let logs = paths.logs_dir();
    let held = tokio::task::spawn_blocking(move || lock(&run)).await??;
    let outcome = match &held {
        Some(_) => match probe(target, PROBE_TIMEOUT).await? {
            Probe::Silent => {
                spawn(env, request, paths.home(), &logs)?;
                Outcome::Started
            }
            Probe::Node(health) => return Ok(Ok((Outcome::AlreadyRunning, *health))),
            Probe::Foreign { status, source } => return Ok(Err(target.foreign(status, source))),
        },
        None => Outcome::AlreadyRunning,
    };
    let last = wait_until(target, deadline).await?;
    drop(held);
    Ok(match last {
        Probe::Node(health) => Ok((outcome, *health)),
        Probe::Foreign { status, source } => Err(target.foreign(status, source)),
        Probe::Silent => Err(Refusal::NotReady {
            origin: target.origin.clone(),
            port: target.port,
            seconds: request.wait_seconds,
            log: logs.join(LOG_FILE),
        }),
    })
}

async fn bound_config(
    env: &Environment,
    port: Option<u16>,
    no_lan: bool,
) -> Result<(Config, Option<String>), Error> {
    let env = env.clone();
    tokio::task::spawn_blocking(move || {
        let adopted = config::adopt_legacy_state(&env)?;
        let mut config = match config::resolve_config_path(&env)? {
            Some(location) => Config::load(&env, &location, Profile::Standalone)?.config,
            None => generated(&env, port, no_lan)?,
        };
        apply_bind(&mut config, port, no_lan);
        Ok((config, adopted))
    })
    .await?
}

fn generated(env: &Environment, port: Option<u16>, no_lan: bool) -> Result<Config, Error> {
    Ok(Config {
        gateway: Some(GatewaySection {
            host: Some(bind_host(env, no_lan)?.to_owned()),
            port: bind_port(port),
            ..GatewaySection::default()
        }),
        ..Config::default()
    })
}

async fn probe(target: &Target, timeout: Duration) -> Result<Probe, Error> {
    let sent = target
        .endpoint
        .client
        .get(target.endpoint.url.clone())
        .timeout(timeout)
        .send()
        .await;
    let response = match sent {
        Ok(response) => response,
        Err(error) if error.is_connect() || error.is_timeout() => return Ok(Probe::Silent),
        Err(error) => return Err(error.into()),
    };
    let status = response.status();
    let body = response.bytes().await?;
    Ok(match serde_json::from_slice::<HealthzResponse>(&body) {
        Ok(health) => Probe::Node(Box::new(health)),
        Err(source) => Probe::Foreign { status, source },
    })
}

async fn wait_until(target: &Target, deadline: Instant) -> Result<Probe, Error> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(Probe::Silent);
        }
        match probe(target, remaining.min(PROBE_TIMEOUT)).await? {
            Probe::Silent => tokio::time::sleep(remaining.min(POLL_INTERVAL)).await,
            answered @ (Probe::Node(_) | Probe::Foreign { .. }) => return Ok(answered),
        }
    }
}

fn lock(run: &Path) -> Result<Option<Flock<File>>, Error> {
    DirBuilder::new().recursive(true).mode(0o700).create(run)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(run.join(LOCK_FILE))?;
    match Flock::lock(file, FlockArg::LockExclusiveNonblock) {
        Ok(held) => Ok(Some(held)),
        Err((_, Errno::EWOULDBLOCK)) => Ok(None),
        Err((_, error)) => Err(error.into()),
    }
}

fn spawn(env: &Environment, request: Ensure, home: &Path, logs: &Path) -> Result<(), Error> {
    let executable = std::env::current_exe()?;
    DirBuilder::new().recursive(true).mode(0o700).create(logs)?;
    let output = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(logs.join(LOG_FILE))?;
    let mut command = tokio::process::Command::new(executable);
    command.arg("local");
    if let Some(port) = request.port {
        command.arg("--port").arg(port.to_string());
    }
    if request.no_lan {
        command.arg("--no-lan");
    }
    command
        .env_clear()
        .envs(env.iter())
        .current_dir(home)
        .stdin(Stdio::null())
        .stdout(output.try_clone()?)
        .stderr(output)
        .process_group(0)
        .spawn()?;
    Ok(())
}
