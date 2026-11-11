use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, thiserror::Error)]
pub enum SettingsError {
    #[error(transparent)]
    Config(#[from] config::ConfigError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub hostname: String,
    pub node: String,
    pub allowed_origins: Vec<String>,
    pub allowed_hosts: Vec<String>,
    pub static_dir: Option<PathBuf>,
    pub root_redirect: Option<String>,
    pub state_dir: PathBuf,
    pub private_ca: Option<PathBuf>,
    pub request_deadline: Duration,
}

pub const REQUEST_DEADLINE: Duration = Duration::from_secs(30);

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
        let state_dir = state_dir(env)?;
        let shared_dir = canonical_dir(config::shared_dir(env));
        let private_ca = crate::tls_resolve::private_ca(env, &shared_dir);
        Ok(Self {
            hostname,
            node,
            allowed_origins: section.allowed_origins,
            allowed_hosts: section.allowed_hosts,
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
            state_dir,
            private_ca,
            request_deadline: section
                .request_deadline_secs
                .map_or(REQUEST_DEADLINE, |secs| Duration::from_secs(secs.get())),
        })
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
