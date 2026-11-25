use jiff::Timestamp;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Read as _;
use std::os::unix::fs::MetadataExt as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::Path;
use std::path::PathBuf;

use capture::bundle::Sealed;
use capture::sanitize::LocalKey;
use capture::sanitize::LocalReview;
use capture::sanitize::Report;
use capture::sanitize::Restored;
use capture::seal::MAX_BUNDLE_BYTES;
use capture::share::BundlePaths;
use kernel::memory::share::desk::BundleName;
use kernel::memory::share::desk::DeskError;
use kernel::memory::share::desk::Draft;
use kernel::memory::share::desk::ReadyBundle;
use uuid::Uuid;
use wire::capture::FlagMark;
use wire::capture::ReviewStamp;
use wire::capture::Sanitization;
use wire::capture::SessionBundle;

use super::NodeDesk;
use super::drafts::DraftState;
use super::drafts::Phase;

const EXTENSION: &str = ".json";
const SIDECAR_BYTES: usize = 16 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BundleFile {
    path: PathBuf,
    device: u64,
    inode: u64,
    bytes: u64,
    modified: (i64, i64),
}

impl BundleFile {
    fn of(path: PathBuf, metadata: &std::fs::Metadata) -> Self {
        Self {
            path,
            device: metadata.dev(),
            inode: metadata.ino(),
            bytes: metadata.len(),
            modified: (metadata.mtime(), metadata.mtime_nsec()),
        }
    }

    fn remove(self) -> std::io::Result<()> {
        let now = match std::fs::symlink_metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
        if Self::of(self.path.clone(), &now) != self {
            return Ok(());
        }
        std::fs::remove_file(&self.path)
    }
}

