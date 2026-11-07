use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;
use wire::gateway::account::{PendingUpdate, ReleaseChannel, UpdateSource};

use super::AccountError;

const FOLDER: &str = "account";
const INSTALL_FILE: &str = "install.json";
const CRASH_FOLDER: &str = "crashes";
const UPDATE_FOLDER: &str = "updates";

#[derive(Debug, Clone)]
pub struct AccountFiles {
    root: PathBuf,
}

impl AccountFiles {
    #[must_use]
    pub fn new(state_dir: &Path) -> Self {
        Self {
            root: state_dir.join(FOLDER),
        }
    }

    #[must_use]
    pub fn install(&self) -> PathBuf {
        self.root.join(INSTALL_FILE)
    }

    #[must_use]
    pub fn crashes(&self) -> PathBuf {
        self.root.join(CRASH_FOLDER)
    }

    #[must_use]
    pub fn updates(&self) -> PathBuf {
        self.root.join(UPDATE_FOLDER)
    }

    pub fn read(&self) -> Result<Option<InstallState>, AccountError> {
        match std::fs::read(self.install()) {
            Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    pub fn open(&self) -> Result<InstallState, AccountError> {
        if let Some(state) = self.read()? {
            return Ok(state);
        }
        let state = InstallState::fresh();
        self.save(&state)?;
        Ok(state)
    }

    pub fn save(&self, state: &InstallState) -> Result<(), AccountError> {
        write_private(&self.install(), &serde_json::to_vec_pretty(state)?)
    }
}

pub(crate) fn private_folder(path: &Path) -> Result<(), AccountError> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    Ok(())
}

pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<(), AccountError> {
    if let Some(folder) = path.parent() {
        private_folder(folder)?;
    }
    let mut staged = path.as_os_str().to_owned();
    staged.push(format!(".{}.tmp", Uuid::new_v4().simple()));
    let staged = PathBuf::from(staged);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&staged)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    std::fs::rename(&staged, path)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallState {
    pub install_id: Uuid,
    pub crash_reports: bool,
    pub usage_counts: bool,
    pub channel: ReleaseChannel,
    pub auto_download: bool,
    #[serde(default)]
    pub update_source: Option<UpdateSource>,
    #[serde(default)]
    pub membership: Membership,
    #[serde(default)]
    pub pending_update: Option<PendingUpdate>,
    #[serde(default)]
    pub usage: Option<UsageDay>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "kebab-case")]
pub enum Membership {
    #[default]
    SignedOut,
    Pending {
        request_id: String,
    },
    SignedIn {
        email: String,
        token: String,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageDay {
    pub day: NaiveDate,
    pub searches: u64,
    pub app_opens: u64,
}

impl InstallState {
    fn fresh() -> Self {
        Self {
            install_id: Uuid::new_v4(),
            crash_reports: true,
            usage_counts: true,
            channel: ReleaseChannel::Stable,
            auto_download: true,
            update_source: None,
            membership: Membership::SignedOut,
            pending_update: None,
            usage: None,
        }
    }

    #[must_use]
    pub fn token(&self) -> Option<&str> {
        match &self.membership {
            Membership::SignedIn { token, .. } => Some(token),
            Membership::SignedOut | Membership::Pending { .. } => None,
        }
    }

    #[must_use]
    pub fn email(&self) -> Option<&str> {
        match &self.membership {
            Membership::SignedIn { email, .. } => Some(email),
            Membership::SignedOut | Membership::Pending { .. } => None,
        }
    }

    #[must_use]
    pub fn update_source(&self) -> UpdateSource {
        match (&self.membership, self.update_source) {
            (Membership::SignedIn { .. }, Some(source)) => source,
            (Membership::SignedIn { .. }, None) => UpdateSource::RivethubCloud,
            (Membership::SignedOut | Membership::Pending { .. }, _) => UpdateSource::Github,
        }
    }
}
