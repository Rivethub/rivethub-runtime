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

pub const RELEASE_KEY: UpdateKey = UpdateKey([
    0xb0, 0xa9, 0x5e, 0x88, 0x41, 0x43, 0xa7, 0xbd, 0xfa, 0x88, 0x26, 0x30, 0xe2, 0x68, 0x80, 0x0c,
    0x26, 0x8f, 0x74, 0x8a, 0x04, 0x89, 0xae, 0x2b, 0x25, 0x28, 0x7a, 0xfd, 0xb0, 0xcc, 0x2d, 0x5b,
]);

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
    pub fn parse(text: &str) -> Result<Self, AccountError> {
        let bytes = STANDARD
            .decode(text.trim())
            .map_err(|_| AccountError::UpdateKey)?;
        Ok(Self(bytes.try_into().map_err(|_| AccountError::UpdateKey)?))
    }

    pub fn verify(&self, manifest: &ReleaseManifest) -> Result<[u8; 32], AccountError> {
        let mut digest = [0; 32];
        hex::decode_to_slice(&manifest.sha256, &mut digest).map_err(|_| AccountError::Manifest)?;
        let signature = STANDARD
            .decode(manifest.signature.trim())
            .map_err(|_| AccountError::Signature)?;
        UnparsedPublicKey::new(&ED25519, self.0)
            .verify(&digest, &signature)
            .map_err(|_| AccountError::Signature)?;
        Ok(digest)
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
