use std::fs::{self, File, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::os::unix::fs::symlink;
use std::path::{Component, Path, PathBuf};

use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;
use url::Url;

use super::{Channel, UpdateError, files, release};

pub(super) const RELEASES: &str = "https://api.github.com/repos/Rivethub/rivethub-app/releases";
const DOWNLOAD_LIMIT: usize = 128 * 1024 * 1024;
const UNPACK_LIMIT: u64 = 512 * 1024 * 1024;
const MANIFEST_LIMIT: u32 = 16 * 1024;
const MANIFEST: &str = "web-manifest.json";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Manifest {
    schema_version: u32,
    version: Version,
    commit: String,
    channel: Channel,
    min_rh_version: Version,
    asset: String,
    sha256: String,
}

impl Manifest {
    fn digest(&self) -> Result<[u8; 32], UpdateError> {
        let mut bytes = [0; 32];
        hex::decode_to_slice(&self.sha256, &mut bytes).map_err(|_| UpdateError::WebManifest)?;
        Ok(bytes)
    }

    fn validate(&self, version: &Version, channel: Channel) -> Result<(), UpdateError> {
        if self.schema_version != 1
            || self.version != *version
            || self.channel != channel
            || self.commit.len() != 40
            || !self.commit.bytes().all(|byte| byte.is_ascii_hexdigit())
            || self.asset != format!("rivethub-web-{version}.tar.gz")
        {
            return Err(UpdateError::WebManifest);
        }
        self.digest()?;
        Ok(())
    }

    fn eligible(&self, channel: Channel, runtime: &Version) -> bool {
        accepts(channel, self.channel) && self.min_rh_version.cmp_precedence(runtime).is_le()
    }
}

fn accepts(node: Channel, build: Channel) -> bool {
    build == Channel::Stable || build == node
}

fn select<'a>(
    manifests: impl Iterator<Item = &'a Manifest>,
    channel: Channel,
    runtime: &Version,
) -> Option<&'a Manifest> {
    manifests
        .filter(|manifest| manifest.eligible(channel, runtime))
        .max_by(|left, right| left.version.cmp(&right.version))
}

fn tag(tag_name: &str) -> Option<(Version, Channel)> {
    let version = match Version::parse(tag_name.strip_prefix("web-v")?) {
        Ok(version) => version,
        Err(error) => {
            tracing::debug!(%tag_name, %error, "ignoring a web tag without a SemVer version");
            return None;
        }
    };
    let channel = match version.pre.as_str().split('.').next()? {
        "" => Channel::Stable,
        "beta" => Channel::Beta,
        "nightly" => Channel::Nightly,
        _ => return None,
    };
    Some((version, channel))
}

#[derive(Debug)]
pub(super) enum Outcome {
    Installed(Version),
    Unchanged(Version),
    NoCompatible,
    Explicit,
    Failed(String),
}

impl Outcome {
    pub(super) fn report(self, channel: Channel, report: &mut crate::Report) {
        let line = match self {
            Self::Failed(reason) => {
                let retry = match channel {
                    Channel::Stable => "rh update --web-only".to_owned(),
                    Channel::Beta | Channel::Nightly => {
                        format!("rh update --web-only --channel {channel}")
                    }
                };
                report.stderr.extend([
                    "Web: Update failed. Any active bundle was kept. Resolve the error and retry with `",
                    &retry,
                    "`. ",
                    &reason,
                    "\n",
                ]);
                report.exit = crate::Exit::Failure;
                return;
            }
            Self::Installed(version) => format!("Web: Installed {version}.\n"),
            Self::Unchanged(version) => format!("Web: {version} is already active.\n"),
            Self::NoCompatible => "Web: No compatible release found.\n".to_owned(),
            Self::Explicit => "Web: Using explicit static_dir.\n".to_owned(),
        };
        report.stdout.push_str(&line);
    }
}

pub(super) struct Store {
    root: PathBuf,
    current: PathBuf,
    _lock: File,
}

