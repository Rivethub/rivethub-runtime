use jiff::Timestamp;
use std::collections::HashMap;
use std::fs::{File, OpenOptions, TryLockError};
use std::io::{ErrorKind, Write as _};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, LazyLock, Weak};
use std::thread::{self, JoinHandle};
use std::time::Duration;
use wire::time::TimestampFormat as _;

use kernel::wiki::builder::WikiWriteError;
use parking_lot::Mutex;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

static OWNERS: LazyLock<Mutex<HashMap<PathBuf, Weak<Semaphore>>>> = LazyLock::new(Mutex::default);

const RECLAIM_AGE_MILLIS: i64 = 60_000;
const REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(42);
const REFRESHED_LOCK: &str = "heartbeat";

#[derive(Debug)]
pub(super) struct WriterLock {
    refresh: Refresher,
    path: PathBuf,
    token: String,
    _repository: File,
    _permit: OwnedSemaphorePermit,
}

impl WriterLock {
    pub(super) async fn acquire(root: &Path) -> Result<Self, WikiWriteError> {
        tokio::time::timeout(
            ACQUIRE_TIMEOUT,
            Self::acquire_with_timing(root, RECLAIM_AGE_MILLIS, REFRESH_INTERVAL),
        )
        .await
        .map_err(|_| WikiWriteError::LockTimeout)?
    }

