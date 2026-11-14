use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use crate::environment::Environment;

const DEFAULT_SHARED_DIR: &str = "/rivet-shared";
const SESSION_OWNERS_DIR: &str = "den";
pub const SESSION_OWNERS_FILE_NAME: &str = "session-owners.json";

#[must_use]
pub fn temporary_directory() -> PathBuf {
    std::env::temp_dir()
}

#[must_use]
pub fn session_owners_file(state_dir: &Path) -> PathBuf {
    state_dir
        .join(SESSION_OWNERS_DIR)
        .join(SESSION_OWNERS_FILE_NAME)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigLocation {
    Current(PathBuf),
    Legacy(PathBuf),
}

impl ConfigLocation {
    pub fn path(&self) -> &Path {
        match self {
            Self::Current(path) | Self::Legacy(path) => path,
        }
    }

    pub fn is_legacy(&self) -> bool {
        matches!(self, Self::Legacy(_))
    }
}

pub fn shared_dir(env: &Environment) -> PathBuf {
    PathBuf::from(
        env.configured_shared_dir()
            .map_or_else(|| DEFAULT_SHARED_DIR.to_owned(), str::to_owned),
    )
}

#[must_use]
pub fn normalize(path: &Path) -> PathBuf {
    let absolute = path.is_absolute();
    let mut parts: Vec<&OsStr> = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                let pop = parts.last().is_some_and(|last| *last != "..");
                if pop {
                    parts.pop();
                } else if !absolute {
                    parts.push(OsStr::new(".."));
                }
            }
            Component::Prefix(prefix) => parts.push(prefix.as_os_str()),
            Component::Normal(part) => parts.push(part),
        }
    }
    let mut normalized = PathBuf::new();
    if absolute {
        normalized.push(Component::RootDir.as_os_str());
    }
    for part in parts {
        normalized.push(part);
    }
    if !absolute && normalized.as_os_str().is_empty() {
        normalized.push(".");
    }
    normalized
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    home: PathBuf,
    state: Option<PathBuf>,
}

impl Paths {
    pub fn new(env: &Environment) -> Result<Self, crate::ConfigError> {
        let home = env
            .get("HOME")
            .map(PathBuf::from)
            .ok_or(crate::ConfigError::Home)?;
        if !absolute_without_parents(&home) {
            return Err(crate::ConfigError::Home);
        }
        let state = match env.text("RIVETHUB_HOME") {
            Some(text) => {
                let path = PathBuf::from(text);
                if !absolute_without_parents(&path) {
                    return Err(crate::ConfigError::StateHome);
                }
                Some(normalize(&path))
            }
            None => None,
        };
        Ok(Self {
            home: normalize(&home),
            state,
        })
    }

    pub fn home(&self) -> &Path {
        &self.home
    }

    pub fn hub_home(&self) -> PathBuf {
        self.state
            .clone()
            .unwrap_or_else(|| self.home.join(".rivethub"))
    }

    pub fn legacy_home(&self) -> PathBuf {
        self.home.join(".rivetos")
    }

    pub fn state_dir(&self) -> PathBuf {
        self.hub_home()
    }

    pub fn session_owners_file(&self) -> PathBuf {
        session_owners_file(&self.state_dir())
    }

    pub fn discovery_dir(&self) -> PathBuf {
        self.state_dir().join("discovery")
    }

    pub fn ca_dir(&self) -> PathBuf {
        self.state_dir().join("ca")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.state_dir().join("logs")
    }

    pub fn env_file(&self) -> PathBuf {
        self.state_dir().join(".env")
    }

    pub fn pid_file(&self) -> PathBuf {
        self.state_dir().join(PID_FILE_NAME)
    }

    pub fn owner_lock(&self, data_dir: &Path) -> PathBuf {
        data_dir.join("rivethub-owner.lock")
    }

    pub fn config_candidates(&self) -> Vec<ConfigLocation> {
        if let Some(state) = &self.state {
            return vec![
                ConfigLocation::Current(state.join("config.yaml")),
                ConfigLocation::Current(state.join("config.yml")),
                ConfigLocation::Current(PathBuf::from("config.yaml")),
            ];
        }
        let base = &self.home;
        vec![
            ConfigLocation::Current(base.join(".rivethub").join("config.yaml")),
            ConfigLocation::Current(base.join(".rivethub").join("config.yml")),
            ConfigLocation::Legacy(base.join(".rivetos").join("config.yaml")),
            ConfigLocation::Legacy(base.join(".rivetos").join("config.yml")),
            ConfigLocation::Current(PathBuf::from("config.yaml")),
        ]
    }
}

