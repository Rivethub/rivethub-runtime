use std::time::Duration;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use strum::Display;
use strum::EnumString;
use thiserror::Error;

pub const STALE_AFTER: Duration = Duration::from_hours(72);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum CaptureMethod {
    Hook,
    Plugin,
    StoreScan,
    ClientPush,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CaptureState {
    Receiving,
    Stale,
    NotConnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecallState {
    Connected,
    NotConnected,
    Elsewhere,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display, EnumString)]
#[serde(rename_all = "lowercase")]
#[strum(serialize_all = "lowercase")]
pub enum ConnectPart {
    Capture,
    Recall,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureActivity {
    pub key_prefix: String,
    pub last_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureHistory {
    Unknown,
    Known(Option<DateTime<Utc>>),
}

impl CaptureHistory {
    #[must_use]
    pub fn of(activity: &[CaptureActivity], prefix: &str) -> Self {
        Self::Known(
            activity
                .iter()
                .filter(|entry| entry.key_prefix == prefix)
                .map(|entry| entry.last_at)
                .max(),
        )
    }

    fn last(self) -> Option<DateTime<Utc>> {
        match self {
            Self::Unknown => None,
            Self::Known(last) => last,
        }
    }

    fn phrase(self, found: bool, capture_installed: bool) -> Option<String> {
        if !found {
            return None;
        }
        match self {
            Self::Known(None) if capture_installed => Some("nothing captured yet".to_owned()),
            Self::Unknown | Self::Known(None) => None,
            Self::Known(Some(at)) => Some(format!(
                "last capture {}",
                at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
            )),
        }
    }
}

fn detail_with_history(
    install_detail: &str,
    found: bool,
    capture_installed: bool,
    history: CaptureHistory,
) -> String {
    match history.phrase(found, capture_installed) {
        Some(phrase) => format!("{install_detail}; {phrase}"),
        None => install_detail.to_owned(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecallStatus {
    pub state: RecallState,
    pub detail: String,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureStatus {
    pub method: CaptureMethod,
    pub state: CaptureState,
    pub last_capture_at: Option<DateTime<Utc>>,
    pub detail: String,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HarnessStatus {
    pub id: String,
    pub name: String,
    pub found: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub capture: CaptureStatus,
    pub recall: RecallStatus,
    pub can_connect: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessProbe {
    pub id: String,
    pub name: String,
    pub session_prefix: String,
    pub found: bool,
    pub path: Option<String>,
    pub version: Option<String>,
    pub method: CaptureMethod,
    pub capture_installed: bool,
    pub install_detail: String,
    pub recall: RecallStatus,
    pub can_connect: bool,
}

impl HarnessProbe {
    #[must_use]
    pub fn into_status(self, history: CaptureHistory, now: DateTime<Utc>) -> HarnessStatus {
        let last = history.last();
        let recent = last.is_some_and(|at| {
            now.signed_duration_since(at)
                .to_std()
                .is_ok_and(|age| age <= STALE_AFTER)
                || at > now
        });
        let state = if recent {
            CaptureState::Receiving
        } else if self.capture_installed {
            CaptureState::Stale
        } else {
            CaptureState::NotConnected
        };
        HarnessStatus {
            id: self.id,
            name: self.name,
            found: self.found,
            path: self.path,
            version: self.version,
            capture: CaptureStatus {
                method: self.method,
                state,
                last_capture_at: last,
                detail: detail_with_history(
                    &self.install_detail,
                    self.found,
                    self.capture_installed,
                    history,
                ),
            },
            recall: self.recall,
            can_connect: self.can_connect,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectOutcome {
    pub diff: String,
    pub backup_path: String,
}

#[derive(Debug, Error)]
pub enum SetupError {
    #[error("unknown harness {0}")]
    UnknownHarness(String),
    #[error("{id} has no {part} setup rh can write: {reason}")]
    Unsupported {
        id: String,
        part: ConnectPart,
        reason: &'static str,
    },
    #[error("{path}: {message}")]
    Config { path: String, message: String },
    #[error("{path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
}

#[async_trait]
pub trait HarnessSetup: Send + Sync {
    async fn probe(&self) -> Result<Vec<HarnessProbe>, SetupError>;

    async fn connect(
        &self,
        id: &str,
        part: ConnectPart,
        dry_run: bool,
    ) -> Result<ConnectOutcome, SetupError>;
}
