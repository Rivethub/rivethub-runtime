use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use wire::JsonObject;
use wire::plugin::manifest::{Permission, PluginManifest};
use wire::plugin::view::ViewValue;
use wire::plugin::{PluginId, PluginNameError};

use super::HostError;

pub(super) const MANIFEST_FILE: &str = "rivethub-plugin.json";
const REGISTRY_FILE: &str = "registry.json";
const UNREADABLE_FILE: &str = "registry.unreadable.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Stored {
    pub(super) path: PathBuf,
    pub(super) manifest: PluginManifest,
    pub(super) enabled: bool,
    pub(super) granted: Vec<Permission>,
    pub(super) installed_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(super) settings: BTreeMap<String, ViewValue>,
}

#[derive(Debug, Serialize)]
struct RegistryFile {
    plugins: BTreeMap<PluginId, Stored>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct RawRegistry {
    #[serde(default)]
    plugins: BTreeMap<String, JsonObject>,
}

#[derive(Debug, thiserror::Error)]
enum Unreadable {
    #[error(transparent)]
    Id(#[from] PluginNameError),
    #[error(transparent)]
    Entry(#[from] serde_json::Error),
}

pub(super) async fn load(root: &Path) -> Result<BTreeMap<PluginId, Stored>, HostError> {
    let bytes = match tokio::fs::read(root.join(REGISTRY_FILE)).await {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(error.into()),
    };
    let mut plugins = BTreeMap::new();
    let mut unreadable = RawRegistry::default();
    for (name, entry) in serde_json::from_slice::<RawRegistry>(&bytes)?.plugins {
        match stored(&name, &entry) {
            Ok((id, stored)) => {
                plugins.insert(id, stored);
            }
            Err(error) => {
                tracing::warn!(
                    plugin = %name,
                    %error,
                    "a plug-in in the registry cannot be read by this rh; it is left out and kept in registry.unreadable.json"
                );
                unreadable.plugins.insert(name, entry);
            }
        }
    }
    if !unreadable.plugins.is_empty() {
        write_private(
            &root.join(UNREADABLE_FILE),
            &serde_json::to_vec_pretty(&unreadable)?,
        )
        .await?;
    }
    Ok(plugins)
}

fn stored(name: &str, entry: &JsonObject) -> Result<(PluginId, Stored), Unreadable> {
    let id = PluginId::try_from(name.to_owned())?;
    Ok((id, entry.deserialize::<Stored>()?))
}

pub(super) async fn save(
    root: &Path,
    plugins: BTreeMap<PluginId, Stored>,
) -> Result<(), HostError> {
    let bytes = serde_json::to_vec_pretty(&RegistryFile { plugins })?;
    private_folder(root).await?;
    write_private(&root.join(REGISTRY_FILE), &bytes).await?;
    Ok(())
}

pub(super) async fn private_folder(root: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt as _;
    tokio::fs::create_dir_all(root).await?;
    tokio::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700)).await
}

pub(super) async fn write_private(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    let mut staged = path.as_os_str().to_owned();
    staged.push(".tmp");
    let staged = PathBuf::from(staged);
    tokio::fs::write(&staged, bytes).await?;
    set_private(&staged).await?;
    tokio::fs::rename(&staged, path).await
}

pub(super) async fn set_private(path: &Path) -> Result<(), std::io::Error> {
    use std::os::unix::fs::PermissionsExt as _;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await
}

pub(super) async fn read_manifest(dir: &Path) -> Result<PluginManifest, HostError> {
    let file = dir.join(MANIFEST_FILE);
    let bytes = tokio::fs::read(&file)
        .await
        .map_err(|error| HostError::Invalid(format!("{}: {error}", file.display())))?;
    let mut decoder = serde_json::Deserializer::from_slice(&bytes);
    let manifest: PluginManifest = serde_path_to_error::deserialize(&mut decoder)
        .map_err(|error| HostError::Invalid(format!("{}: {error}", error.path())))?;
    super::manifest::check_manifest(&manifest)
        .map_err(|problem| HostError::Invalid(problem.to_string()))?;
    super::jobs::check_schedules(&manifest)?;
    Ok(manifest)
}
