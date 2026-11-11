use std::fmt;
use std::path::Path;
use std::path::PathBuf;
use std::str::FromStr;

use aws_lc_rs::rand::SystemRandom;
use aws_lc_rs::signature::ED25519;
use aws_lc_rs::signature::Ed25519KeyPair;
use aws_lc_rs::signature::KeyPair as _;
use aws_lc_rs::signature::UnparsedPublicKey;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::Deserialize;
use serde::Serialize;
use serde_with::DeserializeFromStr;
use serde_with::SerializeDisplay;
use sha2::Digest as _;
use sha2::Sha256;
use thiserror::Error;
use wire::capture::ReceiptVerification;

use crate::sanitize::KeyError;
use crate::sanitize::key::PrivateFile;

pub const KEY_FILE: &str = "node-signing.key";
pub const TRUST_FILE: &str = "trusted-senders.toml";
const KEY_NAME: &str = "node signing key";
const PKCS8_CAP: u64 = 256;
const MESSAGE_TAG: &[u8] = b"rivethub-bundle-sig-v1\n";
const KEY_PREFIX: &str = "ed25519:";
const PUBLIC_KEY_BYTES: usize = 32;
const FINGERPRINT_HEX: usize = 16;

#[derive(Debug, Error)]
pub enum SignError {
    #[error(transparent)]
    File(#[from] KeyError),
    #[error(
        "the node signing key {} is not an Ed25519 PKCS#8 key; move it aside to make a new one", .0.display()
    )]
    Invalid(PathBuf),
}

pub struct NodeKey(Ed25519KeyPair);

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 140: the node signing key never leaves the machine; deriving Debug would print it"
    )
)]
impl fmt::Debug for NodeKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "NodeKey({})", self.public_key())
    }
}

impl NodeKey {
    #[must_use]
    pub fn path_in(state_dir: &Path) -> PathBuf {
        state_dir
            .join(crate::sanitize::key::DIR_NAME)
            .join(KEY_FILE)
    }

    pub fn load_or_create(path: &Path) -> Result<Self, SignError> {
        let pkcs8 = PrivateFile::new(path, KEY_NAME, PKCS8_CAP).load_or_create(|| {
            Ed25519KeyPair::generate_pkcs8(&SystemRandom::new())
                .map(|document| document.as_ref().to_vec())
                .map_err(|error| KeyError::Random {
                    name: KEY_NAME,
                    reason: error.to_string(),
                })
        })?;
        Ed25519KeyPair::from_pkcs8(&pkcs8)
            .map(Self)
            .map_err(|_| SignError::Invalid(path.to_path_buf()))
    }

    pub fn load(path: &Path) -> Result<Self, SignError> {
        let pkcs8 = PrivateFile::new(path, KEY_NAME, PKCS8_CAP).read()?;
        Ed25519KeyPair::from_pkcs8(&pkcs8)
            .map(Self)
            .map_err(|_| SignError::Invalid(path.to_path_buf()))
    }

    #[must_use]
    pub fn public_key(&self) -> PublicKey {
        let mut bytes = [0u8; PUBLIC_KEY_BYTES];
        bytes.copy_from_slice(self.0.public_key().as_ref());
        PublicKey(bytes)
    }

    #[must_use]
    pub fn sign(&self, bundle: &[u8], user: &str) -> Signature {
        Signature {
            alg: Algorithm::Ed25519,
            public_key: self.public_key(),
            sig: STANDARD.encode(self.0.sign(&message(bundle, user)).as_ref()),
        }
    }
}

fn message(bundle: &[u8], user: &str) -> Vec<u8> {
    let mut message = MESSAGE_TAG.to_vec();
    message.extend_from_slice(&Sha256::digest(bundle));
    message.extend_from_slice(user.as_bytes());
    message
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, SerializeDisplay, DeserializeFromStr)]
pub struct PublicKey([u8; PUBLIC_KEY_BYTES]);

