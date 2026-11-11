use std::fs::OpenOptions;
use std::io::Read as _;
use std::io::Write as _;
use std::os::unix::fs::DirBuilderExt as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::path::PathBuf;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use parking_lot::RwLock;
use sha2::Digest as _;
use sha2::Sha256;
use subtle::ConstantTimeEq as _;

pub const FILE_NAME: &str = "local-owner.token";
const TOKEN_BYTES: usize = 32;
const READ_CAP: u64 = 256;
const OWNER_ONLY: u32 = 0o600;
const SHARED_BITS: u32 = 0o077;

#[derive(Debug, thiserror::Error)]
pub enum TokenError {
    #[error("the local owner token {path} cannot be read or written: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("the local owner token {path} is not a regular file; move it aside to make a new one")]
    NotRegular { path: PathBuf },
    #[error(
        "the local owner token {path} is readable by other users (mode {mode:o}); run chmod 600 on it"
    )]
    Shared { path: PathBuf, mode: u32 },
    #[error(
        "the local owner token {path} is not 32 random bytes in base64url; move it aside to make a new one"
    )]
    Malformed { path: PathBuf },
    #[error("no random bytes for a local owner token: {0}")]
    Random(String),
}

#[derive(Clone, PartialEq, Eq)]
pub struct OwnerToken(String);

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "SS-15a: the local owner token is a credential; deriving Debug would print it into logs and reports"
    )
)]
impl std::fmt::Debug for OwnerToken {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("OwnerToken(..)")
    }
}

impl OwnerToken {
    pub fn generate() -> Result<Self, TokenError> {
        let mut bytes = [0_u8; TOKEN_BYTES];
        getrandom::fill(&mut bytes).map_err(|error| TokenError::Random(error.to_string()))?;
        Ok(Self(URL_SAFE_NO_PAD.encode(bytes)))
    }

    fn parse(text: &str) -> Option<Self> {
        let trimmed = text.trim();
        let decoded = URL_SAFE_NO_PAD.decode(trimmed).ok()?;
        (decoded.len() == TOKEN_BYTES).then(|| Self(trimmed.to_owned()))
    }

    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    fn digest(&self) -> [u8; 32] {
        Sha256::digest(self.0.as_bytes()).into()
    }
}

#[must_use]
pub fn token_path(state_dir: &Path) -> PathBuf {
    state_dir.join(FILE_NAME)
}

pub fn read_token(state_dir: &Path) -> Result<Option<OwnerToken>, TokenError> {
    read_file(&token_path(state_dir))
}

fn io(path: &Path, source: std::io::Error) -> TokenError {
    TokenError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn read_file(path: &Path) -> Result<Option<OwnerToken>, TokenError> {
    let mut options = OpenOptions::new();
    options.read(true);
    options.custom_flags((nix::fcntl::OFlag::O_NOFOLLOW | nix::fcntl::OFlag::O_NONBLOCK).bits());
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io(path, error)),
    };
    let metadata = file.metadata().map_err(|error| io(path, error))?;
    if !metadata.is_file() {
        return Err(TokenError::NotRegular {
            path: path.to_path_buf(),
        });
    }
    let mode = metadata.permissions().mode() & 0o777;
    if mode & SHARED_BITS != 0 {
        return Err(TokenError::Shared {
            path: path.to_path_buf(),
            mode,
        });
    }
    let mut text = String::new();
    file.take(READ_CAP)
        .read_to_string(&mut text)
        .map_err(|error| io(path, error))?;
    OwnerToken::parse(&text)
        .map(Some)
        .ok_or_else(|| TokenError::Malformed {
            path: path.to_path_buf(),
        })
}

fn write_temp(path: &Path, token: &OwnerToken) -> Result<PathBuf, TokenError> {
    let mut suffix = [0_u8; 6];
    getrandom::fill(&mut suffix).map_err(|error| TokenError::Random(error.to_string()))?;
    let temp = path.with_extension(format!("{}.tmp", hex::encode(suffix)));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    options.mode(OWNER_ONLY);
    let mut file = options.open(&temp).map_err(|error| io(&temp, error))?;
    let written = file
        .write_all(token.expose().as_bytes())
        .and_then(|()| file.sync_all());
    match written {
        Ok(()) => Ok(temp),
        Err(error) => {
            drop(std::fs::remove_file(&temp));
            Err(io(&temp, error))
        }
    }
}

fn create_parent(path: &Path) -> Result<(), TokenError> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(parent)
        .map_err(|error| io(parent, error))
}

fn create_exclusive(path: &Path) -> Result<OwnerToken, TokenError> {
    let token = OwnerToken::generate()?;
    let temp = write_temp(path, &token)?;
    let linked = std::fs::hard_link(&temp, path);
    drop(std::fs::remove_file(&temp));
    match linked {
        Ok(()) => Ok(token),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            read_file(path)?.ok_or_else(|| io(path, error))
        }
        Err(error) => Err(io(path, error)),
    }
}

struct Held {
    token: OwnerToken,
    digest: [u8; 32],
}

impl Held {
    fn new(token: OwnerToken) -> Self {
        let digest = token.digest();
        Self { token, digest }
    }
}

pub struct LocalOwner {
    path: Option<PathBuf>,
    held: RwLock<Held>,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "SS-15a: the holder carries the live local owner token; deriving Debug would print it"
    )
)]
impl std::fmt::Debug for LocalOwner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LocalOwner")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl LocalOwner {
    pub fn load_or_create(state_dir: &Path) -> Result<Self, TokenError> {
        let path = token_path(state_dir);
        create_parent(&path)?;
        let token = match read_file(&path)? {
            Some(token) => token,
            None => create_exclusive(&path)?,
        };
        Ok(Self {
            path: Some(path),
            held: RwLock::new(Held::new(token)),
        })
    }

    pub fn unpersisted() -> Result<Self, TokenError> {
        Ok(Self {
            path: None,
            held: RwLock::new(Held::new(OwnerToken::generate()?)),
        })
    }

    #[must_use]
    pub fn verify(&self, presented: &str) -> bool {
        let digest: [u8; 32] = Sha256::digest(presented.as_bytes()).into();
        bool::from(self.held.read().digest.ct_eq(&digest))
    }

    #[must_use]
    pub fn current(&self) -> OwnerToken {
        self.held.read().token.clone()
    }

    pub fn rotate(&self) -> Result<(), TokenError> {
        let fresh = OwnerToken::generate()?;
        let mut held = self.held.write();
        if let Some(path) = &self.path {
            let temp = write_temp(path, &fresh)?;
            if let Err(error) = std::fs::rename(&temp, path) {
                drop(std::fs::remove_file(&temp));
                return Err(io(path, error));
            }
        }
        *held = Held::new(fresh);
        drop(held);
        Ok(())
    }
}
