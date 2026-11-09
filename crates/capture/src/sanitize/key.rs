use std::io::Read as _;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;

use sha2::Digest as _;
use sha2::Sha256;
use thiserror::Error;

const KEY_BYTES: usize = 32;
const BLOCK: usize = 64;
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
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        match options.open(path) {
            Ok(mut file) => {
                let mut bytes = [0u8; KEY_BYTES];
                getrandom::fill(&mut bytes).map_err(|error| KeyError::Random(error.to_string()))?;
                file.write_all(&bytes).map_err(io)?;
                file.sync_all().map_err(io)?;
                Ok(Self(bytes))
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Self::read(path),
            Err(error) => Err(io(error)),
        }
    }

    fn read(path: &Path) -> Result<Self, KeyError> {
        let io = |source| KeyError::Io {
            path: path.to_path_buf(),
            source,
        };
        let mut found = Vec::new();
        std::fs::File::open(path)
            .map_err(io)?
            .read_to_end(&mut found)
            .map_err(io)?;
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
    }
}
