use jiff::Timestamp;
use std::io::{self, Write as _};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::path::PathBuf;

use serde::Deserialize;
use serde::Serialize;

use super::sequence;
use crate::store_path::single_component;

const DIR: &str = "claude-pending";
const DIR_MODE: u32 = 0o700;
const CLAIMED: &str = ".claimed-";

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Pending {
    pub(crate) key: String,
    pub(crate) prompt: String,
    #[serde_as(as = "wire::time::Rfc3339")]
    pub(crate) at: Timestamp,
    #[serde(default)]
    pub(super) position: Option<sequence::Position>,
}

impl Pending {
    pub(super) fn event_id(&self, session_key: &str) -> String {
        format!("{session_key}:hook:{}", self.key)
    }
}

#[derive(Debug)]
pub(crate) struct PendingPrompts {
    dir: PathBuf,
}

#[derive(Debug, Default)]
pub(crate) struct Claim {
    pub(crate) prompts: Vec<Pending>,
    files: Vec<PathBuf>,
}

impl Claim {
    pub(crate) async fn acknowledge(self) -> std::io::Result<()> {
        for file in self.files {
            match tokio::fs::remove_file(&file).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }
}

struct Locked {
    _file: std::fs::File,
}

impl PendingPrompts {
    pub(crate) fn new(state_dir: &Path) -> Self {
        Self {
            dir: state_dir.join(DIR),
        }
    }

    fn file(&self, native: &str) -> Option<PathBuf> {
        single_component(native).then(|| self.dir.join(format!("{native}.jsonl")))
    }

    async fn lock(&self, native: &str) -> std::io::Result<Locked> {
        tokio::fs::create_dir_all(&self.dir).await?;
        tokio::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(DIR_MODE)).await?;
        let path = self.dir.join(format!("{native}.lock"));
        tokio::task::spawn_blocking(move || {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(path)?;
            file.lock()?;
            Ok(Locked { _file: file })
        })
        .await
        .map_err(std::io::Error::other)?
    }

    fn sequence_file(&self, native: &str) -> PathBuf {
        self.dir.join(format!("{native}.sequence"))
    }

    pub(super) async fn position(
        &self,
        native: &str,
        event_id: String,
    ) -> io::Result<sequence::Position> {
        if !single_component(native) {
            return Err(io::Error::other("invalid session for hook sequence"));
        }
        let locked = self.lock(native).await?;
        let path = self.sequence_file(native);
        tokio::task::spawn_blocking(move || {
            let _locked = locked;
            sequence::position(&path, event_id)
        })
        .await
        .map_err(io::Error::other)?
    }

    pub(super) async fn add(
        &self,
        session: &wire::session_id::SessionId,
        mut pending: Pending,
    ) -> io::Result<()> {
        let native = session.native();
        let Some(file) = self.file(native) else {
            return Ok(());
        };
        let locked = self.lock(native).await?;
        let path = self.sequence_file(native);
        let event_id = pending.event_id(session.as_str());
        tokio::task::spawn_blocking(move || {
            let _locked = locked;
            match sequence::position(&path, event_id) {
                Ok(position) => pending.position = Some(position),
                Err(error) => tracing::warn!(%error, "prompt hook position was not saved"),
            }
            let mut line = serde_json::to_vec(&pending)?;
            line.push(b'\n');
            let mut handle = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(file)?;
            handle.write_all(&line)
        })
        .await
        .map_err(io::Error::other)?
    }

    pub(crate) async fn claim(&self, native: &str) -> std::io::Result<Claim> {
        let Some(file) = self.file(native) else {
            return Ok(Claim::default());
        };
        if !tokio::fs::try_exists(&self.dir).await? {
            return Ok(Claim::default());
        }
        let _locked = self.lock(native).await?;
        if tokio::fs::try_exists(&file).await? {
            let claimed = self
                .dir
                .join(format!("{native}{CLAIMED}{}.jsonl", uuid::Uuid::new_v4()));
            tokio::fs::rename(&file, claimed).await?;
        }
        let prefix = format!("{native}{CLAIMED}");
        let mut files = Vec::new();
        let mut entries = tokio::fs::read_dir(&self.dir).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            let claimed = entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(&prefix));
            if claimed
                && path
                    .extension()
                    .is_some_and(|extension| extension == "jsonl")
            {
                files.push(path);
            }
        }
        files.sort();
        let mut prompts = Vec::new();
        let mut kept = Vec::with_capacity(files.len());
        for file in files {
            let text = match tokio::fs::read_to_string(&file).await {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error),
            };
            for line in text.lines().filter(|line| !line.trim().is_empty()) {
                prompts.push(serde_json::from_str::<Pending>(line)?);
            }
            kept.push(file);
        }
        prompts.sort_by(|left, right| {
            left.at
                .cmp(&right.at)
                .then_with(|| left.key.cmp(&right.key))
        });
        prompts.dedup_by(|left, right| left.key == right.key);
        Ok(Claim {
            prompts,
            files: kept,
        })
    }
}
