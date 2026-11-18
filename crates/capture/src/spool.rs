use std::io::ErrorKind;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;

use chrono::DateTime;
use chrono::TimeDelta;
use chrono::Utc;
use thiserror::Error;
use tokio::fs;
use uuid::Uuid;
use wire::capture::{CaptureBatchRequest, CaptureResult};

use crate::writer::PostError;

const FILE_MODE: u32 = 0o600;
const DIR_MODE: u32 = 0o700;
const DEAD_DIR: &str = "dead";
const ACK_DIR: &str = "acked";
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
    pub dead: usize,
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

#[derive(Debug, Clone, Copy)]
pub(crate) struct EndChunk {
    pub(crate) bundle: Uuid,
    pub(crate) index: usize,
    pub(crate) count: usize,
}

impl EndChunk {
    fn id(self) -> String {
        format!(
            "end-{}-{:08}-{:08}",
            self.bundle.simple(),
            self.index,
            self.count
        )
    }

    fn parse(id: &str) -> Option<Self> {
        let mut fields = id.strip_prefix("end-")?.split('-');
        let bundle = Uuid::parse_str(fields.next()?).ok()?;
        let index = fields.next()?.parse().ok()?;
        let count = fields.next()?.parse().ok()?;
        (index < count && fields.next().is_none()).then_some(Self {
            bundle,
            index,
            count,
        })
    }

    fn final_chunk(self) -> bool {
        self.index + 1 == self.count
    }

    fn ack_path(self, dir: &Path) -> PathBuf {
        dir.join(ACK_DIR).join(self.id())
    }

