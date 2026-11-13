use std::collections::BTreeMap;
use std::io::Read as _;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{Channel, UpdateError, files};

#[derive(Debug, Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
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

pub(super) async fn stage(
    api: &Url,
    version: Option<&str>,
    beta: bool,
    running: &str,
    directory: &Path,
    shutdown: &CancellationToken,
) -> Result<Staged, UpdateError> {
    let platform = platform()?;
    let client = reqwest::Client::builder()
        .user_agent("RivetHub updater")
        .https_only(api.scheme() == "https")
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .build()?;
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
    let release = if version.is_none() && beta {
        endpoint.set_query(Some("per_page=100"));
        let body = download(&client, endpoint, 8 * 1024 * 1024, shutdown).await?;
        let releases: Vec<Release> =
            serde_json::from_slice(&body).map_err(|_| UpdateError::Release)?;
        newest(releases)?
    } else {
        let body = match download(&client, endpoint, 1024 * 1024, shutdown).await {
            Err(UpdateError::Http(error))
                if error.status() == Some(reqwest::StatusCode::NOT_FOUND) =>
            {
                return Err(version.map_or(UpdateError::NoRelease, |version| {
                    UpdateError::NotPublished(version.to_owned())
                }));
            }
            downloaded => downloaded?,
        };
        serde_json::from_slice(&body).map_err(|_| UpdateError::Release)?
    };
    accept(&release, version, beta)?;
    validate_version(&release.tag_name)?;
    let pinned = version.is_some();
    let version = release
        .tag_name
        .strip_prefix('v')
        .ok_or_else(|| UpdateError::Version(release.tag_name.clone()))?;
    if !pinned {
        let published = semver::Version::parse(version)
            .map_err(|_| UpdateError::Version(release.tag_name.clone()))?;
        let running = semver::Version::parse(running)
            .map_err(|_| UpdateError::Version(running.to_owned()))?;
        match published.cmp(&running) {
            std::cmp::Ordering::Less => return Ok(Staged::Newer(release.tag_name)),
            std::cmp::Ordering::Equal => return Ok(Staged::UpToDate(release.tag_name)),
            std::cmp::Ordering::Greater => {}
        }
    }
    let archive_name = format!("rh-{version}-{platform}.tar.gz");
    let manifest = asset(&release, "SHA256SUMS")?;
    let manifest = download(&client, manifest, 16 * 1024, shutdown).await?;
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

fn asset(release: &Release, name: &str) -> Result<Url, UpdateError> {
    let missing = || UpdateError::Asset {
        tag: release.tag_name.clone(),
        name: name.to_owned(),
    };
    let mut matching = release.assets.iter().filter(|asset| asset.name == name);
    let asset = matching.next().ok_or_else(missing)?;
    if matching.next().is_some() {
        return Err(missing());
    }
    Ok(asset.browser_download_url.clone())
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

async fn download(
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
