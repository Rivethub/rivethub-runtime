use std::path::Path;

use aws_lc_rs::signature::{ED25519, UnparsedPublicKey};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;
use url::Url;
use wire::gateway::account::ReleaseChannel;

use super::cloud::Cloud;
use super::install::{AccountFiles, write_private};
use super::{AccountError, CloudSettings};

pub const RELEASE_KEY_ID: &str = "2026-main";

pub const RELEASE_KEY: UpdateKey = pinned(include_str!("../../release-keys/2026-main.hex"));

pub const REQUIRE_SIGNED_RELEASES: bool = false;

const fn pinned(hex: &str) -> UpdateKey {
    let mut digits = hex.trim_ascii().as_bytes();
    let mut key = [0; 32];
    let mut filled = 0;
    let mut slots = key.as_mut_slice();
    while let Some((slot, rest)) = slots.split_first_mut() {
        let Some(([high, low], others)) = digits.split_first_chunk::<2>() else {
            break;
        };
        *slot = (nibble(*high) << 4) | nibble(*low);
        digits = others;
        slots = rest;
        filled += 1;
    }
    assert!(
        digits.is_empty() && filled == 32,
        "a pinned release key is 64 lowercase hex digits"
    );
    UpdateKey(key)
}

const fn nibble(digit: u8) -> u8 {
    assert!(
        matches!(digit, b'0'..=b'9' | b'a'..=b'f'),
        "a pinned release key is 64 lowercase hex digits"
    );
    if digit <= b'9' {
        digit - b'0'
    } else {
        digit - b'a' + 10
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrustedKey {
    pub id: &'static str,
    pub key: UpdateKey,
}

pub const TRUSTED_RELEASE_KEYS: &[TrustedKey] = &[TrustedKey {
    id: RELEASE_KEY_ID,
    key: RELEASE_KEY,
}];

const ARCHIVE_LIMIT: usize = 512 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::Display)]
pub enum CloudOs {
    #[serde(rename = "linux-x86_64")]
    #[strum(serialize = "linux-x86_64")]
    LinuxX86,
    #[serde(rename = "linux-aarch64")]
    #[strum(serialize = "linux-aarch64")]
    LinuxArm,
    #[serde(rename = "macos-x86_64")]
    #[strum(serialize = "macos-x86_64")]
    MacosX86,
    #[serde(rename = "macos-aarch64")]
    #[strum(serialize = "macos-aarch64")]
    MacosArm,
}

