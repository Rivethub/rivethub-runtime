use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use config::Environment;
use nix::errno::Errno;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::UpdateError;
use crate::service::command::{capture, require_success};
use crate::{Cli, Command, Error, Report, process};

const STOP_TIMEOUT: Duration = Duration::from_secs(30);
const READY_SECONDS: &str = "30";
const POLL_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Restart {
    Service,
    Standalone,
    Skipped,
}

impl Restart {
    pub(super) async fn plan(unit: &Path, no_restart: bool) -> Result<Self, UpdateError> {
        if no_restart {
            return Ok(Self::Skipped);
        }
        match tokio::fs::metadata(unit).await {
            Ok(metadata) if metadata.is_file() => Ok(Self::Service),
            Ok(_) => Err(UpdateError::Path(unit.to_path_buf())),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(Self::Standalone),
            Err(source) => Err(UpdateError::Unit {
                path: unit.to_path_buf(),
                source,
            }),
        }
    }
}

pub(super) async fn restart_standalone(
    binary: &Path,
    env: &Environment,
    shutdown: &CancellationToken,
    report: &mut Report,
) -> Result<(), UpdateError> {
    let pid = process::read_pid(env)
        .await
        .map_err(|source| failed("read the node's PID file", source))?;
    let Some(pid) = pid else {
        report
            .stdout
            .push_str("No RivetHub node is running, so none needed a restart.\n");
        return Ok(());
    };
    match kill(pid, None) {
        Ok(()) => {}
        Err(Errno::ESRCH) => {
            report.stdout.push_str(
                "No RivetHub node is running (stale PID file), so none needed a restart.\n",
            );
            return Ok(());
        }
        Err(error) => return Err(failed(format!("check PID {pid}"), error.into())),
    }
    let Some(flags) = local_flags(pid).await? else {
        report.stdout.extend([
            "The RivetHub node running as PID ",
            &pid.to_string(),
            " was not started by `rh local`; restart it to run the new binary.\n",
        ]);
        return Ok(());
    };
    report
        .stdout
        .extend(["Stopping the local node (PID ", &pid.to_string(), ")...\n"]);
    match kill(pid, Signal::SIGTERM) {
        Ok(()) | Err(Errno::ESRCH) => {}
        Err(error) => return Err(failed(format!("send SIGTERM to PID {pid}"), error.into())),
    }
    wait_gone(pid, shutdown).await?;
    report
        .stdout
        .push_str("Starting the local node with the new binary...\n");
    let mut command = tokio::process::Command::new(binary);
    command
        .env_clear()
        .envs(env.iter())
        .kill_on_drop(true)
        .args(["local", "--ensure", "--wait", READY_SECONDS])
        .args(&flags);
    let started = capture(command, "rh", shutdown)
        .await
        .and_then(require_success)
        .map_err(|source| {
            failed(
                format!("start {} local --ensure", binary.display()),
                source.into(),
            )
        })?;
    report.stdout.push_str(&started.stdout);
    report.stderr.push_str(&started.stderr);
    Ok(())
}

async fn local_flags(pid: Pid) -> Result<Option<Vec<String>>, UpdateError> {
    if !cfg!(target_os = "linux") {
        return Ok(Some(Vec::new()));
    }
    let path = PathBuf::from(format!("/proc/{pid}/cmdline"));
    let cmdline = tokio::fs::read(&path)
        .await
        .map_err(|source| failed(format!("read {}", path.display()), source.into()))?;
    Ok(local_arguments(&cmdline))
}

fn local_arguments(cmdline: &[u8]) -> Option<Vec<String>> {
    let mut from_local = cmdline
        .split(|byte| *byte == 0)
        .skip(1)
        .skip_while(|argument| *argument != b"local")
        .peekable();
    from_local.peek()?;
    let argv = std::iter::once(OsString::from("rh")).chain(
        from_local
            .filter(|argument| !argument.is_empty())
            .map(|argument| OsStr::from_bytes(argument).to_owned()),
    );
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a real absence, a process whose command line rh does not parse as `rh local` is not a node rh local started, so update leaves it running and says so"
        )
    )]
    let parsed = Cli::parse_args(argv).ok();
    match parsed {
        Some(Cli {
            command:
                Some(Command::Local {
                    port,
                    no_lan,
                    no_cloud,
                    ..
                }),
        }) => Some(bind_flags(port, no_lan, no_cloud)),
        Some(_) | None => None,
    }
}

fn bind_flags(port: Option<u16>, no_lan: bool, no_cloud: bool) -> Vec<String> {
    let mut flags = Vec::new();
    if let Some(port) = port {
        flags.extend(["--port".to_owned(), port.to_string()]);
    }
    if no_lan {
        flags.push("--no-lan".to_owned());
    }
    if no_cloud {
        flags.push("--no-cloud".to_owned());
    }
    flags
}

async fn wait_gone(pid: Pid, shutdown: &CancellationToken) -> Result<(), UpdateError> {
    let deadline = Instant::now() + STOP_TIMEOUT;
    loop {
        match kill(pid, None) {
            Err(Errno::ESRCH) => return Ok(()),
            Ok(()) => {}
            Err(error) => return Err(failed(format!("check PID {pid}"), error.into())),
        }
        if Instant::now() >= deadline {
            return Err(UpdateError::Stopping {
                pid: pid.as_raw(),
                seconds: STOP_TIMEOUT.as_secs(),
            });
        }
        tokio::select! {
            biased;
            () = shutdown.cancelled() => return Err(UpdateError::Cancelled),
            () = tokio::time::sleep(POLL_INTERVAL) => {}
        }
    }
}

fn failed(action: impl Into<String>, source: Error) -> UpdateError {
    UpdateError::Local {
        action: action.into(),
        source: Box::new(source),
    }
}
