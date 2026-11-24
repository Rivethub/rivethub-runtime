use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use nix::errno::Errno;
use nix::unistd::Pid;
use serde::{Deserialize, Serialize};

pub const FILE_NAME: &str = "gateway.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeRecord {
    pub pid: u32,
    pub state_dir: PathBuf,
    pub host: IpAddr,
    pub port: u16,
    pub tls: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    #[error("the gateway runtime record {path} cannot be written: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("the gateway runtime record cannot be encoded: {0}")]
    Encode(#[from] serde_json::Error),
}

#[must_use]
pub fn record_path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE_NAME)
}

#[derive(Debug)]
pub struct Published {
    path: PathBuf,
    pid: u32,
}

impl Drop for Published {
    fn drop(&mut self) {
        if read_file(&self.path).is_some_and(|record| record.pid == self.pid)
            && let Err(error) = std::fs::remove_file(&self.path)
        {
            tracing::debug!(%error, path = %self.path.display(), "gateway runtime record not removed");
        }
    }
}

pub fn publish(state_dir: &Path, bound: SocketAddr, tls: bool) -> Result<Published, RecordError> {
    let path = record_path(state_dir);
    let pid = std::process::id();
    let record = RuntimeRecord {
        pid,
        state_dir: state_dir.to_path_buf(),
        host: bound.ip(),
        port: bound.port(),
        tls,
    };
    let io = |source| RecordError::Io {
        path: path.clone(),
        source,
    };
    let temp = path.with_extension(format!("{pid}.tmp"));
    std::fs::write(&temp, serde_json::to_vec(&record)?).map_err(io)?;
    std::fs::rename(&temp, &path).map_err(io)?;
    Ok(Published { path, pid })
}

#[must_use]
pub fn read_live(state_dir: &Path) -> Option<RuntimeRecord> {
    let record = read_file(&record_path(state_dir))?;
    (record.state_dir == state_dir && alive(record.pid)).then_some(record)
}

fn read_file(path: &Path) -> Option<RuntimeRecord> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(error) => {
            tracing::debug!(%error, path = %path.display(), "gateway runtime record unreadable");
            return None;
        }
    };
    match serde_json::from_str(&text) {
        Ok(record) => Some(record),
        Err(error) => {
            tracing::debug!(%error, path = %path.display(), "gateway runtime record malformed");
            None
        }
    }
}

fn alive(pid: u32) -> bool {
    i32::try_from(pid).is_ok_and(|raw| {
        matches!(
            nix::sys::signal::kill(Pid::from_raw(raw), None),
            Ok(()) | Err(Errno::EPERM)
        )
    })
}