pub(crate) fn expand_home(
    env: &Environment,
    path: &Path,
) -> Result<Option<PathBuf>, crate::ConfigError> {
    let mut components = path.components();
    if components.next() != Some(Component::Normal(OsStr::new("~"))) {
        return Ok(None);
    }
    let home = Paths::new(env)?.home().to_path_buf();
    let rest = components.as_path();
    if rest.as_os_str().is_empty() {
        Ok(Some(home))
    } else {
        Ok(Some(home.join(rest)))
    }
}

pub const PID_FILE_NAME: &str = "rivethub.pid";

#[derive(Debug, thiserror::Error)]
pub enum StateError {
    #[error(transparent)]
    Config(#[from] crate::ConfigError),
    #[error("refusing to use state directory {path}: it is a symlink")]
    Symlink { path: PathBuf },
    #[error("refusing to move legacy state: live process {pid} owns {path}")]
    LiveOwner { path: PathBuf, pid: u32 },
    #[error("a RivetHub node is already running (pid {pid})")]
    AlreadyRunning { pid: u32 },
    #[error("refusing to move legacy state: owner lock is held at {path}")]
    Locked { path: PathBuf },
    #[error("refusing to move legacy state: invalid owner PID in {path}")]
    InvalidOwner { path: PathBuf },
    #[error("failed to inspect state path {path}: {source}")]
    Inspect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to move legacy state {from} to {to}: {source}")]
    Move {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Debug)]
pub struct NodeClaim {
    file: std::fs::File,
    path: PathBuf,
}

impl NodeClaim {
    pub(super) fn hold(file: std::fs::File, path: PathBuf) -> Self {
        Self { file, path }
    }

    pub fn release(self) -> Result<(), StateError> {
        let path = self.path;
        let removed = std::fs::remove_file(&path);
        drop(self.file);
        match removed {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(StateError::Inspect { path, source }),
        }
    }
}

pub fn claim_node(directory: &Path, pid: u32) -> Result<NodeClaim, StateError> {
    state_owner::claim(directory, pid)
}

fn absolute_without_parents(path: &Path) -> bool {
    path.is_absolute()
        && !path
            .components()
            .any(|component| component == Component::ParentDir)
}

pub fn adopt_legacy_state(env: &Environment) -> Result<Option<String>, StateError> {
    if env.text("RIVETHUB_HOME").is_some() {
        return Ok(None);
    }
    let paths = Paths::new(env)?;
    let hub = paths.hub_home();
    let legacy = paths.legacy_home();
    if metadata(&hub)?.is_some() {
        return Ok(None);
    }
    let Some(legacy_meta) = metadata(&legacy)? else {
        return Ok(None);
    };
    if legacy_meta.file_type().is_symlink() {
        return Err(StateError::Symlink { path: legacy });
    }
    if !legacy_meta.is_dir() {
        return Ok(None);
    }
    let _locks = state_owner::check(&legacy)?;
    match std::fs::rename(&legacy, &hub) {
        Ok(()) => Ok(Some(format!(
            "moved legacy state {} to {} once at first start",
            legacy.display(),
            hub.display()
        ))),
        Err(source) => Err(StateError::Move {
            from: legacy,
            to: hub,
            source,
        }),
    }
}

fn metadata(path: &Path) -> Result<Option<std::fs::Metadata>, StateError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => Ok(Some(metadata)),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(StateError::Inspect {
            path: path.to_path_buf(),
            source,
        }),
    }
}

mod state_owner;

impl crate::Config {
    pub fn wiki_directory(&self, env: &Environment) -> PathBuf {
        if let Some(path) = self
            .memory
            .as_ref()
            .and_then(|memory| memory.sqlite.as_ref())
            .and_then(|sqlite| sqlite.wiki_dir.as_ref())
        {
            return normalize(path);
        }
        if let Some(path) = env.get("WIKI_DIR") {
            return normalize(Path::new(path));
        }
        shared_dir(env).join("wiki")
    }
}
