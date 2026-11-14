use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use nix::errno::Errno;
use nix::sys::signal::kill;
use nix::unistd::Pid;

use super::PID_FILE_NAME;
use crate::NodeClaim;
use crate::StateError;

pub(super) fn ensure_directory(directory: &Path) -> Result<(), StateError> {
    match std::fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(StateError::Symlink {
            path: directory.to_owned(),
        }),
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(StateError::Inspect {
            path: directory.to_owned(),
            source: std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "state path is not a directory",
            ),
        }),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(directory).map_err(|source| StateError::Inspect {
                path: directory.to_owned(),
                source,
            })?;
            match std::fs::symlink_metadata(directory) {
                Ok(metadata) if metadata.file_type().is_symlink() => Err(StateError::Symlink {
                    path: directory.to_owned(),
                }),
                Ok(_) => Ok(()),
                Err(source) => Err(StateError::Inspect {
                    path: directory.to_owned(),
                    source,
                }),
            }
        }
        Err(source) => Err(StateError::Inspect {
            path: directory.to_owned(),
            source,
        }),
    }
}

pub(super) fn claim(directory: &Path, pid: u32) -> Result<NodeClaim, StateError> {
    let pid_path = directory.join(PID_FILE_NAME);
    if pid <= 1 || i32::try_from(pid).is_err() {
        return Err(StateError::InvalidOwner { path: pid_path });
    }
    ensure_directory(directory)?;
    let locks = match check(directory) {
        Ok(locks) => locks,
        Err(StateError::Locked { path }) if path == pid_path => {
            return Err(node_already_running(&pid_path));
        }
        Err(StateError::LiveOwner { path, pid }) if path == pid_path => {
            return Err(StateError::AlreadyRunning { pid });
        }
        Err(error) => return Err(error),
    };
    let mut held = None;
    for (path, file) in locks {
        if path == pid_path {
            held = Some(file);
        } else {
            drop(file);
        }
    }
    let mut file = match held {
        Some(file) => file,
        None => match open_pid(&pid_path) {
            Ok(file) => file,
            Err(StateError::Locked { path }) if path == pid_path => {
                return Err(node_already_running(&pid_path));
            }
            Err(error) => return Err(error),
        },
    };
    match rewrite(&mut file, &pid_path, pid) {
        Ok(()) => Ok(NodeClaim::hold(file, pid_path)),
        Err(StateError::LiveOwner { path, pid }) if path == pid_path => {
            Err(StateError::AlreadyRunning { pid })
        }
        Err(error) => Err(error),
    }
}

fn node_already_running(path: &Path) -> StateError {
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: the pid file is already locked; when it cannot be opened there is no pid to name, so the error stays the held lock"
        )
    )]
    let Ok(mut file) = OpenOptions::new()
        .read(true)
        .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits())
        .open(path)
    else {
        return StateError::Locked {
            path: path.to_owned(),
        };
    };
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "PATTERNS FALLBACK: the pid file is already locked; when its text cannot be read there is no pid to name, so the error stays the held lock"
        )
    )]
    let Ok(text) = read_text(&mut file, path) else {
        return StateError::Locked {
            path: path.to_owned(),
        };
    };
    match owner_state(path, &text) {
        Ok(OwnerState::Live(pid)) => StateError::AlreadyRunning { pid },
        _ => StateError::Locked {
            path: path.to_owned(),
        },
    }
}

pub(super) fn check(directory: &Path) -> Result<Vec<(PathBuf, File)>, StateError> {
    let entries = std::fs::read_dir(directory).map_err(|source| StateError::Inspect {
        path: directory.to_owned(),
        source,
    })?;
    let mut locks = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| StateError::Inspect {
            path: directory.to_owned(),
            source,
        })?;
        let path = entry.path();
        let extension = path.extension().and_then(std::ffi::OsStr::to_str);
        if !matches!(extension, Some("pid" | "lock")) {
            continue;
        }
        if directory_lock_is_stale(&entry, &path)? {
            continue;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits())
            .open(&path)
            .map_err(|source| StateError::Inspect {
                path: path.clone(),
                source,
            })?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(StateError::Locked { path }),
            Err(std::fs::TryLockError::Error(source)) => {
                return Err(StateError::Inspect { path, source });
            }
        }
        let text = read_text(&mut file, &path)?;
        if text.trim().is_empty() && extension == Some("lock") {
            locks.push((path, file));
            continue;
        }
        match owner_state(&path, &text)? {
            OwnerState::Live(pid) => return Err(StateError::LiveOwner { path, pid }),
            OwnerState::Stale => locks.push((path, file)),
            OwnerState::Empty => return Err(StateError::InvalidOwner { path }),
        }
    }
    Ok(locks)
}

