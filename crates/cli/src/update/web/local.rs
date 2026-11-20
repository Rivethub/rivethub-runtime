use std::fs::File;
use std::io::Read as _;
use std::path::{Path, PathBuf};

use semver::Version;

use super::{DOWNLOAD_LIMIT, MANIFEST, MANIFEST_LIMIT, Manifest, Outcome, Store, verify_installed};
use crate::update::error::NeedsRuntime;
use crate::update::{UpdateArgs, UpdateError};

#[derive(Debug)]
pub(in crate::update) struct Local {
    archive: PathBuf,
    manifest: PathBuf,
    digest: [u8; 32],
}

impl Local {
    pub(in crate::update) fn parse(args: &UpdateArgs) -> Result<Option<Self>, UpdateError> {
        let Some(archive) = &args.from else {
            return Ok(None);
        };
        let mut digest = [0; 32];
        let pinned = args.sha256.as_deref().ok_or(UpdateError::WebPin)?;
        hex::decode_to_slice(pinned, &mut digest).map_err(|_| UpdateError::WebPin)?;
        let manifest = match &args.manifest {
            Some(path) => path.clone(),
            None => archive.with_file_name(MANIFEST),
        };
        Ok(Some(Self {
            archive: archive.clone(),
            manifest,
            digest,
        }))
    }
}

impl Store {
    pub(in crate::update) fn install_local(
        &self,
        local: &Local,
        runtime: &str,
    ) -> Result<Outcome, UpdateError> {
        let runtime = Version::parse(runtime).map_err(|_| UpdateError::Version(runtime.into()))?;
        let described =
            limited(&local.manifest, u64::from(MANIFEST_LIMIT))?.ok_or(UpdateError::WebManifest)?;
        let manifest: Manifest =
            serde_json::from_slice(&described).map_err(|_| UpdateError::WebManifest)?;
        manifest.validate(&manifest.version, manifest.channel)?;
        if manifest.digest()? != local.digest {
            return Err(UpdateError::Checksum(manifest.asset));
        }
        if manifest.min_rh_version.cmp_precedence(&runtime).is_gt() {
            return Err(UpdateError::WebNeedsRuntime(Box::new(NeedsRuntime {
                web: manifest.version,
                needs: manifest.min_rh_version,
                runtime,
            })));
        }
        if self.active()?.as_ref() == Some(&manifest.version) {
            verify_installed(&self.root.join(manifest.version.to_string()), &manifest)?;
            return Ok(Outcome::Unchanged(manifest.version));
        }
        let bytes =
            limited(&local.archive, DOWNLOAD_LIMIT as u64)?.ok_or(UpdateError::AssetSize)?;
        self.install(&manifest, &bytes)?;
        Ok(Outcome::Installed(manifest.version))
    }
}

fn limited(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, UpdateError> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    Ok((u64::try_from(bytes.len()).is_ok_and(|read| read <= limit)).then_some(bytes))
}
