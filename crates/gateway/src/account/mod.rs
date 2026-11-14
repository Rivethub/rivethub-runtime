mod auth;
mod cloud;
pub use auth::{AccountLogin, LoginProgress};
mod hubs;
mod install;
mod service;
mod telemetry;
mod update;

use url::Url;

pub use cloud::Cloud;
pub use hubs::{Hubs, SERVING_HUB};
pub use install::{AccountFiles, InstallState, Membership};
pub use service::{AccountService, UsageEvent};
pub use telemetry::{
    CrashJob, CrashKind, CrashReport, PanicSite, install_crash_hook, queue, queued,
};
pub use update::{
    CloudOs, RELEASE_KEY, ReleaseManifest, UpdateKey, VerifiedRelease, fetch, latest,
};

pub const DEFAULT_CLOUD: &str = "https://rivethub.cloud/";

#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    #[error("enter an email address")]
    Email,
    #[error("there is no sign-in waiting; start one first")]
    NoSignIn,
    #[error("the sign-in link expired; ask for a new one")]
    Expired,
    #[error("this install is not signed in")]
    NotSignedIn,
    #[error("this install is already signed in; sign out first")]
    SignedIn,
    #[error("rivethub.cloud no longer accepts this sign-in; sign in again")]
    SignedOutElsewhere,
    #[error("rivethub.cloud asked us to slow down; try again in a minute")]
    RateLimited,
    #[error("rivethub.cloud answered HTTP {0}")]
    CloudStatus(u16),
    #[error("rivethub.cloud sent a reply that does not parse")]
    CloudReply,
    #[error("could not reach rivethub.cloud: {0}")]
    Http(#[from] reqwest::Error),
    #[error("invalid cloud url: {0}")]
    Url(#[from] url::ParseError),
    #[error("crash reports and usage counts are turned off by this hub's policy")]
    Policy,
    #[error(
        "cloud sign-in is disabled by this hub's policy (cloud.disabled), so updates come from GitHub"
    )]
    CloudPolicy,
    #[error("the update key is not a base64 Ed25519 public key")]
    UpdateKey,
    #[error("the release manifest is invalid")]
    Manifest,
    #[error("the release signature does not verify against the update key")]
    Signature,
    #[error("the downloaded archive does not match the signed SHA-256")]
    Checksum,
    #[error("the download is too large")]
    TooLarge,
    #[error("rivethub.cloud publishes no release for this platform")]
    Platform,
    #[error("cancelled")]
    Cancelled,
    #[error("account state I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("account state does not parse: {0}")]
    State(#[from] serde_json::Error),
    #[error("account task failed: {0}")]
    Task(#[from] tokio::task::JoinError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudSettings {
    pub base: Url,
    pub telemetry: Url,
    pub update_key: UpdateKey,
    pub access: CloudAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloudAccess {
    Open,
    NoTelemetry,
    TelemetryOnly,
    Disabled,
}

impl CloudAccess {
    #[must_use]
    pub fn from_config(config: &config::Config) -> Self {
        match (config.cloud_disabled(), config.telemetry_disabled()) {
            (true, true) => Self::Disabled,
            (true, false) => Self::TelemetryOnly,
            (false, true) => Self::NoTelemetry,
            (false, false) => Self::Open,
        }
    }

    #[must_use]
    pub const fn telemetry(self) -> bool {
        matches!(self, Self::Open | Self::TelemetryOnly)
    }

    #[must_use]
    pub const fn cloud(self) -> bool {
        matches!(self, Self::Open | Self::NoTelemetry)
    }
}

impl CloudSettings {
    pub fn from_config(config: &config::Config) -> Result<Self, AccountError> {
        let section = config.cloud.clone().unwrap_or_default();
        let base = directory(match section.url {
            Some(url) => url,
            None => Url::parse(DEFAULT_CLOUD)?,
        });
        let telemetry = config
            .telemetry_url()
            .cloned()
            .map_or_else(|| base.clone(), directory);
        let update_key = match section.update_key.as_deref() {
            Some(text) => UpdateKey::parse(text)?,
            None => RELEASE_KEY,
        };
        Ok(Self {
            base,
            telemetry,
            update_key,
            access: CloudAccess::from_config(config),
        })
    }
}

fn directory(mut url: Url) -> Url {
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    url
}
