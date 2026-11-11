use std::fs::OpenOptions;
use std::io::Read as _;
use std::io::Write as _;
use std::path::Path;
use std::path::PathBuf;

use hmac::Hmac;
use hmac::Mac as _;
use serde::Deserialize;
use serde::Serialize;
use sha2::Sha256;
use thiserror::Error;

const KEY_BYTES: usize = 32;
const V2_DOMAIN: &[u8] = b"rivethub sanitize mac v2";
const READ_CAP: u64 = 64;
const OWNER_ONLY: u32 = 0o600;
const SHARED_BITS: u32 = 0o077;
pub const FILE_NAME: &str = "sanitize.key";
pub const DIR_NAME: &str = "rivethub";

#[derive(Debug, Error)]
pub enum KeyError {
    #[error("the {name} {path} cannot be read or created: {source}")]
    Io {
        name: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(
        "the {name} {path} holds {found} bytes, not {KEY_BYTES}; move it aside to make a new one"
    )]
    Length {
        name: &'static str,
        path: PathBuf,
        found: usize,
    },
    #[error("no random bytes for a new {name}: {reason}")]
    Random { name: &'static str, reason: String },
    #[error("the {name} {path} is not a regular file; move it aside to make a new one")]
    NotRegular { name: &'static str, path: PathBuf },
    #[error("the {name} {path} is readable by other users (mode {mode:o}); run chmod 600 on it")]
    Shared {
        name: &'static str,
        path: PathBuf,
        mode: u32,
    },
}

const NAME: &str = "local sanitize key";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MacFormat {
    V1,
    V2,
}

#[derive(Clone)]
pub struct LocalKey {
    bytes: [u8; KEY_BYTES],
    format: MacFormat,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 136: the local sanitize key never leaves the machine; deriving Debug would print its bytes into logs and reports"
    )
)]
impl std::fmt::Debug for LocalKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "LocalKey(.., {:?})", self.format)
    }
}

impl LocalKey {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; KEY_BYTES]) -> Self {
        Self {
            bytes,
            format: MacFormat::V2,
        }
    }

    #[must_use]
    pub fn in_format(&self, format: MacFormat) -> Self {
        Self {
            format,
            ..self.clone()
        }
    }

    #[must_use]
    pub const fn format(&self) -> MacFormat {
        self.format
    }

    pub fn path_in(state_dir: &Path) -> PathBuf {
        state_dir.join(DIR_NAME).join(FILE_NAME)
    }

    pub fn load_or_create(path: &Path) -> Result<Self, KeyError> {
        let found = PrivateFile::new(path, NAME, READ_CAP).load_or_create(|| {
            let mut bytes = [0u8; KEY_BYTES];
            getrandom::fill(&mut bytes).map_err(|error| KeyError::Random {
                name: NAME,
                reason: error.to_string(),
            })?;
            Ok(bytes.to_vec())
        })?;
        let bytes: [u8; KEY_BYTES] = found.as_slice().try_into().map_err(|_| KeyError::Length {
            name: NAME,
            path: path.to_path_buf(),
            found: found.len(),
        })?;
        Ok(Self::from_bytes(bytes))
    }

    fn hmac(&self, parts: &[&[u8]]) -> Hmac<Sha256> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.bytes)
            .unwrap_or_else(|error| unreachable!("HMAC takes a key of any length: {error}"));
        match self.format {
            MacFormat::V1 => {
                for (index, part) in parts.iter().enumerate() {
                    if index > 0 {
                        mac.update(&[0]);
                    }
                    mac.update(part);
                }
            }
            MacFormat::V2 => {
                for part in std::iter::once(V2_DOMAIN).chain(parts.iter().copied()) {
                    mac.update(&(part.len() as u64).to_be_bytes());
                    mac.update(part);
                }
            }
        }
        mac
    }

    #[must_use]
    pub fn mac(&self, parts: &[&[u8]]) -> [u8; KEY_BYTES] {
        self.hmac(parts).finalize().into_bytes().into()
    }

    #[must_use]
    pub fn verifies(&self, parts: &[&[u8]], hex_tag: &str) -> bool {
        hex_tag.len() == 2 * KEY_BYTES
            && hex_tag
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
            && hex::decode(hex_tag).is_ok_and(|tag| self.hmac(parts).verify_slice(&tag).is_ok())
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

#[derive(Debug, Clone, Copy)]
pub struct PrivateFile<'a> {
    path: &'a Path,
    name: &'static str,
    cap: u64,
}

impl<'a> PrivateFile<'a> {
    pub const fn new(path: &'a Path, name: &'static str, cap: u64) -> Self {
        Self { path, name, cap }
    }

    fn io(&self, source: std::io::Error) -> KeyError {
        KeyError::Io {
            name: self.name,
            path: self.path.to_path_buf(),
            source,
        }
    }

