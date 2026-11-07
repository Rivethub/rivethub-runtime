use std::path::PathBuf;
use std::time::Duration;

use wire::harness::HarnessId;

const DEFAULT_PROBE_TIMEOUT: Duration = Duration::from_millis(1_500);
const DEFAULT_MESH_CACHE_TTL: Duration = Duration::from_secs(10);

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error("{variable}={text} is not a millisecond count")]
    MeshCache {
        variable: &'static str,
        text: String,
        #[source]
        source: std::num::ParseIntError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub hostname: String,
    pub node: String,
    pub allowed_origins: Vec<String>,
    pub allowed_hosts: Vec<String>,
    pub files_root: Option<PathBuf>,
    pub static_dir: Option<PathBuf>,
    pub root_redirect: Option<String>,
    pub state_dir: PathBuf,
    pub directory_root: PathBuf,
    pub mesh_file: Option<PathBuf>,
    pub shared_dir: PathBuf,
    pub legacy_mesh_dir: PathBuf,
    pub allowed_harnesses: Option<Vec<HarnessId>>,
    pub devices_enabled: bool,
    pub probe_timeout: Duration,
    pub mesh_ca: Option<PathBuf>,
    pub mesh_cache_ttl: Duration,
}

pub fn state_dir(env: &config::Environment) -> Result<PathBuf, config::ConfigError> {
    Ok(canonical_dir(
        match configured_path(env, "RIVETHUB_STATE_DIR", "RIVETOS_DEN_STATE_DIR") {
            Some(path) => path,
            None => config::Paths::new(env)?.state_dir(),
        },
    ))
}

impl Settings {
    pub fn from_config_in(
        config: &config::Config,
        env: &config::Environment,
        hostname: String,
        node: String,
    ) -> Result<Self, SettingsError> {
        let section = config.gateway.clone().unwrap_or_default();
        let paths = config::Paths::new(env)?;
        let state_dir = state_dir(env)?;
        let shared_dir = canonical_dir(config::shared_dir(env));
        let mesh_ca = crate::tls_resolve::mesh_ca(Some(&section), env, &shared_dir);
        Ok(Self {
            hostname,
            node,
            allowed_origins: section.allowed_origins,
            allowed_hosts: section.allowed_hosts,
            files_root: configured_files_root(section.files_root),
            static_dir: match configured_path(env, "RIVETHUB_STATIC_DIR", "RIVETOS_DEN_STATIC_DIR")
            {
                Some(path) => Some(path),
                None => section.static_dir,
            },
            root_redirect: match configured_text(
                env,
                "RIVETHUB_ROOT_REDIRECT",
                "RIVETOS_DEN_ROOT_REDIRECT",
            ) {
                Some(target) => Some(target),
                None => section.root_redirect,
            },
            directory_root: state_dir.join("agents"),
            state_dir,
            mesh_file: configured_path(env, "RIVETHUB_MESH_FILE", "RIVETOS_DEN_MESH_FILE"),
            shared_dir,
            legacy_mesh_dir: paths.legacy_home(),
            allowed_harnesses: section
                .allowed_harnesses
                .map(|entries| entries.iter().filter_map(known_harness).collect()),
            devices_enabled: section.devices.enabled == Some(true),
            probe_timeout: DEFAULT_PROBE_TIMEOUT,
            mesh_ca,
            mesh_cache_ttl: mesh_cache_ttl(env)?,
        })
    }
}

fn known_harness(slot: &config::HarnessSlot) -> Option<HarnessId> {
    match slot {
        config::HarnessSlot::Known(id) => Some(*id),
        config::HarnessSlot::Unknown(_) => None,
    }
}

fn canonical_dir(path: PathBuf) -> PathBuf {
    let mut missing = Vec::new();
    let mut cursor = path.clone();
    loop {
        match std::fs::canonicalize(&cursor) {
            Ok(mut found) => {
                for name in missing.into_iter().rev() {
                    found.push(name);
                }
                return found;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let Some(name) = cursor.file_name().map(std::ffi::OsStr::to_os_string) else {
                    return path;
                };
                missing.push(name);
                if !cursor.pop() {
                    return path;
                }
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    path = %path.display(),
                    "agent path could not be canonicalized"
                );
                return path;
            }
        }
    }
}

fn configured_text(env: &config::Environment, current: &str, legacy: &str) -> Option<String> {
    if let Some(notice) = env.legacy_notice(current, legacy) {
        tracing::warn!("{notice}");
    }
    let text = match env.text(current) {
        Some(value) => Some(value),
        None => env.text(legacy),
    };
    text.map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn configured_path(env: &config::Environment, current: &str, legacy: &str) -> Option<PathBuf> {
    configured_text(env, current, legacy).map(PathBuf::from)
}

fn mesh_cache_ttl(env: &config::Environment) -> Result<Duration, SettingsError> {
    if let Some(notice) = env.legacy_notice("RIVETHUB_MESH_CACHE_MS", "RIVETOS_DEN_MESH_CACHE_MS") {
        tracing::warn!("{notice}");
    }
    if let Some(text) = nonempty(env.text("RIVETHUB_MESH_CACHE_MS")) {
        return parse_cache_millis("RIVETHUB_MESH_CACHE_MS", text);
    }
    if let Some(text) = nonempty(env.text("RIVETOS_DEN_MESH_CACHE_MS")) {
        return parse_cache_millis("RIVETOS_DEN_MESH_CACHE_MS", text);
    }
    Ok(DEFAULT_MESH_CACHE_TTL)
}

fn nonempty(text: Option<&str>) -> Option<&str> {
    text.map(str::trim).filter(|value| !value.is_empty())
}

fn parse_cache_millis(variable: &'static str, text: &str) -> Result<Duration, SettingsError> {
    match text.parse::<u64>() {
        Ok(millis) => Ok(Duration::from_millis(millis)),
        Err(source) => Err(SettingsError::MeshCache {
            variable,
            text: text.to_owned(),
            source,
        }),
    }
}

fn configured_files_root(root: Option<PathBuf>) -> Option<PathBuf> {
    match root {
        Some(path) if !path.as_os_str().is_empty() => Some(path),
        Some(_) | None => None,
    }
}
