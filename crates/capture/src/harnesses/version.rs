use std::fmt;
use std::path::Path;
use std::process::Command;
use std::process::ExitStatus;
use std::process::Stdio;
use std::sync::mpsc;
use std::time::Duration;

use nix::sys::signal::Signal;
use nix::sys::signal::kill;
use nix::unistd::Pid;
use thiserror::Error;

const ANSWER_WITHIN: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    pub(super) const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    pub(super) const fn major(self) -> u64 {
        self.major
    }

    pub(super) fn parse(output: &str) -> Result<Self, Unread> {
        let not_a_version = || Unread::NotAVersion(output.trim().to_owned());
        let word = output.split_whitespace().last().ok_or_else(not_a_version)?;
        let number = word.strip_prefix('v').unwrap_or(word);
        let parts = number
            .split('.')
            .map(str::parse::<u64>)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| not_a_version())?;
        match parts.as_slice() {
            [major, minor, patch] => Ok(Self::new(*major, *minor, *patch)),
            _ => Err(not_a_version()),
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Debug, Error)]
pub(super) enum Unread {
    #[error("could not run it: {0}")]
    Run(#[from] std::io::Error),
    #[error("its process id does not fit a pid: {0}")]
    Pid(#[from] std::num::TryFromIntError),
    #[error("gave no answer within five seconds (stopping it: {stopped:?})")]
    Silent { stopped: nix::Result<()> },
    #[error("exited with {0}")]
    Failed(ExitStatus),
    #[error("answered {0:?}, which is not a version")]
    NotAVersion(String),
}

pub(super) fn installed(binary: &Path) -> Result<Version, Unread> {
    let child = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let pid = Pid::from_raw(i32::try_from(child.id())?);
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || drop(sender.send(child.wait_with_output())));
    let output = receiver
        .recv_timeout(ANSWER_WITHIN)
        .map_err(|_| Unread::Silent {
            stopped: kill(pid, Signal::SIGKILL),
        })??;
    if !output.status.success() {
        return Err(Unread::Failed(output.status));
    }
    Version::parse(&String::from_utf8_lossy(&output.stdout))
}

pub(super) fn known(binary: &Path) -> Option<Version> {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: a harness version rh cannot read is an unknown version, and status and connect then make no claim that depends on it (CONTRACT note 204)"
        )
    )]
    let version = installed(binary).ok();
    version
}
