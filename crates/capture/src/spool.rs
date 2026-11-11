use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;

use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use thiserror::Error;
use tokio::fs;
use wire::capture::CaptureResult;

use crate::writer::PostError;

const FILE_MODE: u32 = 0o600;
const DIR_MODE: u32 = 0o700;
const DEAD_DIR: &str = "dead";
const SUFFIX: &str = ".json";
pub const MAX_TRANSIENT_ATTEMPTS: u64 = 8;
const BACKOFF_BASE_SECONDS: i64 = 30;
const BACKOFF_CEILING_SECONDS: i64 = 3600;
const WARNED_MARKER: &str = "overflow-warned";
pub const WARN_ENTRIES: usize = 1000;
pub const WARN_BYTES: u64 = 100 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum SpoolError {
    #[error("capture spool failed: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Error)]
pub enum ReplayError {
    #[error("capture spool replay failed: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Spool(#[from] SpoolError),
}

#[derive(Debug)]
pub struct CaptureSpool {
    dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Usage {
    pub entries: usize,
    pub bytes: u64,
}

impl Usage {
    #[must_use]
    pub fn over_threshold(self) -> bool {
        self.entries > WARN_ENTRIES || self.bytes > WARN_BYTES
    }
}

#[derive(Debug)]
struct Name {
    prefix: u64,
    id: String,
    attempts: u64,
    not_before: u64,
}

#[derive(Debug)]
struct Entry {
    path: PathBuf,
    name: Name,
    bytes: u64,
}

impl CaptureSpool {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn dead_dir(&self) -> PathBuf {
        self.dir.join(DEAD_DIR)
    }

    pub async fn spool(&self, body: &str, now: DateTime<Utc>) -> Result<PathBuf, SpoolError> {
        write_new(&self.dir, body, now).await
    }

    pub async fn discard(&self, path: &Path) -> Result<(), SpoolError> {
        Ok(remove_present(path).await?)
    }

    pub async fn usage(&self) -> Result<Usage, std::io::Error> {
        let mut entries = 0;
        let mut bytes = 0;
        for dir in [self.dir.clone(), self.dead_dir()] {
            let found = queued_files(&dir).await?;
            entries += found.len();
            bytes += found.iter().map(|entry| entry.bytes).sum::<u64>();
        }
        Ok(Usage { entries, bytes })
    }

    pub async fn crossed_threshold(&self) -> Result<Option<Usage>, std::io::Error> {
        let usage = self.usage().await?;
        let marker = self.dir.join(WARNED_MARKER);
        let warned = fs::try_exists(&marker).await?;
        if !usage.over_threshold() {
            if warned {
                remove_present(&marker).await?;
            }
            return Ok(None);
        }
        if warned {
            return Ok(None);
        }
        fs::write(&marker, b"").await?;
        Ok(Some(usage))
    }

    pub async fn replay<F, Future_>(
        &self,
        max: usize,
        now: DateTime<Utc>,
        post: F,
    ) -> Result<(), ReplayError>
    where
        F: Fn(String) -> Future_,
        Future_: std::future::Future<Output = Result<CaptureResult, PostError>>,
    {
        self.replay_except(max, now, &[], post).await
    }

    pub async fn replay_except<F, Future_>(
        &self,
        max: usize,
        now: DateTime<Utc>,
        skip: &[PathBuf],
        post: F,
    ) -> Result<(), ReplayError>
    where
        F: Fn(String) -> Future_,
        Future_: std::future::Future<Output = Result<CaptureResult, PostError>>,
    {
        let mut queued = queued_files(&self.dir).await?;
        queued.sort_by(|a, b| (a.name.prefix, &a.name.id).cmp(&(b.name.prefix, &b.name.id)));
        let now_millis = now.timestamp_millis().unsigned_abs();
        let due = queued
            .into_iter()
            .filter(|entry| entry.name.not_before <= now_millis && !skip.contains(&entry.path))
            .take(max);
        for entry in due {
            let body = match fs::read_to_string(&entry.path).await {
                Ok(body) => body,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => {
                    if let Some(moved) = self.dead_letter(&entry.path).await? {
                        tracing::warn!(
                            "spooled capture batch {} is unreadable ({error}); moved it to {}",
                            entry.path.display(),
                            moved.display()
                        );
                    }
                    continue;
                }
            };
            match post(body).await {
                Ok(_) => remove_present(&entry.path).await?,
                Err(PostError::Client { status }) => {
                    if let Some(moved) = self.dead_letter(&entry.path).await? {
                        tracing::warn!(
                            "capture server rejected spooled batch {} with HTTP {status}; moved it to {}",
                            entry.path.display(),
                            moved.display()
                        );
                    }
                }
                Err(error @ (PostError::Unauthorized | PostError::Refused { .. })) => {
                    tracing::warn!(
                        "spooled capture batch {} kept: {error}",
                        entry.path.display()
                    );
                    break;
                }
                Err(error) => {
                    self.defer(&entry, now, &error).await?;
                    break;
                }
            }
        }
        Ok(())
    }

    async fn defer(
        &self,
        entry: &Entry,
        now: DateTime<Utc>,
        error: &PostError,
    ) -> Result<(), SpoolError> {
        let attempts = entry.name.attempts.saturating_add(1);
        if attempts >= MAX_TRANSIENT_ATTEMPTS {
            if let Some(moved) = self.dead_letter(&entry.path).await? {
                tracing::warn!(
                    "spooled capture batch {} failed {attempts} times ({error}); moved it to {}",
                    entry.path.display(),
                    moved.display()
                );
            }
            return Ok(());
        }
        let not_before = now + backoff(attempts);
        let target = self.dir.join(format!(
            "{}-{}.{attempts}.{}{SUFFIX}",
            entry.name.prefix,
            entry.name.id,
            not_before.timestamp_millis()
        ));
        match fs::rename(&entry.path, &target).await {
            Ok(()) => {
                tracing::warn!(
                    "spooled capture batch {} failed ({error}); attempt {attempts} of {MAX_TRANSIENT_ATTEMPTS}, next try after {not_before}",
                    entry.path.display()
                );
                Ok(())
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn dead_letter(&self, path: &Path) -> Result<Option<PathBuf>, SpoolError> {
        let name = path.file_name().ok_or_else(|| {
            SpoolError::Io(std::io::Error::new(
                ErrorKind::InvalidData,
                "spool file has no name",
            ))
        })?;
        let dead = self.dead_dir();
        fs::create_dir_all(&dead).await?;
        set_mode(&dead, DIR_MODE).await?;
        let target = dead.join(name);
        match fs::rename(path, &target).await {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        }
        Ok(Some(target))
    }
}

fn backoff(attempts: u64) -> TimeDelta {
    let doublings = attempts.saturating_sub(1).min(16);
    let seconds = BACKOFF_BASE_SECONDS
        .saturating_mul(1_i64 << doublings)
        .min(BACKOFF_CEILING_SECONDS);
    TimeDelta::seconds(seconds)
}

async fn write_new(dir: &Path, body: &str, now: DateTime<Utc>) -> Result<PathBuf, SpoolError> {
    fs::create_dir_all(dir).await?;
    set_mode(dir, DIR_MODE).await?;
    let name = format!(
        "{}-{}{SUFFIX}",
        now.timestamp_millis(),
        uuid::Uuid::now_v7().simple()
    );
    let target = dir.join(&name);
    let temporary = dir.join(format!("{name}.tmp"));
    fs::write(&temporary, body).await?;
    set_mode(&temporary, FILE_MODE).await?;
    fs::rename(&temporary, &target).await?;
    Ok(target)
}

async fn remove_present(path: &Path) -> Result<(), std::io::Error> {
    match fs::remove_file(path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

async fn queued_files(dir: &Path) -> Result<Vec<Entry>, std::io::Error> {
    let mut queued = Vec::new();
    let mut entries = match fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(queued),
        Err(error) => return Err(error),
    };
    while let Some(found) = entries.next_entry().await? {
        let path = found.path();
        let Some(name) = parse_name(&path) else {
            continue;
        };
        match fs::metadata(&path).await {
            Ok(metadata) if metadata.is_file() => queued.push(Entry {
                path,
                name,
                bytes: metadata.len(),
            }),
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(queued)
}

fn parse_name(path: &Path) -> Option<Name> {
    let name = path.file_name()?.to_str()?;
    let stem = name.strip_suffix(SUFFIX)?;
    let (digits, rest) = stem.split_once('-')?;
    let prefix = digits.parse::<u64>().ok()?;
    let mut parts = rest.split('.');
    let id = parts.next().filter(|id| !id.is_empty())?;
    let (attempts, not_before) = match (parts.next(), parts.next(), parts.next()) {
        (None, None, None) => (0, 0),
        (Some(attempts), Some(not_before), None) => (decimal(attempts)?, decimal(not_before)?),
        _ => return None,
    };
    Some(Name {
        prefix,
        id: id.to_owned(),
        attempts,
        not_before,
    })
}

fn decimal(text: &str) -> Option<u64> {
    if text.is_empty() {
        return None;
    }
    text.bytes().try_fold(0_u64, |value, byte| {
        let digit = u64::from(byte.checked_sub(b'0').filter(|digit| *digit <= 9)?);
        value.checked_mul(10)?.checked_add(digit)
    })
}

async fn set_mode(path: &Path, mode: u32) -> Result<(), std::io::Error> {
    fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await
}