    pub fn load_or_create(
        &self,
        fresh: impl FnOnce() -> Result<Vec<u8>, KeyError>,
    ) -> Result<Vec<u8>, KeyError> {
        let path = self.path;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| self.io(error))?;
        }
        if path.symlink_metadata().is_ok() {
            return self.read();
        }
        let bytes = fresh()?;
        let temp = path.with_extension(format!("key.{}.tmp", std::process::id()));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(OWNER_ONLY);
        }
        let written = options
            .open(&temp)
            .map_err(|error| self.io(error))
            .and_then(|mut file| {
                file.write_all(&bytes).map_err(|error| self.io(error))?;
                file.sync_all().map_err(|error| self.io(error))
            });
        let linked = written.and_then(|()| match std::fs::hard_link(&temp, path) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(self.io(error)),
        });
        if let Err(error) = std::fs::remove_file(&temp) {
            tracing::debug!(
                "temporary {} {} not removed: {error}",
                self.name,
                temp.display()
            );
        }
        if linked? { Ok(bytes) } else { self.read() }
    }

    pub fn read(&self) -> Result<Vec<u8>, KeyError> {
        let path = self.path;
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.custom_flags(
                (nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK).bits(),
            );
        }
        let file = options.open(path).map_err(|error| self.io(error))?;
        let metadata = file.metadata().map_err(|error| self.io(error))?;
        if !metadata.is_file() {
            return Err(KeyError::NotRegular {
                name: self.name,
                path: path.to_path_buf(),
            });
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = metadata.permissions().mode() & 0o777;
            if mode & SHARED_BITS != 0 {
                return Err(KeyError::Shared {
                    name: self.name,
                    path: path.to_path_buf(),
                    mode,
                });
            }
        }
        let mut found = Vec::new();
        file.take(self.cap)
            .read_to_end(&mut found)
            .map_err(|error| self.io(error))?;
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EVENT: [&[u8]; 5] = [b"event_id", b"desk", b"c1", b"claude-code:c1", b"evt-1"];

    fn padded(key: &[u8]) -> LocalKey {
        let mut bytes = [0u8; KEY_BYTES];
        for (slot, byte) in bytes.iter_mut().zip(key) {
            *slot = *byte;
        }
        LocalKey::from_bytes(bytes)
    }

    #[test]
    fn the_v1_mac_is_hmac_sha256_per_rfc_4231() {
        let cases: [(Vec<u8>, Vec<u8>, &str); 4] = [
            (
                vec![0x0b; 20],
                b"Hi There".to_vec(),
                "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7",
            ),
            (
                b"Jefe".to_vec(),
                b"what do ya want for nothing?".to_vec(),
                "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843",
            ),
            (
                vec![0xaa; 20],
                vec![0xdd; 50],
                "773ea91e36800e46854db8ebd09181a72959098b3ef8c122d9635514ced565fe",
            ),
            (
                (1..=25).collect(),
                vec![0xcd; 50],
                "82558a389a443c0ea4cc819899f2083a85f0faa3e578f8077a2e3ff46729665b",
            ),
        ];
        for (key, data, expected) in cases {
            let key = padded(&key).in_format(MacFormat::V1);
            assert_eq!(key.hex(&[&data]), expected);
        }
    }

    #[test]
    fn v1_joins_parts_ambiguously_and_v2_does_not() {
        let v1 = LocalKey::from_bytes([0x0b; 32]).in_format(MacFormat::V1);
        assert_eq!(
            v1.hex(&[b"a\0".as_slice(), b"b"]),
            v1.hex(&[b"a".as_slice(), b"\0b"])
        );

        let v2 = LocalKey::from_bytes([0x0b; 32]);
        assert_eq!(v2.format(), MacFormat::V2);
        let tags = [
            v2.hex(&[b"a\0".as_slice(), b"b"]),
            v2.hex(&[b"a".as_slice(), b"\0b"]),
            v2.hex(&[b"a".as_slice(), b"b"]),
            v2.hex(&[b"ab"]),
            v2.hex(&[b"a\0b"]),
        ];
        let distinct: std::collections::HashSet<_> = tags.iter().collect();
        assert_eq!(distinct.len(), tags.len(), "two part lists collide");
    }

    #[test]
    fn the_formats_match_independently_computed_vectors() {
        let key = LocalKey::from_bytes([0x0b; 32]);
        assert_eq!(
            key.hex(&EVENT),
            "b03ee87689cb560078ad309e6d32f6f73c823d5e177c4ca93e29f110cefcd20b"
        );
        assert_eq!(
            key.in_format(MacFormat::V1).hex(&EVENT),
            "11ecb4e82f405d2e76b9d35a457a28da01df6103284e613315ec108b007c597e"
        );
        assert_eq!(
            key.hex(&[b"a\0".as_slice(), b"b"]),
            "a32047473d14518acd7b793bc8f107ce579f5e244cf6fc6a5f232a67108c5da7"
        );
    }

    #[test]
    fn the_mac_depends_on_the_key_and_yields_a_v4_uuid() {
        let key = LocalKey::from_bytes([0x0b; 32]);
        assert_ne!(
            key.hex(&[b"x"]),
            LocalKey::from_bytes([0x0c; 32]).hex(&[b"x"])
        );
        let id = key.uuid(&[b"task_id", b"raw"]);
        assert_eq!(id.len(), 36);
        assert_eq!(id.as_bytes().get(14), Some(&b'4'));
    }

    #[test]
    fn verifies_accepts_only_the_exact_tag_of_its_own_format() {
        let key = LocalKey::from_bytes([0x0b; 32]);
        let tag = key.hex(&EVENT);
        assert!(key.verifies(&EVENT, &tag));

        let rest = tag.get(1..).unwrap_or_default();
        let first = if tag.starts_with('0') { '1' } else { '0' };
        assert!(!key.verifies(&EVENT, &format!("{first}{rest}")));
        assert!(!key.verifies(&EVENT, rest));
        assert!(!key.verifies(&EVENT, &"z".repeat(tag.len())));
        assert!(!key.verifies(&EVENT, ""));
        assert!(!key.verifies(&EVENT, &tag.to_uppercase()));
        assert!(!key.verifies(&EVENT, &key.in_format(MacFormat::V1).hex(&EVENT)));
        assert!(
            key.in_format(MacFormat::V1)
                .verifies(&EVENT, &key.in_format(MacFormat::V1).hex(&EVENT))
        );
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
