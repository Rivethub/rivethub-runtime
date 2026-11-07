use crate::BoxError;
use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::mpsc::Receiver;
use values::HarnessId;
use values::JsonObject;
use values::SessionId;
use values::ThinkingLevel;

mod offering;

pub use offering::{
    EffortOption, HarnessCapabilities, HarnessControlCapabilities, HarnessModelOption,
    HarnessModelsSource, HarnessObservationCapabilities, HarnessOffering,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SessionSwitches {
    pub model: bool,
    pub effort: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalDecision {
    Allow,
    Deny,
    AllowSession,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteerOutcome {
    Steered,
    StartedNewTurn,
    PromptRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanStatus {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Error)]
pub enum HarnessAdapterError {
    #[error("harness does not support releasing session ownership")]
    ReleaseUnsupported,
    #[error("harness cannot steer a running turn")]
    SteeringUnsupported,
    #[error("harness rejected the session id: {0}")]
    InvalidSession(SessionId),
    #[error("harness session is unknown: {0}")]
    UnknownSession(SessionId),
    #[error("harness turn is already in flight: {0}")]
    TurnInFlight(SessionId),
    #[error("harness event stream was already taken: {0}")]
    EventStreamTaken(SessionId),
    #[error("the harness does not offer a {option} option")]
    OptionUnavailable { option: String },
    #[error("the harness does not offer {value} as its {option}")]
    OptionRejected { option: String, value: String },
    #[error("harness failed: {message}")]
    Failed {
        message: String,
        #[source]
        source: BoxError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSpec {
    pub session: SessionId,
    pub harness: HarnessId,
    pub model: Option<String>,
    pub effort: Option<ThinkingLevel>,
    pub working_dir: Option<std::path::PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRequest {
    pub request_id: String,
    pub tool_call_id: Option<String>,
    pub name: String,
    pub input: JsonObject,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallInfo {
    pub tool_call_id: String,
    pub name: String,
    pub input: JsonObject,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCallStatus {
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCallUpdate {
    pub tool_call_id: String,
    pub status: ToolCallStatus,
    pub output: Option<String>,
    pub title: Option<String>,
    pub input: Option<JsonObject>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanEntry {
    pub content: String,
    pub priority: PlanPriority,
    pub status: PlanStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanPriority {
    High,
    Medium,
    Low,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessCommands {
    pub harness: HarnessId,
    pub commands: Vec<Command>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    UserMessageChunk { text: String },
    AgentMessageChunk { text: String },
    AgentThoughtChunk { text: String },
    Usage { usage: values::LlmUsage },
    Context(ContextUsage),
    ToolCall(ToolCallInfo),
    ToolCallUpdate(ToolCallUpdate),
    Plan(Vec<PlanEntry>),
    AvailableCommands(Vec<Command>),
    CurrentMode { mode: String },
    TurnEnded,
    Exited { code: Option<i32> },
    Permission(PermissionRequest),
    TerminalBytes { data: Vec<u8> },
    TerminalResize { cols: u16, rows: u16 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ContextUsage {
    pub used: u64,
    pub window: Option<u64>,
    pub source: ContextUsageSource,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextUsageSource {
    Harness,
    LastCall,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TurnInput {
    Text(String),
    Attached {
        text: String,
        attachments: Vec<TurnAttachment>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnAttachment {
    pub path: std::path::PathBuf,
    pub media_type: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionOption {
    Mode(String),
    Model(String),
    Effort(ThinkingLevel),
}

impl SessionOption {
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::Mode(_) => "mode",
            Self::Model(_) => "model",
            Self::Effort(_) => "effort",
        }
    }

    #[must_use]
    pub fn for_turn(model: Option<String>, effort: Option<ThinkingLevel>) -> Vec<Self> {
        model
            .map(Self::Model)
            .into_iter()
            .chain(effort.map(Self::Effort))
            .collect()
    }
}

#[async_trait]
pub trait HarnessAdapter: Send + Sync {
    async fn release(&self, _session: &SessionId) -> Result<(), HarnessAdapterError> {
        Err(HarnessAdapterError::ReleaseUnsupported)
    }
    async fn new_session(&self, spec: SessionSpec) -> Result<SessionId, HarnessAdapterError>;

    async fn prompt(
        &self,
        session: &SessionId,
        input: TurnInput,
    ) -> Result<(), HarnessAdapterError>;

    async fn cancel(&self, session: &SessionId) -> Result<(), HarnessAdapterError>;

    async fn answer_permission(
        &self,
        session: &SessionId,
        request_id: &str,
        decision: ApprovalDecision,
    ) -> Result<(), HarnessAdapterError>;

    async fn set_option(
        &self,
        session: &SessionId,
        option: SessionOption,
    ) -> Result<(), HarnessAdapterError>;

    async fn events(
        &self,
        session: &SessionId,
    ) -> Result<Receiver<SessionEvent>, HarnessAdapterError>;

    async fn offerings(&self) -> Result<Vec<HarnessOffering>, HarnessAdapterError>;
}
