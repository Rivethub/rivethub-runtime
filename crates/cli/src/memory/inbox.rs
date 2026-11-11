use std::io::Read as _;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;

use capture::seal::DropId;

const INBOX_DIR: &str = "inbox";
const CIPHERTEXT: &str = "age";
const RECEIVED: &str = "received";
const KEEP_FOR: Duration = Duration::from_hours(7 * 24);

#[derive(Debug, Clone)]
pub(super) struct Inbox {
    dir: PathBuf,
}

impl Inbox {
    pub(super) fn in_state(state_home: &Path) -> Self {
        Self {
            dir: state_home
                .join(capture::sanitize::key::DIR_NAME)
                .join(INBOX_DIR),
        }
    }

    fn file(&self, id: &DropId, extension: &str) -> PathBuf {
        self.dir.join(format!("{id}.{extension}"))
    }

    pub(super) fn ciphertext_path(&self, id: &DropId) -> PathBuf {
        self.file(id, CIPHERTEXT)
    }

    pub(super) fn was_received(&self, id: &DropId) -> bool {
        self.file(id, RECEIVED).symlink_metadata().is_ok()
    }

    pub(super) fn prune(&self, now: SystemTime) -> std::io::Result<usize> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error),
        };
        let mut removed = 0;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let ours = path
                .extension()
                .is_some_and(|extension| extension == CIPHERTEXT || extension == RECEIVED);
            let metadata = entry.metadata()?;
            let stale = metadata
                .modified()?
                .checked_add(KEEP_FOR)
                .is_some_and(|limit| limit < now);
            if ours && metadata.is_file() && stale {
                std::fs::remove_file(&path)?;
                removed += 1;
            }
        }
        Ok(removed)
    }

    pub(super) fn read(&self, id: &DropId, cap: usize) -> std::io::Result<Option<Vec<u8>>> {
        let mut options = std::fs::OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits());
        }
        let file = match options.open(self.ciphertext_path(id)) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let mut bytes = Vec::new();
        file.take(cap as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > cap {
            return Err(std::io::Error::other(format!(
                "the inbox file {} is larger than any drop",
                self.ciphertext_path(id).display()
            )));
        }
        Ok(Some(bytes))
    }

    pub(super) fn partial(&self) -> std::io::Result<tempfile::NamedTempFile> {
        create_private_dir(&self.dir)?;
        tempfile::Builder::new()
            .prefix(".inbox-")
            .tempfile_in(&self.dir)
    }

    pub(super) fn keep(
        &self,
        id: &DropId,
        partial: tempfile::NamedTempFile,
    ) -> std::io::Result<()> {
        partial.as_file().sync_all()?;
        partial
            .persist_noclobber(self.ciphertext_path(id))
            .map_err(|error| error.error)?;
        std::fs::File::open(&self.dir)?.sync_all()
    }

    pub(super) fn mark_received(&self, id: &DropId) -> std::io::Result<()> {
        create_private_dir(&self.dir)?;
        std::fs::File::create(self.file(id, RECEIVED))?;
        std::fs::remove_file(self.ciphertext_path(id))?;
        std::fs::File::open(&self.dir)?.sync_all()
    }
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(dir)
}
