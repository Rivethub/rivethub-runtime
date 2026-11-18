use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Component, Path, PathBuf};

use tempfile::{NamedTempFile, TempDir};

use super::UpdateError;

#[derive(Debug)]
pub(super) struct Installation {
    pub directory: PathBuf,
    pub binary: PathBuf,
    pub staging: TempDir,
    _lock: File,
}

impl Installation {
    pub(super) fn prepare(binary: PathBuf) -> Result<Self, UpdateError> {
        if !binary.is_absolute() || binary.components().any(|part| part == Component::ParentDir) {
            return Err(UpdateError::Path(binary));
        }
        let directory = binary
            .parent()
            .ok_or_else(|| UpdateError::Path(binary.clone()))?
            .canonicalize()?;
        regular(&directory, &binary)?;
        let lock_path = directory.join(".rh-update.lock");
        let lock = lock(&lock_path)?;
        let staging = tempfile::Builder::new()
            .prefix(".rh-update-")
            .tempdir_in(&directory)?;
        Ok(Self {
            directory,
            binary,
            staging,
            _lock: lock,
        })
    }

    pub(super) fn install(&self) -> Result<(), UpdateError> {
        let next = self.staging.path().join("rh");
        let migration = self.directory.join("rivethub-migrate");
        for name in ["rh", "rivethub-migrate"] {
            let path = self.staging.path().join(name);
            regular(self.staging.path(), &path)?;
            File::open(path)?.sync_all()?;
        }
        regular(&self.directory, &self.binary)?;
        backup(&self.binary)?;
        let previous_migration = match migration.symlink_metadata() {
            Ok(_) => {
                regular(&self.directory, &migration)?;
                backup(&migration)?;
                Some(migration.with_extension("previous"))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        std::fs::rename(self.staging.path().join("rivethub-migrate"), &migration)?;
        if let Err(error) = std::fs::rename(next, &self.binary) {
            if let Some(previous) = previous_migration {
                std::fs::rename(previous, &migration)?;
            } else {
                std::fs::remove_file(&migration)?;
            }
            return Err(error.into());
        }
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
}

pub(super) fn lock(lock_path: &Path) -> Result<File, UpdateError> {
    match lock_path.symlink_metadata() {
        Ok(metadata) if !metadata.is_file() => return Err(UpdateError::Path(lock_path.to_owned())),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let lock = OpenOptions::new()
        .custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits())
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Err(UpdateError::Busy),
        Err(std::fs::TryLockError::Error(error)) => return Err(error.into()),
    }
    Ok(lock)
}

pub(super) fn regular(root: &Path, path: &Path) -> Result<(), UpdateError> {
    if path.components().any(|part| part == Component::ParentDir)
        || !config::normalize(path).starts_with(root)
        || !path.canonicalize()?.starts_with(root)
        || !path.symlink_metadata()?.is_file()
    {
        return Err(UpdateError::Path(path.to_owned()));
    }
    Ok(())
}

fn backup(path: &Path) -> Result<(), UpdateError> {
    let parent = path
        .parent()
        .ok_or_else(|| UpdateError::Path(path.to_owned()))?;
    let mut temporary = NamedTempFile::new_in(parent)?;
    std::io::copy(&mut File::open(path)?, &mut temporary)?;
    temporary
        .as_file()
        .set_permissions(std::fs::metadata(path)?.permissions())?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path.with_extension("previous"))
        .map_err(|error| error.error)?;
    File::open(parent)?.sync_all()?;
    Ok(())
}

pub(super) fn write_executable(path: &Path, bytes: &[u8]) -> Result<(), UpdateError> {
    let mut output = OpenOptions::new().create_new(true).write(true).open(path)?;
    output.write_all(bytes)?;
    output.set_permissions(std::fs::Permissions::from_mode(0o755))?;
    output.sync_all()?;
    Ok(())
}
