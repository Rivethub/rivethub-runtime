use std::collections::{HashMap, HashSet};
use std::io::{ErrorKind, Seek as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use chrono::{DateTime, TimeDelta, Utc};
use futures::{Stream, StreamExt as _};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio::io::AsyncWriteExt as _;

use super::{AGE_HEADER, DropId, DropsConfig, OpenError};
use crate::tenancy::UserId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Part {
    Body,
    Sending,
    Record,
    RecordDraft,
}

impl Part {
    const ALL: [Self; 4] = [Self::Body, Self::Sending, Self::Record, Self::RecordDraft];

    const fn suffix(self) -> &'static str {
        match self {
            Self::Body => ".age",
            Self::Sending => ".sending",
            Self::Record => ".json",
            Self::RecordDraft => ".json.draft",
        }
    }

    fn of(name: &str) -> Option<(DropId, Self)> {
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                fallback,
                reason = "PATTERNS FALLBACK: a stated rule, as note 143 of the contract leaves a file whose name is not a drop file alone"
            )
        )]
        let id = name.get(..DropId::LEN)?.parse().ok()?;
        let rest = name.get(DropId::LEN..)?;
        Self::ALL
            .into_iter()
            .find(|part| part.suffix() == rest)
            .map(|part| (id, part))
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    owner: String,
    bytes: u64,
    sha256: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

#[derive(Debug)]
struct Entry {
    owner: UserId,
    bytes: u64,
    expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Fate {
    Send,
    Burn,
}

#[derive(Debug, Default)]
struct Index {
    live: HashMap<DropId, Entry>,
    reserved: HashMap<DropId, (UserId, u64)>,
    claimed: HashMap<DropId, Fate>,
}

impl Index {
    fn holds(&self, id: &DropId) -> bool {
        self.live.contains_key(id) || self.reserved.contains_key(id)
    }

    fn usage(&self, owner: &UserId) -> (u64, u64) {
        let live = self
            .live
            .values()
            .filter(|entry| entry.owner == *owner)
            .map(|entry| entry.bytes);
        let reserved = self
            .reserved
            .values()
            .filter(|(holder, _)| holder == owner)
            .map(|(_, bytes)| *bytes);
        live.chain(reserved)
            .fold((0, 0), |(count, total), bytes| (count + 1, total + bytes))
    }
}

#[derive(Debug, Clone, Copy)]
struct Quota {
    drops: u64,
    bytes: u64,
}

#[derive(Debug)]
pub(crate) struct Store {
    root: PathBuf,
    ttl: TimeDelta,
    quota: Quota,
    index: Mutex<Index>,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum ReserveError {
    #[error("drop quota exceeded")]
    Quota,
    #[error("the random source failed")]
    Random(#[from] std::io::Error),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum Refused {
    #[error("body is not an age file")]
    NotAge,
    #[error("payload too large")]
    TooLarge,
    #[error("the upload stream broke")]
    Aborted,
    #[error("the upload did not finish within its deadline")]
    Deadline,
    #[error("drop file operation failed")]
    Io(#[from] std::io::Error),
}

#[derive(Debug)]
pub(crate) struct Stored {
    pub(crate) id: DropId,
    pub(crate) bytes: u64,
    pub(crate) expires_at: DateTime<Utc>,
}

#[derive(Debug)]
pub(crate) enum Claim {
    Gone,
    Failed,
    Ready { file: std::fs::File, bytes: u64 },
}

#[derive(Debug)]
pub(crate) struct Reservation<'a> {
    store: &'a Store,
    id: DropId,
    owner: UserId,
    bytes: u64,
    committed: bool,
}

impl Drop for Reservation<'_> {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        remove(&self.store.path(&self.id, Part::Body), "unlink");
        remove(&self.store.path(&self.id, Part::RecordDraft), "unlink");
        remove(&self.store.path(&self.id, Part::Record), "unlink");
        self.store.index.lock().reserved.remove(&self.id);
    }
}

fn logged(op: &'static str, error: &std::io::Error) {
    tracing::warn!(op, kind = ?error.kind(), "drop file operation failed");
}

fn unlink(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn remove(path: &Path, op: &'static str) {
    if let Err(error) = unlink(path) {
        logged(op, &error);
    }
}

fn nofollow() -> i32 {
    nix::fcntl::OFlag::O_NOFOLLOW.bits()
}

fn digest(file: &mut std::fs::File) -> std::io::Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let total = std::io::copy(file, &mut hasher)?;
    Ok((total, hex::encode(hasher.finalize())))
}

impl Store {
    pub(crate) fn open(config: &DropsConfig) -> Result<Self, OpenError> {
        let root = &config.root;
        let failed = |source: std::io::Error| OpenError::Io {
            path: root.clone(),
            source,
        };
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(root)
            .map_err(failed)?;
        let meta = std::fs::symlink_metadata(root).map_err(failed)?;
        let unsafe_root = |rule: &'static str| OpenError::Unsafe {
            path: root.clone(),
            rule,
        };
        if !meta.file_type().is_dir() {
            return Err(unsafe_root(
                "must be a real directory, not a symlink or a file",
            ));
        }
        if meta.uid() != nix::unistd::geteuid().as_raw() {
            return Err(unsafe_root("must be owned by the user rh runs as"));
        }
        if meta.mode() & 0o077 != 0 {
            return Err(unsafe_root(
                "must give no access to group or others (mode 0700)",
            ));
        }
        let store = Self {
            root: root.clone(),
            ttl: config.ttl,
            quota: Quota {
                drops: u64::from(config.per_user_drops),
                bytes: config.per_user_bytes,
            },
            index: Mutex::new(Index::default()),
        };
        store.recover().map_err(failed)?;
        Ok(store)
    }