    async fn acquire_with_timing(
        root: &Path,
        reclaim_age_millis: i64,
        refresh_interval: Duration,
    ) -> Result<Self, WikiWriteError> {
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
            let result = tokio::task::spawn_blocking(move || {
                acquire_once(&lock_path, &lock_token, reclaim_age_millis)
            })
            .await??;
            match result {
                Attempt::Owned(repository) => {
                    let refresh_path = path.clone();
                    let refresh_token = token.clone();
                    let refresh = Refresher::start(refresh_interval, move || {
                        refresh_lock(&refresh_path, &refresh_token)
                    })?;
                    return Ok(Self {
                        refresh,
                        path,
                        token,
                        _repository: repository,
                        _permit: permit,
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
                refresh,
                path,
                token,
                _repository,
                _permit,
            } = self;
            drop(refresh);
            release(&path, &token)
        })
        .await??;
        Ok(())
    }
}

#[derive(Debug)]
struct Refresher {
    stop: Option<Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Refresher {
    fn start(
        interval: Duration,
        mut refresh: impl FnMut() -> std::io::Result<bool> + Send + 'static,
    ) -> std::io::Result<Self> {
        let (stop, wake) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("wiki-lock-refresh".to_owned())
            .spawn(move || {
                while wake.recv_timeout(interval) == Err(RecvTimeoutError::Timeout) {
                    match refresh() {
                        Ok(true) => {}
                        Ok(false) => break,
                        Err(error) => tracing::warn!(%error, "wiki writer lock refresh failed"),
                    }
                }
            })?;
        Ok(Self {
            stop: Some(stop),
            thread: Some(thread),
        })
    }
}

impl Drop for Refresher {
    fn drop(&mut self) {
        drop(self.stop.take());
        if let Some(thread) = self.thread.take() {
            drop(thread.join());
        }
    }
}

#[derive(Debug)]
enum Attempt {
    Owned(File),
    Reclaimed,
    Busy,
}

fn acquire_once(path: &Path, token: &str, reclaim_age_millis: i64) -> std::io::Result<Attempt> {
    let root = path
        .parent()
        .ok_or_else(|| std::io::Error::other("wiki lock has no parent directory"))?;
    let repository = File::open(root)?;
    match repository.try_lock() {
        Ok(()) => {}
        Err(TryLockError::WouldBlock) => return Ok(Attempt::Busy),
        Err(TryLockError::Error(error)) => return Err(error),
    }
    match publish_lock(path, token, |staged| std::fs::hard_link(staged, path)) {
        Ok(()) => Ok(Attempt::Owned(repository)),
        Err(error) if error.kind() == ErrorKind::AlreadyExists => reclaim(path, reclaim_age_millis),
        Err(error) => Err(error),
    }
}

fn publish_lock<T>(
    path: &Path,
    token: &str,
    publish: impl FnOnce(&Path) -> std::io::Result<T>,
) -> std::io::Result<T> {
    let staged = path.with_file_name(format!(".wiki-writer.lock.pending-{}", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staged)?;
    let result = (|| {
        writeln!(
            file,
            "{token} {} {REFRESHED_LOCK}",
            Timestamp::now().epoch_millis()
        )?;
        publish(&staged)
    })();
    match std::fs::remove_file(&staged) {
        Err(error) if error.kind() != ErrorKind::NotFound => {
            tracing::warn!(%error, "wiki writer lock staging cleanup failed");
        }
        _ => {}
    }
    result
}

fn refresh_lock(path: &Path, token: &str) -> std::io::Result<bool> {
    publish_lock(path, token, |staged| {
        if !owns_lock(path, token)? {
            return Ok(false);
        }
        std::fs::rename(staged, path)?;
        Ok(true)
    })
}

fn reclaim(path: &Path, reclaim_age_millis: i64) -> std::io::Result<Attempt> {
    let Some(content) = read_lock(path)? else {
        return Ok(Attempt::Reclaimed);
    };
    let mut fields = content.split_whitespace();
    let (Some(token), Some(timestamp)) = (fields.next(), fields.next()) else {
        return Ok(Attempt::Busy);
    };
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let Ok(timestamp) = timestamp.parse::<i64>() else {
        return Ok(Attempt::Busy);
    };
    if Timestamp::now().epoch_millis().saturating_sub(timestamp) <= reclaim_age_millis {
        return Ok(Attempt::Busy);
    }
    if fields.next() != Some(REFRESHED_LOCK) && legacy_holder_is_alive(token)? {
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

fn legacy_holder_is_alive(token: &str) -> std::io::Result<bool> {
    let Some((pid, id)) = token.split_once('-') else {
        return Ok(false);
    };
    #[cfg_attr(dylint_lib = "rr_lints", expect(fallback))]
    let Ok(pid) = pid.parse::<i32>() else {
        return Ok(false);
    };
    if pid <= 0 || Uuid::parse_str(id).is_err() {
        return Ok(false);
    }
    let process = Command::new("/bin/ps")
        .args(["-p", &pid.to_string(), "-o", "state="])
        .output()?;
    if process.status.success() {
        return Ok(!process.stdout.trim_ascii_start().starts_with(b"Z"));
    }
    if process.status.code() == Some(1) && process.stderr.is_empty() {
        return Ok(false);
    }
    Err(std::io::Error::other(
        "failed to check legacy wiki lock process",
    ))
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

fn owns_lock(path: &Path, token: &str) -> std::io::Result<bool> {
    Ok(read_lock(path)?
        .as_deref()
        .and_then(|content| content.split_whitespace().next())
        == Some(token))
}

fn release(path: &Path, token: &str) -> std::io::Result<()> {
    if owns_lock(path, token)? {
        match std::fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    const AGE_MILLIS: i64 = 200;
    const INTERVAL: Duration = Duration::from_millis(10);
    const DEADLINE: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn live_holder_renews_the_legacy_timestamp_past_reclaim_age() -> TestResult {
        let root = tempfile::tempdir()?;
        let released = {
            let holder = tokio::time::timeout(
                DEADLINE,
                WriterLock::acquire_with_timing(root.path(), AGE_MILLIS, INTERVAL),
            )
            .await??;
            let initial = timestamp(&holder.path)?;
            let refreshed = wait_for_timestamp(&holder.path, initial + AGE_MILLIS).await?;
            assert!(Timestamp::now().epoch_millis() - refreshed <= AGE_MILLIS);
            assert!(matches!(
                acquire_once(&holder.path, "contender", AGE_MILLIS)?,
                Attempt::Busy
            ));
            holder.release().await
        };
        released?;
        Ok(())
    }

    #[tokio::test]
    async fn dropped_holder_stops_renewing_and_can_be_reclaimed() -> TestResult {
        let root = tempfile::tempdir()?;
        let holder = tokio::time::timeout(
            DEADLINE,
            WriterLock::acquire_with_timing(root.path(), AGE_MILLIS, INTERVAL),
        )
        .await??;
        let path = holder.path.clone();
        let initial = timestamp(&path)?;
        wait_for_timestamp(&path, initial).await?;

        drop(holder);
        let last = timestamp(&path)?;
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                sleep,
                reason = "the abandoned timestamp must age past the reclaim threshold"
            )
        )]
        tokio::time::sleep(Duration::from_millis(250)).await;

        assert_eq!(timestamp(&path)?, last);
        assert!(matches!(
            acquire_once(&path, "contender", AGE_MILLIS)?,
            Attempt::Reclaimed
        ));
        assert!(!path.exists());
        Ok(())
    }

    #[tokio::test]
    async fn release_waits_for_in_flight_refresh_before_removing_lock() -> TestResult {
        let root = tempfile::tempdir()?;
        let (path, continue_refresh, mut release) = {
            let mut holder = tokio::time::timeout(
                DEADLINE,
                WriterLock::acquire_with_timing(root.path(), AGE_MILLIS, REFRESH_INTERVAL),
            )
            .await??;
            let path = holder.path.clone();
            let token = holder.token.clone();
            let refresh_path = path.clone();
            let (started, start) = tokio::sync::oneshot::channel();
            let mut started = Some(started);
            let (continue_refresh, continued) = mpsc::channel();
            holder.refresh = Refresher::start(INTERVAL, move || {
                if let Some(started) = started.take() {
                    started
                        .send(())
                        .map_err(|()| std::io::Error::other("test stopped before refresh"))?;
                    continued
                        .recv_timeout(DEADLINE)
                        .map_err(std::io::Error::other)?;
                    refresh_lock(&refresh_path, &token)?;
                }
                Ok(false)
            })?;
            tokio::time::timeout(DEADLINE, start).await??;

            (path, continue_refresh, Box::pin(holder.release()))
        };
        let pending = tokio::time::timeout(Duration::from_millis(40), &mut release).await;
        continue_refresh.send(())?;
        assert!(pending.is_err());
        tokio::time::timeout(DEADLINE, release).await??;
        assert!(!path.exists());
        Ok(())
    }

    #[test]
    fn live_legacy_process_is_not_reclaimed_after_expiry() -> TestResult {
        let root = tempfile::tempdir()?;
        let path = root.path().join(".wiki-writer.lock");
        let legacy = format!("{}-{} 1\n", std::process::id(), Uuid::new_v4());
        std::fs::write(&path, &legacy)?;
        assert!(matches!(
            acquire_once(&path, "contender", AGE_MILLIS)?,
            Attempt::Busy
        ));
        assert_eq!(std::fs::read_to_string(path)?, legacy);
        Ok(())
    }

    #[tokio::test(start_paused = true)]
    async fn local_waiter_returns_lock_timeout() -> TestResult {
        let root = tempfile::tempdir()?;
        let released = {
            let holder = WriterLock::acquire(root.path()).await?;
            assert!(matches!(
                tokio::time::timeout(ACQUIRE_TIMEOUT + DEADLINE, WriterLock::acquire(root.path()))
                    .await?,
                Err(WikiWriteError::LockTimeout)
            ));
            holder.release().await
        };
        released?;
        Ok(())
    }

    fn timestamp(path: &Path) -> std::io::Result<i64> {
        std::fs::read_to_string(path)?
            .split_whitespace()
            .nth(1)
            .ok_or_else(|| std::io::Error::other("no lock timestamp"))?
            .parse()
            .map_err(std::io::Error::other)
    }

    async fn wait_for_timestamp(
        path: &Path,
        previous: i64,
    ) -> Result<i64, Box<dyn std::error::Error>> {
        let timestamp = tokio::time::timeout(DEADLINE, async {
            let mut poll = tokio::time::interval(INTERVAL);
            loop {
                poll.tick().await;
                let current = timestamp(path)?;
                if current > previous {
                    return Ok::<_, std::io::Error>(current);
                }
            }
        })
        .await??;
        Ok(timestamp)
    }
}