impl Store {
    pub(super) fn open(state: &Path) -> Result<Self, UpdateError> {
        let current = gateway::managed_web_link(state);
        let root = current
            .parent()
            .ok_or_else(|| UpdateError::Path(current.clone()))?
            .to_owned();
        fs::create_dir_all(&root)?;
        if !root.symlink_metadata()?.is_dir() {
            return Err(UpdateError::Path(root));
        }
        let lock = files::lock(&root.join(".update.lock"))?;
        remove_partial(&root.join(".staging"))?;
        Ok(Self {
            root,
            current,
            _lock: lock,
        })
    }

    fn active(&self) -> Result<Option<Version>, UpdateError> {
        let target = match fs::read_link(&self.current) {
            Ok(target) => target,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        let version = target
            .to_str()
            .ok_or_else(|| UpdateError::Path(target.clone()))?;
        let version = Version::parse(version).map_err(|_| UpdateError::Path(target.clone()))?;
        Ok(has_index(&self.root.join(target))?.then_some(version))
    }

    pub(super) async fn refresh(
        self,
        api: &Url,
        channel: Channel,
        runtime: &str,
        shutdown: &CancellationToken,
    ) -> Result<Outcome, UpdateError> {
        let runtime = Version::parse(runtime).map_err(|_| UpdateError::Version(runtime.into()))?;
        let client = release::client(api)?;
        let candidates = candidates(&client, api, channel, shutdown).await?;
        let selected = select(
            candidates.iter().map(|(manifest, _)| manifest),
            channel,
            &runtime,
        );
        let Some((manifest, archive)) = candidates
            .iter()
            .find(|(manifest, _)| Some(manifest) == selected)
        else {
            let outcome =
                tokio::task::spawn_blocking(move || self.no_compatible(&runtime, channel))
                    .await??;
            return Ok(outcome);
        };
        let version = manifest.version.clone();
        let (store, active) = tokio::task::spawn_blocking(move || {
            let active = self.active()?;
            Ok::<_, UpdateError>((self, active))
        })
        .await??;
        if active.as_ref() == Some(&version) {
            let destination = store.root.join(version.to_string());
            let manifest = manifest.clone();
            tokio::task::spawn_blocking(move || verify_installed(&destination, &manifest))
                .await??;
            return Ok(Outcome::Unchanged(version));
        }
        let bytes = release::download(&client, archive.clone(), DOWNLOAD_LIMIT, shutdown).await?;
        let manifest = manifest.clone();
        super::check_cancelled(shutdown)?;
        tokio::task::spawn_blocking(move || store.install(&manifest, &bytes)).await??;
        Ok(Outcome::Installed(version))
    }

    fn no_compatible(&self, runtime: &Version, channel: Channel) -> Result<Outcome, UpdateError> {
        if let Some(version) = self.active()? {
            let directory = self.root.join(version.to_string());
            let manifest = installed(&directory)?;
            if !manifest.eligible(channel, runtime) {
                return Err(UpdateError::WebCompatibility(
                    version,
                    runtime.clone(),
                    channel,
                ));
            }
            return Ok(Outcome::Unchanged(version));
        }
        Ok(Outcome::NoCompatible)
    }

    fn install(&self, manifest: &Manifest, bytes: &[u8]) -> Result<(), UpdateError> {
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        if digest != manifest.digest()? {
            return Err(UpdateError::Checksum(manifest.asset.clone()));
        }
        let staging = tempfile::Builder::new()
            .prefix(".staging")
            .rand_bytes(0)
            .tempdir_in(&self.root)?;
        let bundle = staging.path().join("bundle");
        fs::create_dir(&bundle)?;
        unpack(bytes, &bundle, UNPACK_LIMIT)?;
        let mut stamp = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(bundle.join(MANIFEST))?;
        stamp.write_all(&serde_json::to_vec(manifest).map_err(|_| UpdateError::WebManifest)?)?;
        stamp.sync_all()?;
        sync_dirs(&bundle)?;

        let version = manifest.version.to_string();
        let destination = self.root.join(&version);
        let link = staging.path().join("current");
        symlink(&version, &link)?;
        if verify_installed(&destination, manifest).is_err() {
            remove_partial(&destination)?;
            fs::rename(&bundle, &destination)?;
        }
        let directory = File::open(&self.root)?;
        directory.sync_all()?;
        fs::rename(link, &self.current)?;
        if let Err(error) = directory.sync_all() {
            tracing::warn!(%error, "could not sync the activated web bundle link");
        }
        Ok(())
    }
}

async fn candidates(
    client: &reqwest::Client,
    api: &Url,
    channel: Channel,
    shutdown: &CancellationToken,
) -> Result<Vec<(Manifest, Url)>, UpdateError> {
    let mut candidates = Vec::new();
    for page in 1_u64.. {
        let mut endpoint = api.clone();
        endpoint.set_query(Some(&format!("per_page=100&page={page}")));
        let bytes = release::download(client, endpoint, 8 * 1024 * 1024, shutdown).await?;
        let releases: Vec<release::Release> =
            serde_json::from_slice(&bytes).map_err(|_| UpdateError::Release)?;
        let last_page = releases.len() < 100;
        for release in releases.into_iter().filter(|release| !release.draft) {
            let Some((version, build_channel)) = tag(&release.tag_name) else {
                continue;
            };
            if accepts(channel, build_channel) {
                let Some(candidate) =
                    candidate(client, &release, &version, build_channel, shutdown).await?
                else {
                    tracing::warn!(
                        tag = %release.tag_name,
                        "skipping a web release without a valid manifest"
                    );
                    continue;
                };
                candidates.push(candidate);
            }
        }
        if last_page {
            break;
        }
    }
    Ok(candidates)
}

async fn candidate(
    client: &reqwest::Client,
    release: &release::Release,
    version: &Version,
    channel: Channel,
    shutdown: &CancellationToken,
) -> Result<Option<(Manifest, Url)>, UpdateError> {
    let Ok(url) = release::asset(release, MANIFEST) else {
        return Ok(None);
    };
    let bytes = match release::download(client, url, MANIFEST_LIMIT as usize, shutdown).await {
        Ok(bytes) => bytes,
        Err(UpdateError::AssetSize) => return Ok(None),
        Err(error) => return Err(error),
    };
    let Ok(manifest) = serde_json::from_slice::<Manifest>(&bytes) else {
        return Ok(None);
    };
    if manifest.validate(version, channel).is_err() {
        return Ok(None);
    }
    let Ok(archive) = release::asset(release, &manifest.asset) else {
        return Ok(None);
    };
    Ok(Some((manifest, archive)))
}

fn installed(directory: &Path) -> Result<Manifest, UpdateError> {
    let mut bytes = Vec::new();
    File::open(directory.join(MANIFEST))?
        .take(u64::from(MANIFEST_LIMIT) + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MANIFEST_LIMIT as usize {
        return Err(UpdateError::WebManifest);
    }
    serde_json::from_slice(&bytes).map_err(|_| UpdateError::WebManifest)
}

fn verify_installed(directory: &Path, expected: &Manifest) -> Result<(), UpdateError> {
    if directory.symlink_metadata()?.is_dir()
        && has_index(directory)?
        && installed(directory)? == *expected
    {
        return Ok(());
    }
    Err(UpdateError::WebChanged(expected.version.clone()))
}

fn has_index(directory: &Path) -> io::Result<bool> {
    match directory.join("index.html").symlink_metadata() {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn remove_partial(path: &Path) -> Result<(), UpdateError> {
    match path.symlink_metadata() {
        Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(path)?,
        Ok(_) => fs::remove_file(path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

fn unpack(bytes: &[u8], destination: &Path, limit: u64) -> Result<(), UpdateError> {
    let stream = flate2::read::MultiGzDecoder::new(bytes).take(limit + 1);
    let mut archive = tar::Archive::new(stream);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;
        if path.as_os_str().is_empty()
            || path
                .components()
                .any(|part| !matches!(part, Component::Normal(_) | Component::CurDir))
            || path.components().eq(Path::new(MANIFEST).components())
        {
            return Err(UpdateError::WebArchivePath(path.into_owned()));
        }
        let output = destination.join(path);
        let kind = entry.header().entry_type();
        if entry.size() > limit {
            return Err(UpdateError::AssetSize);
        }
        if kind.is_dir() {
            fs::create_dir_all(output)?;
        } else if kind.is_file() {
            if let Some(parent) = output.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(output)?;
            if io::copy(&mut entry, &mut file)? != entry.size() {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof).into());
            }
            file.sync_all()?;
        } else {
            return Err(UpdateError::WebArchivePath(output));
        }
    }
    let mut stream = archive.into_inner();
    io::copy(&mut stream, &mut io::sink())?;
    if stream.limit() == 0 {
        return Err(UpdateError::AssetSize);
    }
    if !has_index(destination)? {
        return Err(UpdateError::Archive("index.html"));
    }
    Ok(())
}

fn sync_dirs(root: &Path) -> io::Result<()> {
    for entry in root.read_dir()? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            sync_dirs(&entry.path())?;
        }
    }
    File::open(root)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::{Channel, Manifest, Version, select, unpack};

    fn manifest(version: &str, channel: Channel, minimum: &str) -> Result<Manifest, semver::Error> {
        Ok(Manifest {
            schema_version: 1,
            version: Version::parse(version)?,
            commit: "a".repeat(40),
            channel,
            min_rh_version: Version::parse(minimum)?,
            asset: format!("rivethub-web-{version}.tar.gz"),
            sha256: "0".repeat(64),
        })
    }

    #[test]
    fn selection_obeys_channel_minimum_runtime_and_semver_order() -> Result<(), semver::Error> {
        let manifests = [
            manifest("2.1.0-nightly.20261117.1", Channel::Nightly, "0.8.0")?,
            manifest("1.9.0", Channel::Stable, "0.7.0")?,
            manifest("2.0.0-beta.2", Channel::Beta, "0.8.0")?,
            manifest("2.0.0-beta.11", Channel::Beta, "0.8.0")?,
            manifest("3.0.0", Channel::Stable, "0.9.0")?,
        ];
        for (channel, runtime, expected) in [
            (Channel::Stable, "0.8.0", Some("1.9.0")),
            (Channel::Beta, "0.8.0", Some("2.0.0-beta.11")),
            (Channel::Nightly, "0.8.0", Some("2.1.0-nightly.20261117.1")),
            (Channel::Beta, "0.8.0-beta.1", Some("1.9.0")),
            (Channel::Nightly, "0.9.0", Some("3.0.0")),
            (Channel::Stable, "0.6.0", None),
        ] {
            let runtime = Version::parse(runtime)?;
            let actual = select(manifests.iter(), channel, &runtime);
            assert_eq!(
                actual
                    .map(|manifest| manifest.version.to_string())
                    .as_deref(),
                expected
            );
        }
        Ok(())
    }

    #[test]
    fn unpack_limits_total_decompressed_bytes() -> Result<(), Box<dyn std::error::Error>> {
        let mut archive = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        ));
        for path in ["index.html", "asset.js"] {
            let mut header = tar::Header::new_gnu();
            header.set_size(2048);
            header.set_mode(0o644);
            header.set_cksum();
            archive.append_data(&mut header, path, [0_u8; 2048].as_slice())?;
        }
        let bytes = archive.into_inner()?.finish()?;
        let staging = tempfile::tempdir()?;
        assert!(unpack(&bytes, staging.path(), 4096).is_err());
        Ok(())
    }
}