    fn path(&self, id: &DropId, part: Part) -> PathBuf {
        self.root.join(format!("{}{}", id.as_str(), part.suffix()))
    }

    fn parts(&self) -> std::io::Result<Vec<(DropId, Part)>> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(&self.root)? {
            let entry = entry?;
            if let Some(named) = entry.file_name().to_str().and_then(Part::of) {
                found.push(named);
            }
        }
        Ok(found)
    }

    fn recover(&self) -> std::io::Result<()> {
        let parts: HashSet<(DropId, Part)> = self.parts()?.into_iter().collect();
        let has = |id: &DropId, part: Part| parts.contains(&(id.clone(), part));
        let mut index = self.index.lock();
        for (id, _) in parts.iter().filter(|(_, part)| *part == Part::Record) {
            if !has(id, Part::Body) {
                if !has(id, Part::Sending) {
                    remove(&self.path(id, Part::Record), "unlink");
                    continue;
                }
                std::fs::rename(self.path(id, Part::Sending), self.path(id, Part::Body))?;
            }
            match self.record(id) {
                Ok(Some((owner, record))) => {
                    index.live.insert(
                        id.clone(),
                        Entry {
                            owner,
                            bytes: record.bytes,
                            expires_at: record.expires_at,
                        },
                    );
                }
                Ok(None) => {}
                Err(error) => logged("index", &error),
            }
        }
        drop(index);
        Ok(())
    }

    fn record(&self, id: &DropId) -> std::io::Result<Option<(UserId, Record)>> {
        let text = match std::fs::read(self.path(id, Part::Record)) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let record: Record = serde_json::from_slice(&text).map_err(std::io::Error::other)?;
        let owner = UserId::new(record.owner.clone())
            .map_err(|_| std::io::Error::new(ErrorKind::InvalidData, "drop owner"))?;
        Ok(Some((owner, record)))
    }

    pub(crate) fn reserve(
        &self,
        owner: &UserId,
        bytes: u64,
    ) -> Result<Reservation<'_>, ReserveError> {
        let mut index = self.index.lock();
        let (count, total) = index.usage(owner);
        if count >= self.quota.drops || total.saturating_add(bytes) > self.quota.bytes {
            return Err(ReserveError::Quota);
        }
        let id = loop {
            let id = DropId::generate()?;
            if !index.holds(&id) {
                break id;
            }
        };
        index.reserved.insert(id.clone(), (owner.clone(), bytes));
        drop(index);
        Ok(Reservation {
            store: self,
            id,
            owner: owner.clone(),
            bytes,
            committed: false,
        })
    }

    pub(crate) async fn receive<S>(
        &self,
        mut reservation: Reservation<'_>,
        mut body: S,
    ) -> Result<Stored, Refused>
    where
        S: Stream<Item = Result<Bytes, axum::Error>> + Unpin,
    {
        let id = reservation.id.clone();
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(nofollow())
            .open(self.path(&id, Part::Body))
            .await?;
        let mut hasher = Sha256::new();
        let mut total: u64 = 0;
        let mut header: usize = 0;
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|_| Refused::Aborted)?;
            total += chunk.len() as u64;
            if total > reservation.bytes {
                return Err(Refused::TooLarge);
            }
            let expected = AGE_HEADER.iter().skip(header);
            let compared = chunk
                .iter()
                .zip(expected)
                .take_while(|(got, want)| got == want);
            let matched = compared.count();
            if header + matched < AGE_HEADER.len() && matched < chunk.len() {
                return Err(Refused::NotAge);
            }
            header += matched;
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
        }
        if header < AGE_HEADER.len() {
            return Err(Refused::NotAge);
        }
        file.sync_all().await?;
        drop(file);
        let created_at = Utc::now();
        let expires_at = created_at + self.ttl;
        let record = Record {
            owner: reservation.owner.as_str().to_owned(),
            bytes: total,
            sha256: hex::encode(hasher.finalize()),
            created_at,
            expires_at,
        };
        let text = serde_json::to_vec(&record).map_err(std::io::Error::other)?;
        let mut draft = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(nofollow())
            .open(self.path(&id, Part::RecordDraft))
            .await?;
        draft.write_all(&text).await?;
        draft.sync_all().await?;
        drop(draft);
        tokio::fs::rename(
            self.path(&id, Part::RecordDraft),
            self.path(&id, Part::Record),
        )
        .await?;
        tokio::fs::File::open(&self.root).await?.sync_all().await?;
        let mut index = self.index.lock();
        index.reserved.remove(&id);
        index.live.insert(
            id.clone(),
            Entry {
                owner: reservation.owner.clone(),
                bytes: total,
                expires_at,
            },
        );
        drop(index);
        reservation.committed = true;
        Ok(Stored {
            id,
            bytes: total,
            expires_at,
        })
    }

    fn take_locked(&self, index: &mut Index, id: &DropId) -> std::io::Result<bool> {
        match std::fs::rename(self.path(id, Part::Body), self.path(id, Part::Sending)) {
            Ok(()) => {
                index.claimed.insert(id.clone(), Fate::Send);
                Ok(true)
            }
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error),
        }
    }

    fn take(&self, id: &DropId) -> std::io::Result<bool> {
        self.take_locked(&mut self.index.lock(), id)
    }

    fn forget(&self, id: &DropId) -> std::io::Result<Option<Fate>> {
        unlink(&self.path(id, Part::Sending))?;
        unlink(&self.path(id, Part::Record))?;
        let mut index = self.index.lock();
        index.live.remove(id);
        Ok(index.claimed.remove(id))
    }

    fn sync_root(&self) -> std::io::Result<()> {
        std::fs::File::open(&self.root)?.sync_all()
    }

    fn put_back(&self, id: &DropId) {
        let mut index = self.index.lock();
        if index.claimed.get(id) == Some(&Fate::Burn) {
            drop(index);
            if let Err(error) = self.forget(id) {
                logged("unlink", &error);
            }
            return;
        }
        index.claimed.remove(id);
        if let Err(error) = std::fs::rename(self.path(id, Part::Sending), self.path(id, Part::Body))
        {
            logged("release", &error);
        }
        drop(index);
    }

    pub(crate) fn claim(&self, id: &DropId) -> Claim {
        let record = match self.record(id) {
            Ok(Some((_, record))) => record,
            Ok(None) => return Claim::Gone,
            Err(error) => {
                logged("read record", &error);
                return Claim::Gone;
            }
        };
        if record.expires_at <= Utc::now() {
            self.discard(id);
            return Claim::Gone;
        }
        match self.take(id) {
            Ok(true) => {}
            Ok(false) => return Claim::Gone,
            Err(error) => {
                logged("claim", &error);
                return Claim::Failed;
            }
        }
        let opened = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(nofollow())
            .open(self.path(id, Part::Sending));
        let mut file = match opened {
            Ok(file) => file,
            Err(error) => {
                logged("open", &error);
                self.put_back(id);
                return Claim::Failed;
            }
        };
        let checked = digest(&mut file).and_then(|sums| file.rewind().map(|()| sums));
        match checked {
            Ok((bytes, sha256)) if bytes == record.bytes && sha256 == record.sha256 => {
                let fate = match self.forget(id) {
                    Ok(fate) => fate,
                    Err(error) => {
                        logged("unlink", &error);
                        self.put_back(id);
                        return Claim::Failed;
                    }
                };
                if let Err(error) = self.sync_root() {
                    logged("sync", &error);
                    return Claim::Failed;
                }
                match fate {
                    Some(Fate::Send) => Claim::Ready { file, bytes },
                    Some(Fate::Burn) | None => Claim::Gone,
                }
            }
            Ok(_) => {
                tracing::warn!(op = "integrity", "drop file does not match its record");
                if let Err(error) = self.forget(id) {
                    logged("unlink", &error);
                }
                Claim::Failed
            }
            Err(error) => {
                logged("read", &error);
                self.put_back(id);
                Claim::Failed
            }
        }
    }

    pub(crate) fn revoke(&self, id: &DropId, owner: &UserId) -> std::io::Result<bool> {
        match self.record(id)? {
            Some((holder, _)) if holder == *owner => {}
            Some(_) | None => return Ok(false),
        }
        let mut index = self.index.lock();
        if self.take_locked(&mut index, id)? {
            drop(index);
            if let Err(error) = self.forget(id) {
                self.put_back(id);
                return Err(error);
            }
            self.sync_root()?;
            return Ok(true);
        }
        let Some(fate) = index.claimed.get_mut(id) else {
            return Ok(false);
        };
        *fate = Fate::Burn;
        drop(index);
        Ok(true)
    }

    fn discard(&self, id: &DropId) -> bool {
        match self.take(id) {
            Ok(true) => match self.forget(id) {
                Ok(_) => true,
                Err(error) => {
                    logged("expire", &error);
                    self.put_back(id);
                    false
                }
            },
            Ok(false) => false,
            Err(error) => {
                logged("expire", &error);
                false
            }
        }
    }

    pub(crate) fn sweep(&self, orphan_age: Duration) -> std::io::Result<usize> {
        let now = Utc::now();
        let expired: Vec<DropId> = self
            .index
            .lock()
            .live
            .iter()
            .filter(|(_, entry)| entry.expires_at <= now)
            .map(|(id, _)| id.clone())
            .collect();
        let discarded = expired.iter().filter(|id| self.discard(id)).count();
        let cutoff = SystemTime::now()
            .checked_sub(orphan_age)
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let mut orphans = 0;
        for (id, part) in self.parts()? {
            if self.index.lock().holds(&id) {
                continue;
            }
            let path = self.path(&id, part);
            let old = std::fs::symlink_metadata(&path)
                .and_then(|meta| meta.modified())
                .is_ok_and(|modified| modified <= cutoff);
            if old {
                remove(&path, "sweep");
                orphans += 1;
            }
        }
        Ok(discarded + orphans)
    }
}

#[cfg(test)]
mod tests;
