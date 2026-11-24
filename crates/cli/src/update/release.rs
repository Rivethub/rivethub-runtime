use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;
use url::Url;

use gateway::account::ReleaseKeyring;

use super::{Channel, UpdateError, files};

#[derive(Debug, Deserialize)]
pub(super) struct Release {
    pub(super) tag_name: String,
    pub(super) draft: bool,
    prerelease: bool,
    assets: Vec<Asset>,
}

#[derive(Debug, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: Url,
}

pub(super) fn validate_version(version: &str) -> Result<(), UpdateError> {
    if version.is_empty()
        || !version.starts_with(|c: char| c.is_ascii_alphanumeric())
        || !version
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        || version.contains("..")
    {
        return Err(UpdateError::Version(version.to_owned()));
    }
    Ok(())
}

const BINARY_LIMIT: u64 = 512 * 1024 * 1024;

pub(super) fn platform() -> Result<&'static str, UpdateError> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-gnu"),
        ("macos", "x86_64" | "aarch64") => Ok("universal-apple-darwin"),
        _ => Err(UpdateError::Platform),
    }
}

pub(super) enum Staged {
    Ready(String),
    UpToDate(String),
    Newer(String),
}

impl Staged {
    pub(super) fn version(self, running: &str, channel: Channel) -> Result<String, String> {
        match self {
            Self::Ready(version) => Ok(version),
            Self::UpToDate(latest) => Err(format!(
                "rh {running} is already up to date (latest {channel} release {latest}).\n"
            )),
            Self::Newer(latest) => Err(format!(
                "rh {running} is already newer than the latest {channel} release {latest}; nothing to do.\n"
            )),
        }
    }
}

pub(super) struct StageTarget<'a> {
    pub(super) version: Option<&'a str>,
    pub(super) beta: bool,
    pub(super) running: &'a str,
}

pub(super) async fn stage(
    api: &Url,
    target: StageTarget<'_>,
    directory: &Path,
    keyring: &ReleaseKeyring,
    shutdown: &CancellationToken,
) -> Result<Staged, UpdateError> {
    let platform = platform()?;
    let client = client(api)?;
    let release = fetch_release(&client, api, target.version, target.beta, shutdown).await?;
    accept(&release, target.version, target.beta)?;
    validate_version(&release.tag_name)?;
    let version = release
        .tag_name
        .strip_prefix('v')
        .ok_or_else(|| UpdateError::Version(release.tag_name.clone()))?;
    if target.version.is_none() {
        let published = semver::Version::parse(version)
            .map_err(|_| UpdateError::Version(release.tag_name.clone()))?;
        let running = semver::Version::parse(target.running)
            .map_err(|_| UpdateError::Version(target.running.to_owned()))?;
        match published.cmp(&running) {
            std::cmp::Ordering::Less => return Ok(Staged::Newer(release.tag_name)),
            std::cmp::Ordering::Equal => return Ok(Staged::UpToDate(release.tag_name)),
            std::cmp::Ordering::Greater => {}
        }
    }
    let archive_name = format!("rh-{version}-{platform}.tar.gz");
    let manifest = fetch_verified_manifest(&client, &release, keyring, shutdown).await?;
    let expected = *checksums(&manifest)?
        .get(&archive_name)
        .ok_or(UpdateError::Manifest)?;
    let archive = download(
        &client,
        asset(&release, &archive_name)?,
        512 * 1024 * 1024,
        shutdown,
    )
    .await?;
    let actual: [u8; 32] = Sha256::digest(&archive).into();
    if actual != expected {
        return Err(UpdateError::Checksum(archive_name));
    }
    let directory = directory.to_owned();
    tokio::task::spawn_blocking(move || unpack(&archive, &directory)).await??;
    Ok(Staged::Ready(release.tag_name))
}

async fn fetch_release(
    client: &reqwest::Client,
    api: &Url,
    version: Option<&str>,
    beta: bool,
    shutdown: &CancellationToken,
) -> Result<Release, UpdateError> {
    let mut endpoint = api.clone();
    {
        let mut segments = endpoint
            .path_segments_mut()
            .map_err(|()| UpdateError::Release)?;
        segments.pop_if_empty();
        if let Some(version) = version {
            validate_version(version)?;
            segments.push("tags").push(version);
        } else if !beta {
            segments.push("latest");
        }
    }
    if version.is_none() && beta {
        endpoint.set_query(Some("per_page=100"));
        let body = download(client, endpoint, 8 * 1024 * 1024, shutdown).await?;
        let releases: Vec<Release> =
            serde_json::from_slice(&body).map_err(|_| UpdateError::Release)?;
        return newest(releases);
    }
    let body = match download(client, endpoint, 1024 * 1024, shutdown).await {
        Err(UpdateError::Http(error)) if error.status() == Some(reqwest::StatusCode::NOT_FOUND) => {
            return Err(version.map_or(UpdateError::NoRelease, |version| {
                UpdateError::NotPublished(version.to_owned())
            }));
        }
        downloaded => downloaded?,
    };
    serde_json::from_slice(&body).map_err(|_| UpdateError::Release)
}

