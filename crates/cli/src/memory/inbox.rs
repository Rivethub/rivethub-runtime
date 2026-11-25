use std::io::Read as _;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;

use capture::seal::DropId;
use capture::seal::ShareServer;

const INBOX_DIR: &str = "inbox";
const CIPHERTEXT: &str = "age";
const RECEIVED: &str = "received";
const RECOVERABLE: &str = "rcv";
const KEEP_FOR: Duration = Duration::from_hours(7 * 24);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Slot(String);

impl Slot {
    pub(super) fn of(server: &ShareServer, id: &DropId) -> Self {
        Self(format!("{}~{id}", server.as_str().replace(':', "_")))
    }
}

#[derive(Debug)]
pub(super) struct Copy {
    pub(super) bytes: Vec<u8>,
    pub(super) path: PathBuf,
}

#[derive(Debug)]
pub(super) struct Held {
    pub(super) first: Copy,
    pub(super) second: Option<Copy>,
}

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

    fn file(&self, slot: &Slot, extension: &str) -> PathBuf {
        self.dir.join(format!("{}.{extension}", slot.0))
    }

    pub(super) fn ciphertext_path(&self, slot: &Slot) -> PathBuf {
        self.file(slot, CIPHERTEXT)
    }

    pub(super) fn was_received(&self, slot: &Slot) -> bool {
        self.file(slot, RECEIVED).symlink_metadata().is_ok()
    }

    pub(super) fn prune(&self, now: SystemTime) -> std::io::Result<usize> {
        let entries = match std::fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(error) => return Err(error),
        };
        let mut removed = 0;
        for entry in entries {
            if expire(&entry?, now)? {
                removed += 1;
            }
        }
        Ok(removed)
    }

    fn sync_dir(&self) -> std::io::Result<()> {
        std::fs::File::open(&self.dir)?.sync_all()
    }

    fn promote(&self, slot: &Slot) -> std::io::Result<()> {
        let recovery = self.file(slot, RECOVERABLE);
        std::fs::hard_link(&recovery, self.ciphertext_path(slot))?;
        self.sync_dir()?;
        remove_if_present(&recovery)?;
        Ok(())
    }

    pub(super) fn promote_recovered(&self, slot: &Slot, opened: PathBuf) -> PathBuf {
        if opened != self.file(slot, RECOVERABLE) {
            return opened;
        }
        if self.promote(slot).is_ok() {
            self.ciphertext_path(slot)
        } else {
            opened
        }
    }

    pub(super) fn read(&self, slot: &Slot, cap: usize) -> std::io::Result<Option<Held>> {
        let mut found = Vec::new();
        let mut failure = None;
        for path in [self.ciphertext_path(slot), self.file(slot, RECOVERABLE)] {
            match read_capped(&path, cap) {
                Ok(Some(bytes)) => found.push(Copy { bytes, path }),
                Ok(None) => {}
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
        }
        let mut found = found.into_iter();
        match (found.next(), failure) {
            (Some(first), _) => Ok(Some(Held {
                first,
                second: found.next(),
            })),
            (None, Some(error)) => Err(error),
            (None, None) => Ok(None),
        }
    }

    pub(super) fn partial(&self) -> std::io::Result<tempfile::NamedTempFile> {
        create_private_dir(&self.dir)?;
        tempfile::Builder::new()
            .prefix(".inbox-")
            .tempfile_in(&self.dir)
    }

    pub(super) fn keep(
        &self,
        slot: &Slot,
        partial: tempfile::NamedTempFile,
    ) -> std::io::Result<()> {
        let persisted = match partial.as_file().sync_all() {
            Ok(()) => partial
                .persist_noclobber(self.ciphertext_path(slot))
                .map_err(|error| (error.error, error.file)),
            Err(error) => Err((error, partial)),
        };
        if let Err((cause, file)) = persisted {
            return Err(self.rescue(slot, file, &cause));
        }
        self.sync_dir()
    }

    fn rescue(
        &self,
        slot: &Slot,
        file: tempfile::NamedTempFile,
        cause: &std::io::Error,
    ) -> std::io::Error {
        let recoverable = self.file(slot, RECOVERABLE);
        let saved = match file.persist_noclobber(&recoverable) {
            Ok(file) => Ok((file, recoverable, None)),
            Err(error) => error
                .file
                .keep()
                .map(|(file, path)| (file, path, Some(recoverable))),
        };
        let (file, saved, misplaced) = match saved {
            Ok(saved) => saved,
            Err(error) => {
                return std::io::Error::other(format!(
                    "could not keep the downloaded drop ({cause}) and could not save it ({error}); it is lost and the sender must share again"
                ));
            }
        };
        let rerun = match misplaced {
            None => "run `rh memory receive` again with the same code to use them without downloading again".to_owned(),
            Some(target) => format!(
                "rename it to {} and run `rh memory receive` again with the same code",
                target.display()
            ),
        };
        let durability = match file.sync_all().and_then(|()| self.sync_dir()) {
            Ok(()) => String::new(),
            Err(error) => format!(
                "; syncing it to disk failed ({error}), so it may be lost if this machine stops before the next receive"
            ),
        };
        std::io::Error::new(
            cause.kind(),
            format!(
                "could not keep the downloaded drop in the inbox ({cause}); the bytes are saved at {}{durability}; {rerun}",
                saved.display()
            ),
        )
    }

    pub(super) fn mark_received(&self, slot: &Slot) -> std::io::Result<()> {
        create_private_dir(&self.dir)?;
        std::fs::File::create(self.file(slot, RECEIVED))?;
        for path in [self.ciphertext_path(slot), self.file(slot, RECOVERABLE)] {
            remove_if_present(&path)?;
        }
        self.sync_dir()
    }
}

