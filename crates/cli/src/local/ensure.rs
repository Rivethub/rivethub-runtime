use std::fs::{DirBuilder, File, OpenOptions};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use config::model::GatewaySection;
use config::{Config, Environment, Paths, Profile};
use nix::errno::Errno;
use nix::fcntl::{Flock, FlockArg};
use tokio::time::Instant;

use super::{apply_bind, bind_host, bind_port};
use crate::status::{Endpoint, endpoint};
use crate::{Error, Report};

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

enum Outcome {
    Running,
    Started,
}

pub(crate) async fn ensure(env: &Environment, request: Ensure) -> Result<Report, Error> {
    if request.port == Some(0) {
        return Err(Error::GatewayPort);
    }
    let (config, adopted) = bound_config(env, request.port, request.no_lan).await?;
    let target = endpoint(env, &config).await?;
    let origin = target.url.origin().ascii_serialization();
    let outcome = if answers(&target, PROBE_TIMEOUT).await? {
        Outcome::Running
    } else {
        start(env, request, &target, &origin).await?
    };
    let mut report = Report::success(match outcome {
        Outcome::Running => format!("RivetHub local node is running at {origin}\n"),
        Outcome::Started => format!("Started RivetHub local node at {origin}\n"),
    });
    if let Some(notice) = adopted {
        report.stderr = format!("{notice}\n");
    }
    Ok(report)
}

async fn start(
    env: &Environment,
    request: Ensure,
    target: &Endpoint,
    origin: &str,
) -> Result<Outcome, Error> {
    let paths = Paths::new(env)?;
    let deadline = Instant::now() + Duration::from_secs(request.wait_seconds);
    let run = paths.state_dir().join(LOCK_DIRECTORY);
    let logs = paths.logs_dir();
    let held = tokio::task::spawn_blocking(move || lock(&run)).await??;
    let outcome = match &held {
        Some(_) if !answers(target, PROBE_TIMEOUT).await? => {
            spawn(env, request, paths.home(), &logs)?;
            Outcome::Started
        }
        Some(_) | None => Outcome::Running,
    };
    let ready = wait_until(target, deadline).await?;
    drop(held);
    if ready {
        Ok(outcome)
    } else {
        Err(Error::LocalStart {
            origin: origin.to_owned(),
            seconds: request.wait_seconds,
            log: logs.join(LOG_FILE),
        })
    }
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

async fn answers(target: &Endpoint, timeout: Duration) -> Result<bool, Error> {
    match target
        .client
        .get(target.url.clone())
        .timeout(timeout)
        .send()
        .await
    {
        Ok(_) => Ok(true),
        Err(error) if error.is_connect() || error.is_timeout() => Ok(false),
        Err(error) => Err(error.into()),
    }
}

async fn wait_until(target: &Endpoint, deadline: Instant) -> Result<bool, Error> {
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Ok(false);
        }
        if answers(target, remaining.min(PROBE_TIMEOUT)).await? {
            return Ok(true);
        }
        tokio::time::sleep(remaining.min(POLL_INTERVAL)).await;
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