impl PublicKey {
    #[must_use]
    pub fn fingerprint(&self) -> String {
        let mut digest = hex::encode(Sha256::digest(self.0));
        digest.truncate(FINGERPRINT_HEX);
        format!("{KEY_PREFIX}{digest}")
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{KEY_PREFIX}{}", STANDARD.encode(self.0))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("{0:?} is not a sender key; give the ed25519:<base64> line that rh memory key show prints")]
pub struct KeyParseError(String);

impl FromStr for PublicKey {
    type Err = KeyParseError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let invalid = || KeyParseError(text.to_owned());
        let encoded = text.trim().strip_prefix(KEY_PREFIX).ok_or_else(invalid)?;
        let bytes = STANDARD.decode(encoded).map_err(|_| invalid())?;
        Ok(Self(bytes.try_into().map_err(|_| invalid())?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Algorithm {
    Ed25519,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Signature {
    pub alg: Algorithm,
    pub public_key: PublicKey,
    pub sig: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
#[error(
    "the bundle signature does not match its contents; it was altered after signing, so nothing was imported"
)]
pub struct BadSignature;

impl Signature {
    pub fn verify(&self, bundle: &[u8], user: &str) -> Result<PublicKey, BadSignature> {
        let Algorithm::Ed25519 = self.alg;
        let sig = STANDARD.decode(&self.sig).map_err(|_| BadSignature)?;
        UnparsedPublicKey::new(&ED25519, self.public_key.0)
            .verify(&message(bundle, user), &sig)
            .map_err(|_| BadSignature)?;
        Ok(self.public_key)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verification {
    Unsigned,
    Signed { key: PublicKey, trust: Trust },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Trust {
    Listed,
    NotListed { trusted_as: Vec<String> },
}

impl Verification {
    #[must_use]
    pub const fn kind(&self) -> ReceiptVerification {
        match self {
            Self::Unsigned => ReceiptVerification::Unsigned,
            Self::Signed {
                trust: Trust::Listed,
                ..
            } => ReceiptVerification::Verified,
            Self::Signed {
                trust: Trust::NotListed { .. },
                ..
            } => ReceiptVerification::UnknownKey,
        }
    }

    #[must_use]
    pub const fn key(&self) -> Option<&PublicKey> {
        match self {
            Self::Signed { key, .. } => Some(key),
            Self::Unsigned => None,
        }
    }

    #[must_use]
    pub fn reason(&self, user: &str) -> String {
        match self {
            Self::Unsigned => "unverified: the bundle is unsigned".to_owned(),
            Self::Signed {
                key,
                trust: Trust::Listed,
            } => format!("verified, signed by {}", key.fingerprint()),
            Self::Signed {
                key,
                trust: Trust::NotListed { trusted_as },
            } if trusted_as.is_empty() => format!(
                "unverified: signed by {}, which is not in your trusted senders",
                key.fingerprint()
            ),
            Self::Signed {
                key,
                trust: Trust::NotListed { trusted_as },
            } => format!(
                "unverified: signed by {}, which you trust as {}, not as {user}",
                key.fingerprint(),
                trusted_as.join(", ")
            ),
        }
    }
}

#[derive(Debug, Error)]
pub enum TrustError {
    #[error("the trusted senders file {path} cannot be read or written: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("the trusted senders file {path} is invalid: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("the trusted senders file cannot be written: {0}")]
    Write(#[from] toml::ser::Error),
    #[error("a sender name is blank; name the person whose key this is")]
    BlankName,
    #[error("{name} is already trusted with {key}")]
    Duplicate { name: String, key: PublicKey },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustedSender {
    pub name: String,
    pub key: PublicKey,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrustStore {
    #[serde(default, rename = "sender", skip_serializing_if = "Vec::is_empty")]
    senders: Vec<TrustedSender>,
}

impl TrustStore {
    #[must_use]
    pub fn path_in(config_dir: &Path) -> PathBuf {
        config_dir
            .join(crate::sanitize::key::DIR_NAME)
            .join(TRUST_FILE)
    }

    pub fn load(path: &Path) -> Result<Self, TrustError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => {
                return Err(TrustError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        toml::from_str(&text).map_err(|source| TrustError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn save(&self, path: &Path) -> Result<(), TrustError> {
        let io = |source| TrustError::Io {
            path: path.to_path_buf(),
            source,
        };
        let text = toml::to_string(self)?;
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent).map_err(io)?;
        let mut partial = tempfile::Builder::new()
            .prefix(".trusted-senders-")
            .tempfile_in(parent)
            .map_err(io)?;
        std::io::Write::write_all(&mut partial, text.as_bytes()).map_err(io)?;
        partial.persist(path).map_err(|error| io(error.error))?;
        Ok(())
    }

    #[must_use]
    pub fn senders(&self) -> &[TrustedSender] {
        &self.senders
    }

    pub fn add(&mut self, name: &str, key: PublicKey) -> Result<(), TrustError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(TrustError::BlankName);
        }
        if self
            .senders
            .iter()
            .any(|sender| sender.name == name && sender.key == key)
        {
            return Err(TrustError::Duplicate {
                name: name.to_owned(),
                key,
            });
        }
        self.senders.push(TrustedSender {
            name: name.to_owned(),
            key,
        });
        Ok(())
    }

    pub fn remove(&mut self, name: &str, key: Option<&PublicKey>) -> usize {
        let before = self.senders.len();
        self.senders
            .retain(|sender| sender.name != name || key.is_some_and(|key| sender.key != *key));
        before - self.senders.len()
    }

    pub fn verification(
        &self,
        signature: Option<&Signature>,
        bundle: &[u8],
        user: &str,
    ) -> Result<Verification, BadSignature> {
        let Some(signature) = signature else {
            return Ok(Verification::Unsigned);
        };
        let key = signature.verify(bundle, user)?;
        if self
            .senders
            .iter()
            .any(|sender| sender.name == user && sender.key == key)
        {
            return Ok(Verification::Signed {
                key,
                trust: Trust::Listed,
            });
        }
        Ok(Verification::Signed {
            key,
            trust: Trust::NotListed {
                trusted_as: self
                    .senders
                    .iter()
                    .filter(|sender| sender.key == key)
                    .map(|sender| sender.name.clone())
                    .collect(),
            },
        })
    }
}