fn expire(entry: &std::fs::DirEntry, now: SystemTime) -> std::io::Result<bool> {
    let path = entry.path();
    let ours = path.extension().is_some_and(|extension| {
        extension == CIPHERTEXT || extension == RECEIVED || extension == RECOVERABLE
    });
    if !ours {
        return Ok(false);
    }
    let metadata = match entry.metadata() {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    let stale = metadata
        .modified()?
        .checked_add(KEEP_FOR)
        .is_some_and(|limit| limit < now);
    if metadata.is_file() && stale {
        remove_if_present(&path)
    } else {
        Ok(false)
    }
}

fn remove_if_present(path: &Path) -> std::io::Result<bool> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn read_capped(path: &Path, cap: usize) -> std::io::Result<Option<Vec<u8>>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(nix::fcntl::OFlag::O_NOFOLLOW.bits());
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.take(cap as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > cap {
        return Err(std::io::Error::other(format!(
            "the inbox file {} is larger than any drop",
            path.display()
        )));
    }
    Ok(Some(bytes))
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

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    fn slot() -> Slot {
        slot_on("https://share.example")
    }

    fn slot_on(server: &str) -> Slot {
        Slot::of(
            &server.parse().expect("server"),
            &"Qm9iQm9iQm9iQm9iQm9iQg".parse().expect("id"),
        )
    }

    fn inbox_in(state: &tempfile::TempDir) -> Inbox {
        let inbox = Inbox::in_state(state.path());
        create_private_dir(&inbox.dir).expect("dir");
        inbox
    }

    #[test]
    fn a_failed_keep_saves_the_bytes_and_the_next_read_finds_them() {
        let state = tempfile::tempdir().expect("state");
        let inbox = Inbox::in_state(state.path());
        let slot = slot();
        let mut partial = inbox.partial().expect("partial");
        partial.write_all(b"downloaded").expect("write");
        std::fs::write(inbox.ciphertext_path(&slot), b"squatter").expect("squatter");
        let error = inbox.keep(&slot, partial).expect_err("the slot is taken");
        let message = error.to_string();
        assert!(message.contains(&inbox.file(&slot, RECOVERABLE).display().to_string()));
        assert!(message.contains("rh memory receive"));
        assert_eq!(
            std::fs::read(inbox.file(&slot, RECOVERABLE)).expect("saved"),
            b"downloaded"
        );
        std::fs::remove_file(inbox.ciphertext_path(&slot)).expect("clear");
        let held = inbox.read(&slot, 1024).expect("read").expect("held");
        assert_eq!(held.first.bytes, b"downloaded");
        assert!(held.second.is_none());
        let kept = inbox.promote_recovered(&slot, held.first.path);
        assert_eq!(kept, inbox.ciphertext_path(&slot));
        assert_eq!(std::fs::read(&kept).expect("promoted"), b"downloaded");
        assert!(!inbox.file(&slot, RECOVERABLE).exists());
        inbox.mark_received(&slot).expect("received");
        assert!(!inbox.ciphertext_path(&slot).exists());
    }

    #[test]
    fn an_unreadable_primary_does_not_hide_the_recovery_copy() {
        let state = tempfile::tempdir().expect("state");
        let inbox = inbox_in(&state);
        let slot = slot();
        std::fs::create_dir(inbox.ciphertext_path(&slot)).expect("unreadable primary");
        std::fs::write(inbox.file(&slot, RECOVERABLE), b"recovered").expect("recovery");
        let held = inbox.read(&slot, 1024).expect("read").expect("held");
        assert_eq!(held.first.bytes, b"recovered");
        assert_eq!(held.first.path, inbox.file(&slot, RECOVERABLE));
        assert!(held.second.is_none());
    }

    #[test]
    fn an_unreadable_primary_alone_is_still_an_error() {
        let state = tempfile::tempdir().expect("state");
        let inbox = inbox_in(&state);
        let slot = slot();
        std::fs::create_dir(inbox.ciphertext_path(&slot)).expect("unreadable primary");
        inbox
            .read(&slot, 1024)
            .expect_err("nothing to fall back on");
    }

    #[test]
    fn both_copies_are_offered_primary_first_and_both_stay_on_disk() {
        let state = tempfile::tempdir().expect("state");
        let inbox = inbox_in(&state);
        let slot = slot();
        std::fs::write(inbox.ciphertext_path(&slot), b"primary").expect("primary");
        std::fs::write(inbox.file(&slot, RECOVERABLE), b"recovery").expect("recovery");
        let held = inbox.read(&slot, 1024).expect("read").expect("held");
        assert_eq!(held.first.bytes, b"primary");
        assert_eq!(held.second.expect("second").bytes, b"recovery");
        assert!(inbox.ciphertext_path(&slot).exists());
        assert!(inbox.file(&slot, RECOVERABLE).exists());
    }

    #[test]
    fn promotion_never_overwrites_an_existing_ciphertext() {
        let state = tempfile::tempdir().expect("state");
        let inbox = inbox_in(&state);
        let slot = slot();
        std::fs::write(inbox.ciphertext_path(&slot), b"good").expect("good");
        std::fs::write(inbox.file(&slot, RECOVERABLE), b"candidate").expect("candidate");
        let error = inbox.promote(&slot).expect_err("the destination exists");
        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        let kept = inbox.promote_recovered(&slot, inbox.file(&slot, RECOVERABLE));
        assert_eq!(kept, inbox.file(&slot, RECOVERABLE));
        assert_eq!(
            std::fs::read(inbox.ciphertext_path(&slot)).expect("good"),
            b"good"
        );
        assert_eq!(
            std::fs::read(inbox.file(&slot, RECOVERABLE)).expect("candidate"),
            b"candidate"
        );
    }

    #[test]
    fn the_recovery_name_fits_wherever_the_ciphertext_name_fits() {
        let state = tempfile::tempdir().expect("state");
        let inbox = Inbox::in_state(state.path());
        let host = format!(
            "{}.{}.{}.{}",
            "a".repeat(60),
            "b".repeat(60),
            "c".repeat(60),
            "d".repeat(45)
        );
        let slot = slot_on(&format!("https://{host}"));
        let name = |extension| {
            inbox
                .file(&slot, extension)
                .file_name()
                .expect("name")
                .len()
        };
        assert_eq!(name(CIPHERTEXT), 255);
        assert!(name(RECOVERABLE) <= name(CIPHERTEXT));
        let mut partial = inbox.partial().expect("partial");
        partial.write_all(b"downloaded").expect("write");
        std::fs::write(inbox.ciphertext_path(&slot), b"squatter").expect("squatter");
        let error = inbox.keep(&slot, partial).expect_err("the slot is taken");
        assert!(
            error
                .to_string()
                .contains(&inbox.file(&slot, RECOVERABLE).display().to_string()),
            "{error}"
        );
        assert_eq!(
            std::fs::read(inbox.file(&slot, RECOVERABLE)).expect("saved"),
            b"downloaded"
        );
    }

    #[test]
    fn a_file_another_receive_already_removed_is_not_an_error_to_prune() {
        let state = tempfile::tempdir().expect("state");
        let inbox = inbox_in(&state);
        let slot = slot();
        let path = inbox.ciphertext_path(&slot);
        std::fs::write(&path, b"stale").expect("stale");
        let later = SystemTime::now() + KEEP_FOR + Duration::from_hours(24);
        let entry = std::fs::read_dir(&inbox.dir)
            .expect("dir")
            .next()
            .expect("entry")
            .expect("entry");
        std::fs::remove_file(&path).expect("raced");
        assert!(!expire(&entry, later).expect("a vanished file is skipped"));
        assert!(!remove_if_present(&path).expect("already gone"));
        assert_eq!(inbox.prune(later).expect("prune"), 0);
    }

    #[test]
    fn prune_removes_only_stale_files_it_owns() {
        let state = tempfile::tempdir().expect("state");
        let inbox = inbox_in(&state);
        let slot = slot();
        std::fs::write(inbox.ciphertext_path(&slot), b"old").expect("old");
        std::fs::write(inbox.file(&slot, RECOVERABLE), b"old").expect("old");
        std::fs::write(inbox.dir.join("keep.txt"), b"not ours").expect("other");
        let later = SystemTime::now() + KEEP_FOR + Duration::from_hours(24);
        assert_eq!(inbox.prune(SystemTime::now()).expect("fresh"), 0);
        assert_eq!(inbox.prune(later).expect("stale"), 2);
        assert!(inbox.dir.join("keep.txt").exists());
    }
}