    fn rejection_path(self, dir: &Path) -> PathBuf {
        self.ack_path(dir).with_extension("rejected")
    }
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
        write_new(&self.dir, body, now, &Uuid::now_v7().simple().to_string()).await
    }

    pub(crate) async fn spool_end(
        &self,
        body: &str,
        now: DateTime<Utc>,
        chunk: EndChunk,
    ) -> Result<PathBuf, SpoolError> {
        write_new(&self.dir, body, now, &chunk.id()).await
    }

    pub(crate) async fn delivery_body(
        &self,
        path: &Path,
        body: String,
        replay: bool,
    ) -> Result<Option<String>, SpoolError> {
        let chunk = parse_name(path).and_then(|name| EndChunk::parse(&name.id));
        let Some(chunk) = chunk else {
            return if replay {
                suppress_end(body).map(Some)
            } else {
                Ok(Some(body))
            };
        };
        if !chunk.final_chunk() {
            return Ok(Some(body));
        }

        let mut rejected = false;
        let mut dead = None;
        for index in 0..chunk.index {
            let predecessor = EndChunk { index, ..chunk };
            if fs::try_exists(predecessor.rejection_path(&self.dir)).await? {
                rejected = true;
                continue;
            }
            if fs::try_exists(predecessor.ack_path(&self.dir)).await? {
                continue;
            }
            let dead = match &dead {
                Some(entries) => entries,
                None => dead.insert(queued_files(&self.dead_dir()).await?),
            };
            if !dead.iter().any(|entry| entry.name.id == predecessor.id()) {
                return Ok(None);
            }
            rejected = true;
        }
        if rejected {
            suppress_end(body).map(Some)
        } else {
            Ok(Some(body))
        }
    }

    pub(crate) async fn delivered(&self, path: &Path) -> Result<(), SpoolError> {
        let Some(chunk) = parse_name(path).and_then(|name| EndChunk::parse(&name.id)) else {
            return self.discard(path).await;
        };
        if chunk.final_chunk() {
            self.discard(path).await?;
            for index in 0..chunk.index {
                let predecessor = EndChunk { index, ..chunk };
                remove_present(&predecessor.ack_path(&self.dir)).await?;
                remove_present(&predecessor.rejection_path(&self.dir)).await?;
            }
            return Ok(());
        }
        let ack = self.dir.join(ACK_DIR);
        fs::create_dir_all(&ack).await?;
        set_mode(&ack, DIR_MODE).await?;
        match fs::rename(path, chunk.ack_path(&self.dir)).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn discard(&self, path: &Path) -> Result<(), SpoolError> {
        Ok(remove_present(path).await?)
    }

    pub async fn usage(&self) -> Result<Usage, std::io::Error> {
        let live = queued_files(&self.dir).await?;
        let dead = queued_files(&self.dead_dir()).await?;
        let bytes = live.iter().chain(&dead).map(|entry| entry.bytes).sum();
        Ok(Usage {
            entries: live.len() + dead.len(),
            dead: dead.len(),
            bytes,
        })
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
            let Some(body) = self.delivery_body(&entry.path, body, true).await? else {
                continue;
            };
            match post(body).await {
                Ok(_) => self.delivered(&entry.path).await?,
                Err(PostError::Client { status }) => {
                    if let Some(moved) = self.dead_letter(&entry.path).await? {
                        tracing::warn!(
                            "capture server rejected spooled batch {} with HTTP {status}; moved it to {}",
                            entry.path.display(),
                            moved.display()
                        );
                    }
                }
                Err(PostError::Throttled {
                    status,
                    retry_after_seconds,
                }) => {
                    tracing::warn!(
                        "capture server answered HTTP {status} for spooled batch {}; holding it",
                        entry.path.display()
                    );
                    self.hold(&entry.path, now, retry_after_seconds).await?;
                    break;
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

    pub async fn hold(
        &self,
        path: &Path,
        now: DateTime<Utc>,
        retry_after_seconds: Option<u32>,
    ) -> Result<(), SpoolError> {
        let name = parse_name(path).ok_or_else(|| {
            SpoolError::Io(std::io::Error::new(
                ErrorKind::InvalidData,
                "spool file has an unknown name",
            ))
        })?;
        let delay = retry_after_seconds.map_or_else(
            || backoff(1),
            |seconds| TimeDelta::seconds(i64::from(seconds).min(BACKOFF_CEILING_SECONDS)),
        );
        let target = self.dir.join(format!(
            "{}-{}.{}.{}{SUFFIX}",
            name.prefix,
            name.id,
            name.attempts,
            (now + delay).timestamp_millis()
        ));
        match fs::rename(path, &target).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    pub async fn requeue_dead(&self) -> Result<usize, std::io::Error> {
        let mut moved = 0;
        for entry in queued_files(&self.dead_dir()).await? {
            if let Some(chunk) = EndChunk::parse(&entry.name.id)
                && !chunk.final_chunk()
            {
                let ack = self.dir.join(ACK_DIR);
                fs::create_dir_all(&ack).await?;
                set_mode(&ack, DIR_MODE).await?;
                fs::write(chunk.rejection_path(&self.dir), b"").await?;
            }
            let target = self
                .dir
                .join(format!("{}-{}{SUFFIX}", entry.name.prefix, entry.name.id));
            match fs::rename(&entry.path, &target).await {
                Ok(()) => moved += 1,
                Err(error) if error.kind() == ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(moved)
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

async fn write_new(
    dir: &Path,
    body: &str,
    now: DateTime<Utc>,
    id: &str,
) -> Result<PathBuf, SpoolError> {
    fs::create_dir_all(dir).await?;
    set_mode(dir, DIR_MODE).await?;
    let name = format!("{}-{id}{SUFFIX}", now.timestamp_millis());
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

fn suppress_end(body: String) -> Result<String, SpoolError> {
    if body.contains("SessionEnd")
        && let Ok(mut batch) = serde_json::from_str::<CaptureBatchRequest>(&body)
        && batch
            .session_observation()
            .is_some_and(|observation| observation.end_key.is_some())
        && let Some(settings) = &mut batch.settings
    {
        settings
            .insert("summary_end_suppressed", true)
            .map_err(std::io::Error::other)?;
        return serde_json::to_string(&batch).map_err(|error| std::io::Error::other(error).into());
    }
    Ok(body)
}
