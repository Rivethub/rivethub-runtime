use std::fs::OpenOptions;
use std::io::Read as _;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;

use sha2::Digest as _;
use sha2::Sha256;
use thiserror::Error;

const KEY_BYTES: usize = 32;
const BLOCK: usize = 64;
const READ_CAP: u64 = 64;
const OWNER_ONLY: u32 = 0o600;
const SHARED_BITS: u32 = 0o077;
pub const FILE_NAME: &str = "sanitize.key";
pub const DIR_NAME: &str = "rivethub";

#[derive(Debug, Error)]
pub enum KeyError {
    #[error("the local sanitize key {path} cannot be read or created: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "the local sanitize key {path} holds {found} bytes, not {KEY_BYTES}; move it aside to make a new one"
    )]
    Length { path: PathBuf, found: usize },
    #[error("no random bytes for a new local sanitize key: {0}")]
    Random(String),
    #[error("the local sanitize key {path} is not a regular file; move it aside to make a new one")]
    NotRegular { path: PathBuf },
    #[error(
        "the local sanitize key {path} is readable by other users (mode {mode:o}); run chmod 600 on it"
    )]
    Shared { path: PathBuf, mode: u32 },
}

#[derive(Clone)]
pub struct LocalKey([u8; KEY_BYTES]);

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 136: the local sanitize key never leaves the machine; deriving Debug would print its bytes into logs and reports"
    )
)]
impl std::fmt::Debug for LocalKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("LocalKey(..)")
    }
}

impl LocalKey {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; KEY_BYTES]) -> Self {
        Self(bytes)
    }

    pub fn path_in(state_dir: &Path) -> PathBuf {
        state_dir.join(DIR_NAME).join(FILE_NAME)
    }

    pub fn load_or_create(path: &Path) -> Result<Self, KeyError> {
        let io = |source| KeyError::Io {
            path: path.to_path_buf(),
            source,
        };
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        if path.symlink_metadata().is_ok() {
            return Self::read(path);
        }
        let temp = path.with_extension(format!("key.{}.tmp", std::process::id()));
        let mut bytes = [0u8; KEY_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| KeyError::Random(error.to_string()))?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(OWNER_ONLY);
        }
        let written = options.open(&temp).map_err(io).and_then(|mut file| {
            file.write_all(&bytes).map_err(io)?;
            file.sync_all().map_err(io)
        });
        let linked = written.and_then(|()| match std::fs::hard_link(&temp, path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(io(error)),
        });
        if let Err(error) = std::fs::remove_file(&temp) {
            tracing::debug!(
                "temporary sanitize key {} not removed: {error}",
                temp.display()
            );
        }
        if linked? {
            Ok(Self(bytes))
        } else {
            Self::read(path)
        }
    }

    fn read(path: &Path) -> Result<Self, KeyError> {
        let io = |source| KeyError::Io {
            path: path.to_path_buf(),
            source,
        };
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(
                (nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK).bits(),
            );
        }
        let file = options.open(path).map_err(io)?;
        let metadata = file.metadata().map_err(io)?;
        if !metadata.is_file() {
            return Err(KeyError::NotRegular {
                path: path.to_path_buf(),
            });
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = metadata.permissions().mode() & 0o777;
            if mode & SHARED_BITS != 0 {
                return Err(KeyError::Shared {
                    path: path.to_path_buf(),
                    mode,
                });
            }
        }
        let mut found = Vec::new();
        file.take(READ_CAP).read_to_end(&mut found).map_err(io)?;
        let bytes: [u8; KEY_BYTES] = found.as_slice().try_into().map_err(|_| KeyError::Length {
            path: path.to_path_buf(),
            found: found.len(),
        })?;
        Ok(Self(bytes))
    }

    #[must_use]
    pub fn mac(&self, parts: &[&[u8]]) -> [u8; KEY_BYTES] {
        let mut inner = Sha256::new();
        inner.update(self.0.map(|byte| byte ^ 0x36));
        inner.update([0x36u8; BLOCK - KEY_BYTES]);
        for (index, part) in parts.iter().enumerate() {
            if index > 0 {
                inner.update([0]);
            }
            inner.update(part);
        }
        let mut outer = Sha256::new();
        outer.update(self.0.map(|byte| byte ^ 0x5c));
        outer.update([0x5cu8; BLOCK - KEY_BYTES]);
        outer.update(inner.finalize());
        outer.finalize().into()
    }

    #[must_use]
    pub fn hex(&self, parts: &[&[u8]]) -> String {
        hex::encode(self.mac(parts))
    }

    #[must_use]
    pub fn uuid(&self, parts: &[&[u8]]) -> String {
        let mac = self.mac(parts);
        let mut bytes = [0u8; 16];
        bytes.copy_from_slice(&mac[..16]);
        uuid::Builder::from_random_bytes(bytes)
            .into_uuid()
            .hyphenated()
            .to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mac_is_keyed_and_separates_its_parts() {
        let key = LocalKey::from_bytes([0x0b; 32]);
        assert_eq!(key.hex(&[b"a", b"b"]), key.hex(&[b"a\0b"]));
        assert_ne!(key.hex(&[b"a", b"b"]), key.hex(&[b"ab"]));
        assert_ne!(
            key.hex(&[b"x"]),
            LocalKey::from_bytes([0x0c; 32]).hex(&[b"x"])
        );
        assert_eq!(key.hex(&[b"Hi There"]), key.hex(&[b"Hi There"]),);
        let id = key.uuid(&[b"task_id", b"raw"]);
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes().get(14), Some(&b'4'));
    }

    #[test]
    fn a_key_is_created_once_with_owner_only_permissions_and_read_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = LocalKey::path_in(dir.path());
        let first = LocalKey::load_or_create(&path).expect("create");
        let again = LocalKey::load_or_create(&path).expect("read");
        assert_eq!(first.hex(&[b"x"]), again.hex(&[b"x"]));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(&path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        std::fs::write(&path, b"short").expect("write");
        assert!(matches!(
            LocalKey::load_or_create(&path),
            Err(KeyError::Length { found: 5, .. })
        ));
        assert!(
            std::fs::read_dir(path.parent().expect("parent"))
                .expect("dir")
                .count()
                == 1,
            "no temp file stays behind"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_shared_or_irregular_key_file_is_refused() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = LocalKey::path_in(dir.path());
        LocalKey::load_or_create(&path).expect("create");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        assert!(matches!(
            LocalKey::load_or_create(&path),
            Err(KeyError::Shared { mode: 0o644, .. })
        ));
        let dir_key = LocalKey::path_in(&dir.path().join("other"));
        std::fs::create_dir_all(&dir_key).expect("mkdir");
        assert!(matches!(
            LocalKey::load_or_create(&dir_key),
            Err(KeyError::NotRegular { .. })
        ));
    }
}
