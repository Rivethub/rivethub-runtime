use std::fmt;

use kernel::harness::ApprovalDecision;
use kernel::model::ProviderRecovery;
use thiserror::Error;
use wire::harness::HarnessId;

use crate::identity::IdentityError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatUnsupportedReason {
    TerminalOnly,
    CaptureOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnverifiedReason {
    NotInstalled,
}

impl fmt::Display for ChatUnsupportedReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TerminalOnly => f.write_str(
                "it is terminal-only and has no ACP endpoint; chat runs through the terminal view",
            ),
            Self::CaptureOnly => f.write_str("it is capture-only and cannot run chat turns"),
        }
    }
}

impl fmt::Display for UnverifiedReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotInstalled => f.write_str("not installed"),
        }
    }
}

#[derive(Debug, Error)]
pub enum AcpError {
    #[error("headless launcher probe failed: {0}")]
    ProbeTask(tokio::task::JoinError),
    #[error("{harness} does not support chat over ACP: {reason}")]
    ChatUnsupported {
        harness: HarnessId,
        reason: ChatUnsupportedReason,
    },
    #[error("failed to launch {program}: {source}")]
    Launch {
        program: String,
        source: std::io::Error,
    },
    #[error("{program} did not open its stdio pipes")]
    Stdio { program: String },
    #[error("ACP {method} failed: {source}")]
    Protocol {
        method: &'static str,
        source: agent_client_protocol::Error,
    },
    #[error("the agent behind {method} is gone")]
    Closed { method: &'static str },
    #[error("session {session} is already active")]
    SessionActive {
        session: wire::session_id::SessionId,
    },
    #[error("no ACP launch is configured for {harness}")]
    Unconfigured { harness: HarnessId },
    #[error("the ACP invocation of {program} for {harness} is not verified: {reason}")]
    UnverifiedLaunch {
        harness: HarnessId,
        program: String,
        reason: UnverifiedReason,
    },
    #[error("the agent does not offer a {category} config option")]
    ConfigOptionUnavailable { category: &'static str },
    #[error("the agent rejected {category} value {value}")]
    ConfigValueRejected {
        category: &'static str,
        value: String,
    },
    #[error("the permission request has no tool title")]
    MissingPermissionTitle,
    #[error("ACP media content cannot be represented as text")]
    NonTextContent,
    #[error("ACP discovery timed out after {}ms", timeout.as_millis())]
    DiscoveryTimeout { timeout: std::time::Duration },
    #[error("failed to prepare the discovery directory {}: {source}", path.display())]
    DiscoveryDir {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("provider {provider} is degraded ({recovery}): {cause}", recovery = degradation.recovery, cause = degradation.cause)]
    ProviderDegraded {
        provider: String,
        degradation: kernel::model::Degradation,
    },
    #[error("the session update has no kernel event")]
    UnmappedUpdate,
    #[error(transparent)]
    Identity(#[from] IdentityError),
    #[error("no permission request {request_id} is waiting for an answer")]
    UnknownPermission { request_id: String },
    #[error("the agent offered no option to {decision:?} this permission request")]
    UnanswerablePermission { decision: ApprovalDecision },
    #[error("the agent reported a tool input that is not a JSON object: {source}")]
    ToolInput { source: serde_json::Error },
    #[error("the agent reported turn usage under _meta that is not token counts: {source}")]
    UsageMeta { source: serde_json::Error },
    #[error("the startup context did not encode as session/new _meta: {source}")]
    StartupMeta { source: serde_json::Error },
    #[error(
        "no working directory: the request, the launch command and runtime.workspace all leave it unset"
    )]
    WorkingDir,
}

impl AcpError {
    #[must_use]
    pub const fn recovery(&self) -> ProviderRecovery {
        match self {
            Self::DiscoveryTimeout { .. }
            | Self::DiscoveryDir { .. }
            | Self::Launch { .. }
            | Self::Stdio { .. }
            | Self::Closed { .. }
            | Self::ProbeTask(_) => ProviderRecovery::Retrying,
            Self::ChatUnsupported { .. }
            | Self::Protocol { .. }
            | Self::SessionActive { .. }
            | Self::Unconfigured { .. }
            | Self::UnverifiedLaunch { .. }
            | Self::ConfigOptionUnavailable { .. }
            | Self::ConfigValueRejected { .. }
            | Self::MissingPermissionTitle
            | Self::NonTextContent
            | Self::ProviderDegraded { .. }
            | Self::UnmappedUpdate
            | Self::Identity(_)
            | Self::UnknownPermission { .. }
            | Self::UnanswerablePermission { .. }
            | Self::ToolInput { .. }
            | Self::UsageMeta { .. }
            | Self::StartupMeta { .. }
            | Self::WorkingDir => ProviderRecovery::Permanent,
        }
    }
}

pub(crate) fn port(error: impl Into<AcpError>) -> kernel::harness::HarnessAdapterError {
    let error: AcpError = error.into();
    if let AcpError::ConfigOptionUnavailable { category } = &error {
        return kernel::harness::HarnessAdapterError::OptionUnavailable {
            option: (*category).to_owned(),
        };
    }
    if let AcpError::ConfigValueRejected { category, value } = &error {
        return kernel::harness::HarnessAdapterError::OptionRejected {
            option: (*category).to_owned(),
            value: value.clone(),
        };
    }
    kernel::harness::HarnessAdapterError::Failed {
        message: error.to_string(),
        source: Box::new(error),
    }
}