fn open_pid(path: &Path) -> Result<File, StateError> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits())
        .open(path)
        .map_err(|source| StateError::Inspect {
            path: path.to_owned(),
            source,
        })?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(std::fs::TryLockError::WouldBlock) => Err(StateError::Locked {
            path: path.to_owned(),
        }),
        Err(std::fs::TryLockError::Error(source)) => Err(StateError::Inspect {
            path: path.to_owned(),
            source,
        }),
    }
}

fn rewrite(file: &mut File, path: &Path, pid: u32) -> Result<(), StateError> {
    let text = read_text(file, path)?;
    match owner_state(path, &text)? {
        OwnerState::Empty | OwnerState::Stale => {}
        OwnerState::Live(owner) if owner == pid => {}
        OwnerState::Live(owner) => {
            return Err(StateError::LiveOwner {
                path: path.to_owned(),
                pid: owner,
            });
        }
    }
    file.seek(SeekFrom::Start(0))
        .map_err(|source| inspect(path, source))?;
    file.set_len(0).map_err(|source| inspect(path, source))?;
    writeln!(file, "{pid}").map_err(|source| inspect(path, source))?;
    file.sync_all().map_err(|source| inspect(path, source))?;
    Ok(())
}

fn read_text(file: &mut File, path: &Path) -> Result<String, StateError> {
    file.seek(SeekFrom::Start(0))
        .map_err(|source| inspect(path, source))?;
    let mut text = String::new();
    Read::by_ref(file)
        .take(1025)
        .read_to_string(&mut text)
        .map_err(|source| inspect(path, source))?;
    Ok(text)
}

enum OwnerState {
    Empty,
    Stale,
    Live(u32),
}

fn owner_state(path: &Path, text: &str) -> Result<OwnerState, StateError> {
    if text.len() > 1024 {
        return Err(StateError::InvalidOwner {
            path: path.to_owned(),
        });
    }
    if text.trim().is_empty() {
        return Ok(OwnerState::Empty);
    }
    let pid = text
        .trim()
        .parse::<u32>()
        .map_err(|_| StateError::InvalidOwner {
            path: path.to_owned(),
        })?;
    let raw = i32::try_from(pid).map_err(|_| StateError::InvalidOwner {
        path: path.to_owned(),
    })?;
    if raw <= 0 {
        return Err(StateError::InvalidOwner {
            path: path.to_owned(),
        });
    }
    match kill(Pid::from_raw(raw), None) {
        Ok(()) | Err(Errno::EPERM) => Ok(OwnerState::Live(pid)),
        Err(Errno::ESRCH) => Ok(OwnerState::Stale),
        Err(error) => Err(StateError::Inspect {
            path: path.to_owned(),
            source: error.into(),
        }),
    }
}

fn inspect(path: &Path, source: std::io::Error) -> StateError {
    StateError::Inspect {
        path: path.to_owned(),
        source,
    }
}

const DIRECTORY_LOCK_STALE: std::time::Duration = std::time::Duration::from_secs(10);

fn directory_lock_is_stale(entry: &std::fs::DirEntry, path: &Path) -> Result<bool, StateError> {
    let inspect = |source| StateError::Inspect {
        path: path.to_owned(),
        source,
    };
    let kind = entry.file_type().map_err(inspect)?;
    if !kind.is_dir() {
        return Ok(false);
    }
    let modified = entry
        .metadata()
        .map_err(inspect)?
        .modified()
        .map_err(inspect)?;
    let held = match std::time::SystemTime::now().duration_since(modified) {
        Ok(age) => age < DIRECTORY_LOCK_STALE,
        Err(_future) => true,
    };
    if held {
        return Err(StateError::Locked {
            path: path.to_owned(),
        });
    }
    Ok(true)
}