impl CloudOs {
    #[must_use]
    pub fn current() -> Option<Self> {
        match (std::env::consts::OS, std::env::consts::ARCH) {
            ("linux", "x86_64") => Some(Self::LinuxX86),
            ("linux", "aarch64") => Some(Self::LinuxArm),
            ("macos", "x86_64") => Some(Self::MacosX86),
            ("macos", "aarch64") => Some(Self::MacosArm),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReleaseManifest {
    pub version: String,
    pub url: Url,
    pub sha256: String,
    pub signature: String,
    pub notes_url: Url,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateKey([u8; 32]);

impl UpdateKey {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn parse(text: &str) -> Result<Self, AccountError> {
        let bytes = STANDARD
            .decode(text.trim())
            .map_err(|_| AccountError::UpdateKey)?;
        Ok(Self(bytes.try_into().map_err(|_| AccountError::UpdateKey)?))
    }

    pub fn verify_bytes(&self, payload: &[u8], signature: &[u8]) -> Result<(), AccountError> {
        UnparsedPublicKey::new(&ED25519, self.0)
            .verify(payload, signature)
            .map_err(|_| AccountError::Signature)
    }

    pub fn verify(&self, manifest: &ReleaseManifest) -> Result<[u8; 32], AccountError> {
        let mut digest = [0; 32];
        hex::decode_to_slice(&manifest.sha256, &mut digest).map_err(|_| AccountError::Manifest)?;
        let signature = STANDARD
            .decode(manifest.signature.trim())
            .map_err(|_| AccountError::Signature)?;
        self.verify_bytes(&digest, &signature)?;
        Ok(digest)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseKeyring {
    keys: Vec<(String, UpdateKey)>,
    require_signatures: bool,
}

impl ReleaseKeyring {
    #[must_use]
    pub fn default_trusted() -> Self {
        Self {
            keys: TRUSTED_RELEASE_KEYS
                .iter()
                .map(|entry| (entry.id.to_owned(), entry.key))
                .collect(),
            require_signatures: REQUIRE_SIGNED_RELEASES,
        }
    }

    #[must_use]
    pub const fn require_signatures(mut self, required: bool) -> Self {
        self.require_signatures = required;
        self
    }

    #[must_use]
    pub const fn requires_signatures(&self) -> bool {
        self.require_signatures
    }

    #[must_use]
    pub fn with_key(mut self, id: impl Into<String>, key: UpdateKey) -> Self {
        let id = id.into();
        self.keys.retain(|(existing, _)| existing != &id);
        self.keys.push((id, key));
        self
    }

    pub fn verify_envelope(&self, payload: &[u8], envelope: &str) -> Result<String, AccountError> {
        let trimmed = envelope.trim();
        let Some(("v1", rest)) = trimmed.split_once(':') else {
            return Err(AccountError::Signature);
        };
        let Some((key_id, b64)) = rest.split_once(':') else {
            return Err(AccountError::Signature);
        };
        if key_id.is_empty() || b64.is_empty() || b64.contains(':') {
            return Err(AccountError::Signature);
        }
        let signature = STANDARD.decode(b64).map_err(|_| AccountError::Signature)?;
        if signature.len() != 64 {
            return Err(AccountError::Signature);
        }
        let key = self
            .keys
            .iter()
            .find_map(|(id, key)| (id == key_id).then_some(*key))
            .ok_or(AccountError::Signature)?;
        key.verify_bytes(payload, &signature)?;
        Ok(key_id.to_owned())
    }
}

#[derive(Debug)]
pub struct VerifiedRelease {
    pub manifest: ReleaseManifest,
    pub archive: Vec<u8>,
}

pub(super) fn plain_version(version: &str) -> Result<semver::Version, AccountError> {
    semver::Version::parse(version).map_err(|_| AccountError::Manifest)
}

pub async fn latest(
    cloud: &Cloud,
    settings: &CloudSettings,
    token: &str,
    channel: ReleaseChannel,
) -> Result<Option<(ReleaseManifest, [u8; 32])>, AccountError> {
    let os = CloudOs::current().ok_or(AccountError::Platform)?;
    let Some(manifest) = cloud.latest(token, channel, os).await? else {
        return Ok(None);
    };
    plain_version(&manifest.version)?;
    let digest = settings.update_key.verify(&manifest)?;
    Ok(Some((manifest, digest)))
}

pub async fn fetch(
    cloud: &Cloud,
    files: &AccountFiles,
    manifest: ReleaseManifest,
    digest: [u8; 32],
    shutdown: &CancellationToken,
) -> Result<VerifiedRelease, AccountError> {
    let cached = files.updates().join(archive_name(&manifest)?);
    let archive = match tokio::fs::read(&cached).await {
        Ok(bytes) if matches(&bytes, &digest) => bytes,
        Ok(_) => fresh(cloud, &manifest, &cached, shutdown).await?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fresh(cloud, &manifest, &cached, shutdown).await?
        }
        Err(error) => return Err(error.into()),
    };
    if !matches(&archive, &digest) {
        return Err(AccountError::Checksum);
    }
    Ok(VerifiedRelease { manifest, archive })
}

async fn fresh(
    cloud: &Cloud,
    manifest: &ReleaseManifest,
    cached: &Path,
    shutdown: &CancellationToken,
) -> Result<Vec<u8>, AccountError> {
    let archive = tokio::select! {
        biased;
        () = shutdown.cancelled() => return Err(AccountError::Cancelled),
        archive = cloud.download(manifest.url.clone(), ARCHIVE_LIMIT) => archive?,
    };
    let path = cached.to_owned();
    let bytes = archive.clone();
    tokio::task::spawn_blocking(move || write_private(&path, &bytes)).await??;
    Ok(archive)
}

fn archive_name(manifest: &ReleaseManifest) -> Result<String, AccountError> {
    let version = plain_version(&manifest.version)?;
    Ok(format!("rh-{version}.tar.gz"))
}

fn matches(bytes: &[u8], digest: &[u8; 32]) -> bool {
    let actual: [u8; 32] = Sha256::digest(bytes).into();
    &actual == digest
}

#[cfg(test)]
mod tests {
    use aws_lc_rs::signature::{Ed25519KeyPair, KeyPair as _};
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;

    use super::{AccountError, RELEASE_KEY_ID, ReleaseKeyring, TRUSTED_RELEASE_KEYS, UpdateKey};

    #[test]
    fn trusted_keys_are_exactly_the_committed_pin_files() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("release-keys");
        let mut committed = Vec::new();
        for entry in std::fs::read_dir(directory)? {
            let path = entry?.path();
            let id = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .ok_or("pin file name")?
                .to_owned();
            let mut bytes = [0; 32];
            hex::decode_to_slice(std::fs::read_to_string(&path)?.trim(), &mut bytes)?;
            committed.push((id, UpdateKey::from_bytes(bytes)));
        }
        committed.sort_by(|left, right| left.0.cmp(&right.0));
        let mut trusted: Vec<_> = TRUSTED_RELEASE_KEYS
            .iter()
            .map(|entry| (entry.id.to_owned(), entry.key))
            .collect();
        trusted.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(trusted, committed);
        Ok(())
    }

    #[test]
    fn keyring_verifies_valid_envelope_and_rejects_forged_or_unknown() {
        let pair = Ed25519KeyPair::generate().expect("ephemeral key");
        let raw: [u8; 32] = pair
            .public_key()
            .as_ref()
            .try_into()
            .expect("ed25519 public key length");
        let key = UpdateKey::from_bytes(raw);
        let keyring = ReleaseKeyring::default_trusted().with_key("test-key", key);
        assert!(keyring.keys.iter().any(|(id, _)| id == RELEASE_KEY_ID));

        let payload = b"deadbeef  rh-1.0.0.tar.gz\n";
        let sig_b64 = STANDARD.encode(pair.sign(payload).as_ref());
        let envelope = format!("v1:test-key:{sig_b64}\n");
        assert_eq!(
            keyring
                .verify_envelope(payload, &envelope)
                .expect("valid signature"),
            "test-key"
        );

        assert!(matches!(
            keyring.verify_envelope(b"tampered", &envelope),
            Err(AccountError::Signature)
        ));
        assert!(matches!(
            keyring.verify_envelope(payload, &format!("v1:{RELEASE_KEY_ID}:{sig_b64}")),
            Err(AccountError::Signature)
        ));
        assert!(matches!(
            keyring.verify_envelope(payload, &format!("v1:unknown-key:{sig_b64}")),
            Err(AccountError::Signature)
        ));
        for bad in ["", "v2:test-key:AA==", "v1::AA==", "v1:test-key:AAAA"] {
            assert!(
                matches!(
                    keyring.verify_envelope(payload, bad),
                    Err(AccountError::Signature)
                ),
                "expected failure for {bad}"
            );
        }
    }
}