#[derive(Debug, thiserror::Error)]
enum Unready {
    #[error("not a regular file of at most the bundle size cap")]
    NotFile,
    #[error("not UTF-8: {0}")]
    Text(#[from] std::str::Utf8Error),
    #[error("not a session bundle: {0}")]
    Unparsed(#[from] capture::bundle::BundleError),
    #[error("a raw bundle")]
    Raw,
    #[error("its review stamp does not add up: {0}")]
    Flags(#[from] capture::sanitize::SanitizeError),
    #[error("it holds too many sessions to count")]
    Count(#[from] std::num::TryFromIntError),
    #[error("its text is not the sanitized bundle its digest covers")]
    Altered,
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl Unready {
    fn not_ready(&self) -> bool {
        !matches!(self, Self::Io(error) if error.kind() != std::io::ErrorKind::NotFound)
    }
}

struct Ready {
    file: BundleFile,
    text: Vec<u8>,
    bundle: SessionBundle,
    sessions: u32,
    open: u32,
    at: Timestamp,
    policy_sha256: String,
    review: Option<ReviewStamp>,
}

fn regular(path: &Path, cap: usize) -> Result<(std::fs::File, std::fs::Metadata), Unready> {
    let opened = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags((nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK).bits())
        .open(path)
        .map_err(|error| {
            if error.raw_os_error().map(nix::errno::Errno::from_raw)
                == Some(nix::errno::Errno::ELOOP)
            {
                Unready::NotFile
            } else {
                Unready::Io(error)
            }
        })?;
    let metadata = opened.metadata()?;
    if !metadata.is_file() || metadata.len() > cap as u64 {
        return Err(Unready::NotFile);
    }
    Ok((opened, metadata))
}

fn capped(opened: std::fs::File, cap: usize) -> Result<Vec<u8>, Unready> {
    let mut text = Vec::new();
    opened.take(cap as u64 + 1).read_to_end(&mut text)?;
    if text.len() > cap {
        return Err(Unready::NotFile);
    }
    Ok(text)
}

fn load(path: PathBuf) -> Result<Ready, Unready> {
    let (opened, metadata) = regular(&path, MAX_BUNDLE_BYTES)?;
    let file = BundleFile::of(path, &metadata);
    let text = capped(opened, MAX_BUNDLE_BYTES)?;
    let bundle = capture::bundle::parse(std::str::from_utf8(&text)?)?;
    let text = text.trim_ascii_end();
    if capture::bundle::canonical_text(&bundle)? != text {
        return Err(Unready::Altered);
    }
    let (at, policy_sha256, review) = match &bundle.manifest.sanitization {
        Sanitization::Sanitized(stamp) => {
            (stamp.at, stamp.policy_sha256.clone(), stamp.review.clone())
        }
        Sanitization::Raw => return Err(Unready::Raw),
    };
    let flags = capture::sanitize::open_flags(&bundle)?;
    Ok(Ready {
        file,
        text: text.to_vec(),
        sessions: u32::try_from(bundle.sessions.len())?,
        open: u32::try_from(flags.withheld + flags.unreviewed)?,
        bundle,
        at,
        policy_sha256,
        review,
    })
}

fn bundle_name(file: &OsStr) -> Option<BundleName> {
    match file
        .to_str()
        .and_then(|name| name.strip_suffix(EXTENSION))
        .map(str::parse::<BundleName>)
    {
        Some(Ok(name)) => Some(name),
        Some(Err(_)) | None => None,
    }
}

fn listed(dir: &Path) -> Result<Vec<ReadyBundle>, DeskError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(super::failed(error)),
    };
    let mut bundles = Vec::new();
    for entry in entries {
        let entry = entry.map_err(super::failed)?;
        let Some(name) = bundle_name(&entry.file_name()) else {
            continue;
        };
        match load(entry.path()) {
            Ok(ready) => bundles.push(ReadyBundle {
                name,
                sessions: ready.sessions,
                bytes: ready.file.bytes,
                open_flags: ready.open,
                created_at: ready.at,
            }),
            Err(unready) if unready.not_ready() => {
                tracing::debug!(%name, %unready, "not listed as a ready bundle");
            }
            Err(unready) => {
                tracing::warn!(%name, %unready, "a bundle in the share folder could not be read");
            }
        }
    }
    bundles.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(bundles)
}

pub(super) async fn list(dir: PathBuf) -> Result<Vec<ReadyBundle>, DeskError> {
    tokio::task::spawn_blocking(move || listed(&dir))
        .await
        .map_err(super::failed)?
}

pub(super) async fn forget(file: Option<BundleFile>) {
    let Some(file) = file else {
        return;
    };
    let path = file.path.clone();
    let removed = tokio::task::spawn_blocking(move || file.remove())
        .await
        .map_err(std::io::Error::other)
        .and_then(|removed| removed);
    if let Err(error) = removed {
        tracing::warn!(path = %path.display(), %error, "the bundle was shared but could not be removed; delete it yourself");
    }
}

fn sidecar(path: &Path) -> Result<Option<Vec<u8>>, Unready> {
    match regular(path, SIDECAR_BYTES) {
        Ok((opened, _)) => Ok(Some(capped(opened, SIDECAR_BYTES)?)),
        Err(Unready::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(unready) => Err(unready),
    }
}

fn reviewed(
    dir: &Path,
    name: &BundleName,
    review: &ReviewStamp,
    at: Timestamp,
    key: &Path,
) -> Result<Option<(LocalReview, Restored)>, DeskError> {
    let paths = BundlePaths::beside(&dir.join(format!("{name}{EXTENSION}")));
    let refused = |error: &dyn std::fmt::Display| {
        DeskError::Refused(format!(
            "the review files beside {name} cannot be used: {error}"
        ))
    };
    let report = sidecar(&paths.report).map_err(|error| refused(&error))?;
    let local = sidecar(&paths.local).map_err(|error| refused(&error))?;
    let (Some(report), Some(local)) = (report, local) else {
        return Ok(None);
    };
    let local: LocalReview = serde_json::from_slice(&local).map_err(|error| refused(&error))?;
    let key = LocalKey::load(key).map_err(|error| refused(&error))?;
    if !local.holds_its_seal(&key.in_format(local.mac_format)) {
        return Err(refused(
            &"the local review file is not sealed under this machine's key",
        ));
    }
    let report = String::from_utf8(report).map_err(|error| refused(&error))?;
    let restored =
        capture::sanitize::restore(review, at, &local, &report).map_err(|error| refused(&error))?;
    Ok(Some((local, restored)))
}

type Opened = (Ready, Option<(LocalReview, Restored)>);

fn opened(dir: &Path, name: &BundleName, key: &Path) -> Result<Opened, DeskError> {
    let ready = load(dir.join(format!("{name}{EXTENSION}"))).map_err(|unready| {
        if unready.not_ready() {
            DeskError::UnknownBundle
        } else {
            super::failed(unready)
        }
    })?;
    let reviewed = match &ready.review {
        Some(review) if !review.flags.is_empty() => reviewed(dir, name, review, ready.at, key)?,
        Some(_) | None => None,
    };
    Ok((ready, reviewed))
}

fn unshown(ready: &Ready) -> Result<usize, DeskError> {
    let shown = ready.review.as_ref().map_or(0, |review| {
        review
            .flags
            .iter()
            .filter(|flag| matches!(flag.mark, FlagMark::Open | FlagMark::Keep))
            .count()
    });
    Ok(usize::try_from(ready.open)
        .map_err(super::failed)?
        .max(shown))
}

impl NodeDesk {
    pub(super) async fn draft_from(&self, name: &BundleName) -> Result<Draft, DeskError> {
        let dir = self.share_dir.clone();
        let key = self.share.key.clone();
        let named = name.clone();
        let (ready, reviewed) = tokio::task::spawn_blocking(move || opened(&dir, &named, &key))
            .await
            .map_err(super::failed)??;
        let (local, decision, resolved) = match reviewed {
            Some((local, restored)) => (Some(local), Some(restored.decision), restored.resolved),
            None => match unshown(&ready)? {
                0 => (None, None, BTreeMap::new()),
                flags => return Err(DeskError::BundleFlags { flags }),
            },
        };
        let conversations = ready
            .bundle
            .sessions
            .iter()
            .map(|session| session.origin.conversation_id.parse::<Uuid>())
            .collect::<Result<Vec<_>, _>>()
            .map_err(super::failed)?;
        self.sweep();
        let reservation = self.drafts.reserve(2 * ready.text.len(), (self.clock)())?;
        let report = Report {
            at: ready.at,
            policy_sha256: ready.policy_sha256,
            bundle_sha256: ready.bundle.manifest.bundle_sha256.clone(),
            sessions: Vec::new(),
            rules: Vec::new(),
            changes: Vec::new(),
            collisions: Vec::new(),
            rechecked: 0,
            decision,
        };
        reservation.commit(
            DraftState {
                phase: Phase::Open,
                sealed: Sealed {
                    bundle: ready.bundle,
                    text: ready.text,
                },
                local,
                report,
                resolved,
                conversations,
                file: Some(ready.file),
            },
            (self.clock)(),
        )
    }
}
