use std::fs::OpenOptions;
use std::io::{ErrorKind, Read};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use config::{Environment, Paths};
use nix::errno::Errno;
use nix::fcntl::OFlag;
use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;

use crate::{Error, Report};

pub(crate) async fn read_pid(env: &Environment) -> Result<Option<Pid>, Error> {
    let paths = Paths::new(env)?;
    let root = paths.state_dir();
    let path = paths.pid_file();
    tokio::task::spawn_blocking(move || read_file(&root, &path)).await?
}

fn read_file(root: &Path, path: &Path) -> Result<Option<Pid>, Error> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    }
    if !std::fs::symlink_metadata(root)?.is_dir() {
        return Err(Error::Pid);
    }
    let root = root.canonicalize()?;
    let target = path.canonicalize()?;
    if !target.starts_with(&root) {
        return Err(Error::Pid);
    }
    let file = OpenOptions::new()
        .read(true)
        .custom_flags((OFlag::O_NOFOLLOW | OFlag::O_NONBLOCK).bits())
        .open(target)?;
    if !file.metadata()?.is_file() {
        return Err(Error::Pid);
    }
    let mut text = String::new();
    file.take(33).read_to_string(&mut text)?;
    if text.len() > 32 {
        return Err(Error::Pid);
    }
    let number: u32 = text.trim().parse().map_err(|_| Error::Pid)?;
    if number <= 1 {
        return Err(Error::Pid);
    }
    Ok(Some(Pid::from_raw(
        i32::try_from(number).map_err(|_| Error::Pid)?,
    )))
}

pub(crate) async fn stop(env: &Environment) -> Result<Report, Error> {
    let Some(pid) = read_pid(env).await? else {
        return Ok(Report::success(
            "No running instance found (no PID file).\n",
        ));
    };
    match kill(pid, Signal::SIGTERM) {
        Ok(()) => Ok(Report::success(format!("Sent SIGTERM to PID {pid}\n"))),
        Err(Errno::ESRCH) => Ok(Report::success(
            "PID file exists but process is not running.\n",
        )),
        Err(error) => Err(error.into()),
    }
}

pub(crate) async fn status(env: &Environment) -> Result<String, Error> {
    let Some(pid) = read_pid(env).await? else {
        return Ok("RivetHub is not running (no PID file).\n".to_owned());
    };
    match kill(pid, None) {
        Ok(()) => Ok(format!(
            "RivetHub is running (PID {pid}; health unavailable).\n"
        )),
        Err(Errno::ESRCH) => Ok("RivetHub is not running (stale PID file).\n".to_owned()),
        Err(error) => Err(error.into()),
    }
}
