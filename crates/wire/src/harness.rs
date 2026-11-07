use crate::Extra;
use crate::session_id::SessionId;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use std::fmt;
use std::str::FromStr;
use strum::{Display, EnumIter, EnumString};
use thiserror::Error;

mod driver;
mod prompt;

pub use driver::{SessionDriver, TerminalApproval};
pub use prompt::{PromptKind, PromptOption, PromptQuestion, PromptResolution, PromptScreen};
pub use values::HarnessId;

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
pub enum SessionHarness {
    Harness(HarnessId),
    Rivetos,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unknown harness id: {0}")]
pub struct SessionHarnessError(String);

impl TryFrom<String> for SessionHarness {
    type Error = SessionHarnessError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        if value == "rivetos" {
            return Ok(Self::Rivetos);
        }
        HarnessId::from_str(&value)
            .map(Self::Harness)
            .map_err(|_| SessionHarnessError(value))
    }
}

impl From<SessionHarness> for String {
    fn from(value: SessionHarness) -> Self {
        value.to_string()
    }
}

impl fmt::Display for SessionHarness {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Harness(harness) => harness.fmt(f),
            Self::Rivetos => f.write_str("rivetos"),
        }
    }
}

impl From<HarnessId> for SessionHarness {
    fn from(value: HarnessId) -> Self {
        Self::Harness(value)
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    Display,
    EnumString,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum RosterCommand {
    Claude,
    Grok,
    Kimi,
    Opencode,
    Hermes,
    Codex,
    Omp,
    Dsh,
    Cursor,
    Cowork,
}

impl From<HarnessId> for RosterCommand {
    fn from(harness: HarnessId) -> Self {
        match harness {
            HarnessId::ClaudeCode => Self::Claude,
            HarnessId::GrokBuild => Self::Grok,
            HarnessId::KimiCode => Self::Kimi,
            HarnessId::Hermes => Self::Hermes,
            HarnessId::Codex => Self::Codex,
            HarnessId::Opencode => Self::Opencode,
            HarnessId::Omp => Self::Omp,
            HarnessId::Dsh => Self::Dsh,
            HarnessId::Cursor => Self::Cursor,
            HarnessId::Cowork => Self::Cowork,
        }
    }
}

impl From<RosterCommand> for HarnessId {
    fn from(command: RosterCommand) -> Self {
        match command {
            RosterCommand::Claude => Self::ClaudeCode,
            RosterCommand::Grok => Self::GrokBuild,
            RosterCommand::Kimi => Self::KimiCode,
            RosterCommand::Hermes => Self::Hermes,
            RosterCommand::Codex => Self::Codex,
            RosterCommand::Opencode => Self::Opencode,
            RosterCommand::Omp => Self::Omp,
            RosterCommand::Dsh => Self::Dsh,
            RosterCommand::Cursor => Self::Cursor,
            RosterCommand::Cowork => Self::Cowork,
        }
    }
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    Display,
    EnumString,
    EnumIter,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum HarnessCode {
    InvalidSessionId,
    BadRequest,
    SessionIdCollision,
    TurnInFlight,
    CapabilityUnsupported,
    UnknownApproval,
    UnknownPrompt,
    TurnFailed,
    SessionRunningElsewhere,
}

impl HarnessCode {
    #[must_use]
    pub const fn status(self) -> u16 {
        match self {
            Self::InvalidSessionId | Self::BadRequest => 400,
            Self::UnknownApproval | Self::UnknownPrompt => 404,
            Self::SessionIdCollision | Self::TurnInFlight | Self::SessionRunningElsewhere => 409,
            Self::CapabilityUnsupported => 501,
            Self::TurnFailed => 502,
        }
    }

    #[must_use]
    pub const fn retryable(self) -> bool {
        matches!(self, Self::TurnInFlight)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum SteerOutcome {
    Steered,
    StartedNewTurn,
    PromptRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ApprovalDecision {
    Allow,
    Deny,
    AllowSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum HarnessStatus {
    Working,
    Blocked,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PlanEntry {
    pub content: String,
    pub status: PlanStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessCapabilities {
    #[serde(flatten)]
    pub control: HarnessControlCapabilities,
    #[serde(flatten)]
    pub observation: HarnessObservationCapabilities,
    pub turn_options: Option<bool>,
    pub launch_model: Option<bool>,
    pub image_attachments: Option<bool>,
    pub models: Option<Vec<HarnessModelOption>>,
    pub models_source: Option<HarnessModelsSource>,
    pub efforts: Option<Vec<EffortOption>>,
    pub model_flag: Option<String>,
    pub effort_flag: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessControlCapabilities {
    pub interrupt: bool,
    pub resume: bool,
    pub approvals: bool,
    pub steering: Option<bool>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessObservationCapabilities {
    pub live_stream: bool,
    pub list_sessions: bool,
    pub drive: Option<bool>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EffortOption {
    pub id: String,
    pub label: String,
    pub is_default: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum HarnessModelsSource {
    Discovered,
    Config,
    Merged,
    Static,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessModelOption {
    pub input_modalities: Option<Vec<String>>,
    pub id: String,
    pub label: String,
    pub is_default: Option<bool>,
    pub efforts: Option<Vec<EffortOption>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum HarnessSessionStatus {
    Active,
    Idle,
    Ended,
    Error,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessSessionSummary {
    pub session_id: SessionId,
    pub harness_id: HarnessId,
    pub title: Option<String>,
    pub cwd: Option<String>,
    #[serde_as(as = "crate::time::Millis")]
    pub created_at: DateTime<Utc>,
    #[serde_as(as = "crate::time::Millis")]
    pub updated_at: DateTime<Utc>,
    pub status: HarnessSessionStatus,
    pub blocked: Option<bool>,
    pub supersedes: Option<SessionId>,
    pub model: Option<String>,
    pub parent_session_id: Option<SessionId>,
    pub agent_name: Option<String>,
    pub task_id: Option<String>,
    pub effort: Option<String>,
    pub transport: Option<Transport>,
    #[serde(flatten, default)]
    pub extra: Extra,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Transport {
    Protocol,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessSessionList {
    pub sessions: Vec<HarnessSessionSummary>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct HarnessOffering {
    pub harness_id: HarnessId,
    pub chat: bool,
    pub capabilities: HarnessCapabilities,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StoreHarnessCapabilities {
    pub list_sessions: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StoreHarness {
    pub harness_id: HarnessId,
    pub capabilities: StoreHarnessCapabilities,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StoreHarnessList {
    pub harnesses: Vec<StoreHarness>,
}

#[must_use]
pub fn model_id(value: &str) -> std::borrow::Cow<'_, str> {
    match serde_json::from_str::<Vec<String>>(value) {
        Ok(parts) if !parts.is_empty() && parts.iter().all(|part| !part.is_empty()) => {
            std::borrow::Cow::Owned(parts.join("/"))
        }
        _ => std::borrow::Cow::Borrowed(value),
    }
}
