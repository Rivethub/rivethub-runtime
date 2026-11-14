use std::ffi::OsString;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use config::Environment;

use crate::roots::{self, ServiceRoot};
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::process::Child;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::{Error, Report};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RestartPolicy {
    initial: Duration,
    maximum: Duration,
    reset_after: Duration,
    grace: Duration,
}

impl RestartPolicy {
    const fn node() -> Self {
        Self {
            initial: Duration::from_secs(1),
            maximum: Duration::from_secs(60),
            reset_after: Duration::from_secs(60),
            grace: kernel::shutdown::SHUTDOWN_GRACE,
        }
    }

    const fn initial(self) -> Duration {
        self.initial
    }

    const fn grace(self) -> Duration {
        self.grace
    }

    fn doubled(self, current: Duration) -> Duration {
        match current.checked_mul(2) {
            Some(next) if next <= self.maximum => next,
            Some(_) | None => self.maximum,
        }
    }

    fn after_exit(self, uptime: Duration, current: Duration) -> Duration {
        if uptime >= self.reset_after {
            self.initial
        } else {
            current
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisionEvent {
    Spawned {
        root: ServiceRoot,
        arguments: Vec<OsString>,
    },
    Waiting {
        root: ServiceRoot,
        delay: Duration,
    },
    Stopped {
        root: ServiceRoot,
        signal: Signal,
    },
}

#[derive(Debug, Clone)]
pub struct Supervision {
    pub observer: Option<UnboundedSender<SupervisionEvent>>,
    pub program: Option<PathBuf>,
    pub inherit_output: bool,
}

impl Supervision {
    #[must_use]
    pub fn production() -> Self {
        Self {
            observer: None,
            program: None,
            inherit_output: true,
        }
    }

    pub async fn run(
        &self,
        env: &Environment,
        config: Option<PathBuf>,
        shutdown: &CancellationToken,
    ) -> Result<Report, Error> {
        let prepared = prepare(env, config, self.program.as_deref()).await?;
        let guard = PidGuard {
            claim: Some(prepared.claim),
        };
        let result = supervise_all(
            prepared.program,
            prepared.services,
            prepared.config_path,
            shutdown,
            self.observer.as_ref(),
            self.inherit_output,
        )
        .await;
        match (result, guard.release()) {
            (Ok(()), Ok(())) => Ok(Report::success("")),
            (Err(error), Ok(())) => Err(error),
            (Ok(()), Err(error)) => Err(error.into()),
            (Err(primary), Err(error)) => {
                tracing::error!("pid file removal failed: {error}");
                Err(primary)
            }
        }
    }
}

struct Prepared {
    program: PathBuf,
    services: Vec<ServiceRoot>,
    config_path: PathBuf,
    claim: config::NodeClaim,
}

async fn prepare(
    env: &Environment,
    config: Option<PathBuf>,
    program: Option<&Path>,
) -> Result<Prepared, Error> {
    let env = match config {
        Some(path) => env
            .clone()
            .with_override(config::CONFIG_PATH_VARIABLE, path),
        None => env.clone(),
    };
    let owned = env.clone();
    let located = tokio::task::spawn_blocking(move || locate(&owned)).await??;
    Ok(Prepared {
        program: program_path(program).await?,
        services: located.services,
        config_path: located.config_path,
        claim: located.claim,
    })
}

struct Located {
    services: Vec<ServiceRoot>,
    config_path: PathBuf,
    claim: config::NodeClaim,
}

fn locate(env: &Environment) -> Result<Located, Error> {
    if let Some(notice) = config::adopt_legacy_state(env)? {
        tracing::warn!("{notice}");
    }
    let Some(location) = config::resolve_config_path(env)? else {
        return Err(Error::StartConfig);
    };
    let loaded = config::Config::load(env, &location)?;
    let state = config::Paths::new(env)?.state_dir();
    let claim = config::claim_node(&state, std::process::id())?;
    Ok(Located {
        services: roots::roots_from(&loaded.config.service_switches()),
        config_path: location.path().to_owned(),
        claim,
    })
}

async fn program_path(program: Option<&Path>) -> Result<PathBuf, Error> {
    if let Some(program) = program {
        return Ok(program.to_path_buf());
    }
    Ok(tokio::task::spawn_blocking(std::env::current_exe).await??)
}

struct PidGuard {
    claim: Option<config::NodeClaim>,
}

impl PidGuard {
    fn release(mut self) -> Result<(), config::StateError> {
        match self.claim.take() {
            Some(claim) => claim.release(),
            None => Ok(()),
        }
    }
}

impl Drop for PidGuard {
    fn drop(&mut self) {
        if let Some(claim) = self.claim.take() {
            match claim.release() {
                Ok(()) => {}
                Err(error) => tracing::error!("pid file removal failed: {error}"),
            }
        }
    }
}

async fn supervise_all(
    program: PathBuf,
    services: Vec<ServiceRoot>,
    config_path: PathBuf,
    shutdown: &CancellationToken,
    observer: Option<&UnboundedSender<SupervisionEvent>>,
    inherit_output: bool,
) -> Result<(), Error> {
    if services.is_empty() {
        shutdown.cancelled().await;
        return Ok(());
    }
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let tracker = TaskTracker::new();
    for root in services {
        let arguments = arguments_for(&root, &config_path);
        let tx = tx.clone();
        let program = program.clone();
        let shutdown = shutdown.clone();
        let observer = observer.cloned();
        tracker.spawn(async move {
            let result =
                supervise_root(program, root, arguments, shutdown, observer, inherit_output).await;
            match tx.send(result) {
                Ok(()) => {}
                Err(error) => tracing::debug!("supervision result was not received: {error}"),
            }
        });
    }
    drop(tx);
    let mut failure = None;
    loop {
        tokio::select! {
            biased;
            () = shutdown.cancelled() => break,
            message = rx.recv() => match message {
                Some(Ok(())) => {}
                Some(Err(error)) => {
                    failure = Some(error);
                    shutdown.cancel();
                    break;
                }
                None => break,
            },
        }
    }
    tracker.close();
    tracker.wait().await;
    while let Ok(message) = rx.try_recv() {
        if let Err(error) = message {
            match failure {
                Some(_) => tracing::error!("additional supervision failure: {error}"),
                None => failure = Some(error),
            }
        }
    }
    match failure {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

async fn supervise_root(
    program: PathBuf,
    root: ServiceRoot,
    arguments: Vec<OsString>,
    shutdown: CancellationToken,
    observer: Option<UnboundedSender<SupervisionEvent>>,
    inherit_output: bool,
) -> Result<(), Error> {
    let policy = RestartPolicy::node();
    let mut stored = policy.initial();
    loop {
        if shutdown.is_cancelled() {
            return Ok(());
        }
        let mut child = spawn_child(&program, &arguments, inherit_output)?;
        let child_pid = require_pid(&mut child).await?;
        tracing::info!("spawned {root} pid {child_pid}");
        emit(
            observer.as_ref(),
            SupervisionEvent::Spawned {
                root: root.clone(),
                arguments: arguments.clone(),
            },
        );
        let started = tokio::time::Instant::now();
        tokio::select! {
            () = shutdown.cancelled() => {
                let signal = stop_child(&mut child, policy.grace()).await?;
                emit(observer.as_ref(), SupervisionEvent::Stopped { root, signal });
                return Ok(());
            }
            status = child.wait() => {
                let status = status?;
                if shutdown.is_cancelled() {
                    emit(
                        observer.as_ref(),
                        SupervisionEvent::Stopped {
                            root,
                            signal: Signal::SIGTERM,
                        },
                    );
                    return Ok(());
                }
                let delay = policy.after_exit(started.elapsed(), stored);
                emit(
                    observer.as_ref(),
                    SupervisionEvent::Waiting {
                        root: root.clone(),
                        delay,
                    },
                );
                tracing::warn!(
                    "service root {root} exited (code {:?}, signal {:?}); restarting in {delay:?}",
                    status.code(),
                    status.signal()
                );
                tokio::select! {
                    () = shutdown.cancelled() => return Ok(()),
                    () = tokio::time::sleep(delay) => {
                        stored = policy.doubled(delay);
                    }
                }
            }
        }
    }
}

fn arguments_for(root: &ServiceRoot, config: &Path) -> Vec<OsString> {
    let config = config.as_os_str().to_owned();
    match root {
        ServiceRoot::Gateway => vec!["gateway".into(), "--config".into(), config],
        ServiceRoot::Worker(role) => vec![
            "worker".into(),
            "--role".into(),
            role.to_string().into(),
            "--config".into(),
            config,
        ],
        ServiceRoot::Mcp => vec!["mcp".into(), "--config".into(), config],
    }
}

fn spawn_child(
    program: &Path,
    arguments: &[OsString],
    inherit_output: bool,
) -> Result<Child, Error> {
    let mut command = tokio::process::Command::new(program);
    command
        .args(arguments)
        .kill_on_drop(true)
        .stdin(std::process::Stdio::null());
    if inherit_output {
        command.stdout(std::process::Stdio::inherit());
        command.stderr(std::process::Stdio::inherit());
    } else {
        command.stdout(std::process::Stdio::null());
        command.stderr(std::process::Stdio::null());
    }
    Ok(command.spawn()?)
}

async fn require_pid(child: &mut Child) -> Result<u32, Error> {
    match child.id() {
        Some(pid) if pid > 1 => Ok(pid),
        Some(pid) => {
            tracing::error!("refusing child pid {pid}");
            stop_unusable(child).await;
            Err(Error::Pid)
        }
        None => Err(Error::Pid),
    }
}

async fn stop_unusable(child: &mut Child) {
    if let Err(error) = child.kill().await {
        tracing::error!("stopping an unusable child failed: {error}");
    }
    if let Err(error) = child.wait().await {
        tracing::error!("reaping an unusable child failed: {error}");
    }
}

async fn stop_child(child: &mut Child, grace: Duration) -> Result<Signal, Error> {
    let Some(pid) = child.id() else {
        tracing::warn!("supervised child had already exited");
        return Ok(Signal::SIGTERM);
    };
    let raw = match i32::try_from(pid) {
        Ok(raw) if raw > 1 => raw,
        Ok(raw) => {
            tracing::error!("refusing to signal child pid {raw}");
            return Ok(Signal::SIGKILL);
        }
        Err(error) => {
            tracing::error!("supervised child pid {pid} cannot be signalled: {error}");
            return Ok(Signal::SIGKILL);
        }
    };
    let target = Pid::from_raw(raw);
    match kill(target, Signal::SIGTERM) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(error) => tracing::error!("forwarding SIGTERM to {pid} failed: {error}"),
    }
    let exited = tokio::select! {
        result = child.wait() => match result {
            Ok(_status) => true,
            Err(error) => {
                tracing::error!("waiting for child {pid} failed: {error}");
                false
            }
        },
        () = tokio::time::sleep(grace) => false,
    };
    if exited {
        return Ok(Signal::SIGTERM);
    }
    match kill(target, Signal::SIGKILL) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(error) => tracing::error!("sending SIGKILL to {pid} failed: {error}"),
    }
    reap_killed(child, pid, grace).await
}

fn grace_elapsed_on_the_wall(wall: Duration, grace: Duration) -> bool {
    wall >= grace / 2
}

fn killed(pid: u32, result: std::io::Result<std::process::ExitStatus>) -> Signal {
    if let Err(error) = result {
        tracing::error!("reaping child {pid} failed: {error}");
    }
    Signal::SIGKILL
}

async fn reap_killed(child: &mut Child, pid: u32, grace: Duration) -> Result<Signal, Error> {
    let started = Instant::now();
    let finished = tokio::select! {
        result = child.wait() => Some(result),
        () = tokio::time::sleep(grace) => None,
    };
    let wall = started.elapsed();
    match finished {
        Some(result) => Ok(killed(pid, result)),
        None if grace_elapsed_on_the_wall(wall, grace) => {
            tracing::error!("shutdown grace of 10s elapsed; still running: child {pid}");
            Err(Error::Io(std::io::Error::other(format!(
                "shutdown grace of 10s elapsed; still running: child {pid}"
            ))))
        }
        None => {
            tracing::debug!(
                "shutdown timer advanced after {wall:?} of wall time; reaping child {pid}"
            );
            Ok(killed(pid, child.wait().await))
        }
    }
}

fn emit(observer: Option<&UnboundedSender<SupervisionEvent>>, event: SupervisionEvent) {
    let Some(observer) = observer else {
        return;
    };
    match observer.send(event) {
        Ok(()) => {}
        Err(error) => tracing::debug!("supervision observer closed: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use super::{RestartPolicy, grace_elapsed_on_the_wall};
    use std::time::Duration;

    #[test]
    fn backoff_doubles_until_the_cap_and_resets_after_uptime() {
        let policy = RestartPolicy::node();
        let mut stored = policy.initial();
        let mut waits = Vec::new();
        for _ in 0..9 {
            let wait = policy.after_exit(Duration::from_secs(1), stored);
            waits.push(wait);
            stored = policy.doubled(wait);
        }
        assert_eq!(
            waits,
            [
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4),
                Duration::from_secs(8),
                Duration::from_secs(16),
                Duration::from_secs(32),
                Duration::from_secs(60),
                Duration::from_secs(60),
                Duration::from_secs(60),
            ]
        );
        let reset = policy.after_exit(Duration::from_secs(60), Duration::from_secs(2));
        assert_eq!(reset, Duration::from_secs(1));
        assert_eq!(policy.doubled(reset), Duration::from_secs(2));
        assert_eq!(
            policy.after_exit(Duration::from_secs(59), Duration::from_secs(4)),
            Duration::from_secs(4)
        );
        assert_eq!(policy.grace(), Duration::from_secs(10));
    }

    #[test]
    fn a_paused_clock_is_not_the_grace() {
        let grace = Duration::from_secs(10);
        assert!(!grace_elapsed_on_the_wall(Duration::from_millis(60), grace));
        assert!(!grace_elapsed_on_the_wall(Duration::from_secs(4), grace));
        assert!(grace_elapsed_on_the_wall(grace / 2, grace));
        assert!(grace_elapsed_on_the_wall(grace, grace));
    }
}