async fn fetch_verified_manifest(
    client: &reqwest::Client,
    release: &Release,
    keyring: &ReleaseKeyring,
    shutdown: &CancellationToken,
) -> Result<Vec<u8>, UpdateError> {
    let manifest_url = asset(release, "SHA256SUMS")?;
    let manifest = download(client, manifest_url, 16 * 1024, shutdown).await?;
    if let Some(sig_url) = asset_optional(release, "SHA256SUMS.sig")? {
        let sig_bytes = download(client, sig_url, 1024, shutdown).await?;
        let sig_text = std::str::from_utf8(&sig_bytes)
            .map_err(|_| UpdateError::Signature("SHA256SUMS.sig is not valid UTF-8".to_owned()))?;
        keyring.verify_envelope(&manifest, sig_text).map_err(|_| {
            UpdateError::Signature(format!(
                "SHA256SUMS.sig signature verification failed for {}",
                release.tag_name
            ))
        })?;
    } else {
        tracing::warn!(
            tag = %release.tag_name,
            "SHA256SUMS.sig is not published on this release; verifying SHA-256 checksum only"
        );
    }
    Ok(manifest)
}

fn accept(release: &Release, version: Option<&str>, beta: bool) -> Result<(), UpdateError> {
    if release.draft {
        return Err(UpdateError::Draft(release.tag_name.clone()));
    }
    match version {
        Some(version) if release.tag_name != version => Err(UpdateError::Mismatch {
            asked: version.to_owned(),
            answered: release.tag_name.clone(),
        }),
        None if release.prerelease && !beta => {
            Err(UpdateError::Prerelease(release.tag_name.clone()))
        }
        Some(_) | None => Ok(()),
    }
}

pub(super) fn unpack(archive: &[u8], directory: &Path) -> Result<(), UpdateError> {
    let mut unpacked = Vec::new();
    for entry in tar::Archive::new(flate2::read::GzDecoder::new(archive)).entries()? {
        let mut entry = entry?;
        let name = match entry.path()?.to_str() {
            Some("rh") => "rh",
            Some("rivethub-migrate") => "rivethub-migrate",
            _ => continue,
        };
        if !entry.header().entry_type().is_file() || unpacked.contains(&name) {
            return Err(UpdateError::Archive(name));
        }
        if entry.size() > BINARY_LIMIT {
            return Err(UpdateError::AssetSize);
        }
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes)?;
        files::write_executable(&directory.join(name), &bytes)?;
        unpacked.push(name);
    }
    for name in ["rh", "rivethub-migrate"] {
        if !unpacked.contains(&name) {
            return Err(UpdateError::Archive(name));
        }
    }
    Ok(())
}

fn newest(releases: Vec<Release>) -> Result<Release, UpdateError> {
    releases
        .into_iter()
        .filter(|release| !release.draft)
        .map(|release| {
            let version = release
                .tag_name
                .strip_prefix('v')
                .ok_or_else(|| UpdateError::Version(release.tag_name.clone()))?;
            let version = semver::Version::parse(version)
                .map_err(|_| UpdateError::Version(release.tag_name.clone()))?;
            Ok((version, release))
        })
        .collect::<Result<Vec<_>, UpdateError>>()?
        .into_iter()
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .map(|(_, release)| release)
        .ok_or(UpdateError::NoRelease)
}

pub(super) fn asset(release: &Release, name: &str) -> Result<Url, UpdateError> {
    let missing = || UpdateError::Asset {
        tag: release.tag_name.clone(),
        name: name.to_owned(),
    };
    asset_optional(release, name)?.ok_or_else(missing)
}

pub(super) fn asset_optional(release: &Release, name: &str) -> Result<Option<Url>, UpdateError> {
    let duplicate = || UpdateError::Asset {
        tag: release.tag_name.clone(),
        name: name.to_owned(),
    };
    let mut matching = release.assets.iter().filter(|asset| asset.name == name);
    let Some(asset) = matching.next() else {
        return Ok(None);
    };
    if matching.next().is_some() {
        return Err(duplicate());
    }
    Ok(Some(asset.browser_download_url.clone()))
}

fn checksums(bytes: &[u8]) -> Result<BTreeMap<String, [u8; 32]>, UpdateError> {
    let text = std::str::from_utf8(bytes).map_err(|_| UpdateError::Manifest)?;
    let mut entries = BTreeMap::new();
    for line in text.lines() {
        let mut fields = line.split_whitespace();
        let hash = fields.next().ok_or(UpdateError::Manifest)?;
        let name = fields.next().ok_or(UpdateError::Manifest)?;
        if fields.next().is_some() {
            return Err(UpdateError::Manifest);
        }
        let mut bytes = [0; 32];
        hex::decode_to_slice(hash, &mut bytes).map_err(|_| UpdateError::Manifest)?;
        if entries.insert(name.to_owned(), bytes).is_some() {
            return Err(UpdateError::Manifest);
        }
    }
    Ok(entries)
}

pub(super) fn client(api: &Url) -> Result<reqwest::Client, UpdateError> {
    Ok(reqwest::Client::builder()
        .user_agent("RivetHub updater")
        .https_only(api.scheme() == "https")
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .build()?)
}

pub(super) async fn download(
    client: &reqwest::Client,
    url: Url,
    limit: usize,
    shutdown: &CancellationToken,
) -> Result<Vec<u8>, UpdateError> {
    let operation = async {
        let mut response = client.get(url).send().await?.error_for_status()?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await? {
            if chunk.len() > limit.saturating_sub(bytes.len()) {
                return Err(UpdateError::AssetSize);
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    };
    tokio::select! {
        biased;
        () = shutdown.cancelled() => Err(UpdateError::Cancelled),
        result = operation => result,
    }
}
