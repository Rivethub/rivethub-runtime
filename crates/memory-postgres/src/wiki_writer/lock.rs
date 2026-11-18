use jiff::Timestamp;
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{ErrorKind, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Weak};
use std::time::Duration;
use wire::time::TimestampFormat as _;

use kernel::wiki::builder::WikiWriteError;
use parking_lot::Mutex;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

static OWNERS: LazyLock<Mutex<HashMap<PathBuf, Weak<Semaphore>>>> = LazyLock::new(Mutex::default);

#[derive(Debug)]
pub(super) struct WriterLock {
    path: PathBuf,
    token: String,
    permit: OwnedSemaphorePermit,
}

impl WriterLock {
    pub(super) async fn acquire(root: &Path) -> Result<Self, WikiWriteError> {
        let owner = {
            let mut owners = OWNERS.lock();
            owners.retain(|_, owner| owner.strong_count() > 0);
            if let Some(owner) = owners.get(root).and_then(Weak::upgrade) {
                owner
            } else {
                let owner = Arc::new(Semaphore::new(1));
                owners.insert(root.to_path_buf(), Arc::downgrade(&owner));
                owner
            }
        };
        let permit = owner
            .acquire_owned()
            .await
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        let path = root.join(".wiki-writer.lock");
        let token = format!("{}-{}", std::process::id(), Uuid::new_v4());
        for attempt in 0..200_u32 {
            let lock_path = path.clone();
            let lock_token = token.clone();
            let result = tokio::task::spawn_blocking(move || acquire_once(&lock_path, &lock_token))
                .await??;
            match result {
                Attempt::Owned => {
                    return Ok(Self {
                        path,
                        token,
                        permit,
                    });
                }
                Attempt::Reclaimed => {}
                Attempt::Busy => {
                    let jitter =
                        Uuid::new_v4().as_u128() % (20_000 * u128::from((attempt + 1).min(10)));
                    let jitter = u64::try_from(jitter).map_err(std::io::Error::other)?;
                    tokio::time::sleep(Duration::from_micros(10_000 + jitter)).await;
                }
            }
        }
        Err(WikiWriteError::LockTimeout)
    }

    pub(super) async fn release(self) -> Result<(), WikiWriteError> {
        tokio::task::spawn_blocking(move || {
            let Self {
                path,
                token,
                permit,
            } = self;
            let result = release(&path, &token);
            drop(permit);
            result
        })
        .await??;
        Ok(())
    }
}

#[derive(Debug)]
enum Attempt {
    Owned,
    Reclaimed,
    Busy,
}

fn acquire_once(path: &Path, token: &str) -> Result<Attempt, std::io::Error> {
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
    {
        Ok(mut file) => {
            writeln!(file, "{token} {}", Timestamp::now().epoch_millis())?;
            file.sync_all()?;
            Ok(Attempt::Owned)
        }
        Err(error) if error.kind() == ErrorKind::AlreadyExists => reclaim(path),
        Err(error) => Err(error),
    }
}

fn reclaim(path: &Path) -> Result<Attempt, std::io::Error> {
    let Some(content) = read_lock(path)? else {
        return Ok(Attempt::Reclaimed);
    };
    let Some(timestamp) = content.split_whitespace().nth(1) else {
        return Ok(Attempt::Busy);
    };
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let Ok(timestamp) = timestamp.parse::<i64>() else {
        return Ok(Attempt::Busy);
    };
    if Timestamp::now().epoch_millis().saturating_sub(timestamp) <= 60_000 {
        return Ok(Attempt::Busy);
    }
    if read_lock(path)?.as_ref() != Some(&content) {
        return Ok(Attempt::Reclaimed);
    }
    let stale = path.with_file_name(format!(
        ".wiki-writer.lock.stale-{}-{}",
        std::process::id(),
        Uuid::new_v4()
    ));
    match std::fs::rename(path, &stale) {
        Ok(()) => {
            std::fs::remove_file(stale)?;
            Ok(Attempt::Reclaimed)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(Attempt::Reclaimed),
        Err(error) => Err(error),
    }
}

fn read_lock(path: &Path) -> Result<Option<String>, std::io::Error> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.file_type().is_file() => {
            return Err(std::io::Error::new(
                ErrorKind::InvalidData,
                "wiki lock is not a regular file",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    }
    match std::fs::read_to_string(path) {
        Ok(content) => Ok(Some(content)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn release(path: &Path, token: &str) -> Result<(), std::io::Error> {
    if read_lock(path)?
        .as_deref()
        .and_then(|content| content.split_whitespace().next())
        == Some(token)
    {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
