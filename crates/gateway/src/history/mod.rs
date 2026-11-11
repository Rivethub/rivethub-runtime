pub(crate) mod body;
mod context;
mod preview;
mod rows;

use std::collections::HashMap;

use chrono::Utc;
use kernel::transcript::HarnessToolOutput;
use kernel::transcript::{
    StoreSession, StoreTranscript, ToolOutputRead, TranscriptRead, TranscriptWindow,
};
use parking_lot::RwLock;
use strum::IntoEnumIterator as _;
use wire::harness::{HarnessCode, HarnessId, HarnessSessionSummary};
use wire::session_id::{SessionId, SessionRef, decode_segment};

pub(crate) use context::ContextStamp;
use context::Stamps;

use crate::Gateway;
use crate::error::ApiError;
use crate::tenancy::UserId;

#[derive(Debug, Default)]
pub(crate) struct History {
    bare: RwLock<HashMap<String, HarnessId>>,
    stamps: Stamps,
    previews: preview::Previews,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    pub(crate) session: SessionId,
    pub(crate) harness: HarnessId,
    pub(crate) native: String,
    pub(crate) redirected_to: Option<SessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Transcript {
    pub(crate) read: StoreTranscript,
    pub(crate) stamp: ContextStamp,
}

fn invalid(message: String) -> ApiError {
    ApiError::Code(HarnessCode::InvalidSessionId, message)
}

pub(crate) fn no_source(harness: HarnessId) -> ApiError {
    ApiError::Code(
        HarnessCode::CapabilityUnsupported,
        format!("{harness} has no transcript source"),
    )
}

pub(crate) fn registered() -> Vec<HarnessId> {
    HarnessId::iter().collect()
}

impl History {
    pub(crate) async fn resolve(
        &self,
        gateway: &Gateway,
        segment: &str,
    ) -> Result<Resolved, ApiError> {
        let decoded = decode_segment(segment).map_err(|error| invalid(error.to_string()))?;
        let registry = registered();
        let session = match &decoded.session {
            SessionRef::Canonical(session) => {
                let harness = session.harness();
                if !registry.contains(&harness) {
                    return Err(invalid(format!("no driver registered for {harness}")));
                }
                session.clone()
            }
            SessionRef::Bare(native) => SessionId::new(
                self.owner_of_bare(gateway, &registry, native).await?,
                native,
            ),
        };
        Ok(Resolved {
            redirected_to: decoded.redirected_to(&session),
            harness: session.harness(),
            native: session.native().to_owned(),
            session,
        })
    }

    pub(crate) async fn resolve_canonical(
        &self,
        gateway: &Gateway,
        segment: &str,
    ) -> Result<Resolved, ApiError> {
        let decoded = decode_segment(segment).map_err(|error| invalid(error.to_string()))?;
        if let SessionRef::Bare(native) = &decoded.session {
            return Err(invalid(format!("{native} is not a canonical session id")));
        }
        self.resolve(gateway, segment).await
    }

    async fn owner_of_bare(
        &self,
        gateway: &Gateway,
        registry: &[HarnessId],
        native: &str,
    ) -> Result<HarnessId, ApiError> {
        let known = self.bare.read().get(native).copied();
        if let Some(harness) = known.filter(|harness| registry.contains(harness)) {
            return Ok(harness);
        }
        if let Some(lister) = gateway.session_lister() {
            for harness in registry {
                if lister.describe(*harness, native, None).await?.is_some() {
                    self.bare.write().insert(native.to_owned(), *harness);
                    return Ok(*harness);
                }
            }
        }
        Err(invalid(format!(
            "no registered harness owns bare session id {native}"
        )))
    }

    pub(crate) async fn transcript(
        &self,
        gateway: &Gateway,
        resolved: &Resolved,
        window: TranscriptWindow,
    ) -> Result<Transcript, ApiError> {
        let lister = gateway
            .session_lister()
            .ok_or_else(|| no_source(resolved.harness))?;
        let read = match lister
            .transcript(resolved.harness, &resolved.native, window)
            .await?
        {
            TranscriptRead::Read(read) => read,
            TranscriptRead::NoSource => return Err(no_source(resolved.harness)),
        };
        let stamp = self
            .stamps
            .stamp(&resolved.session, resolved.harness, &read.turns, None);
        Ok(Transcript { read, stamp })
    }

    pub(crate) async fn tool_output(
        &self,
        gateway: &Gateway,
        resolved: &Resolved,
        tool_call_id: &str,
    ) -> Result<HarnessToolOutput, ApiError> {
        let unsupported = || {
            ApiError::Code(
                HarnessCode::CapabilityUnsupported,
                format!("{} has no tool output source", resolved.harness),
            )
        };
        let lister = gateway.session_lister().ok_or_else(unsupported)?;
        match lister
            .tool_output(resolved.harness, &resolved.native, tool_call_id)
            .await?
        {
            ToolOutputRead::Found(output) => Ok(output),
            ToolOutputRead::Missing => Err(ApiError::Missing(format!(
                "unknown tool call {tool_call_id} in {}",
                resolved.session
            ))),
            ToolOutputRead::NoSource => Err(unsupported()),
        }
    }
}

fn nested_store(harness: HarnessId, native: &str) -> bool {
    match harness {
        HarnessId::ClaudeCode => !(native.len() == 36 && uuid::Uuid::try_parse(native).is_ok()),
        HarnessId::GrokBuild => true,
        HarnessId::KimiCode
        | HarnessId::Hermes
        | HarnessId::Codex
        | HarnessId::Opencode
        | HarnessId::Omp
        | HarnessId::Dsh
        | HarnessId::Cursor
        | HarnessId::Cowork => false,
    }
}

pub(crate) async fn ensure_nested_owner(
    gateway: &Gateway,
    resolved: &Resolved,
) -> Result<(), ApiError> {
    let registry = gateway.sessions();
    if registry.owner(&resolved.native)?.is_some()
        || !nested_store(resolved.harness, &resolved.native)
    {
        return Ok(());
    }
    let Some(lister) = gateway.session_lister() else {
        return Ok(());
    };
    if let Some(row) = lister
        .describe(resolved.harness, &resolved.native, None)
        .await?
        && let Some(parent) = row.parent_session_id.as_deref()
    {
        registry.inherit(&resolved.native, parent)?;
    }
    Ok(())
}

pub(crate) async fn authorize(
    gateway: &Gateway,
    user: &UserId,
    resolved: &Resolved,
) -> Result<(), ApiError> {
    ensure_nested_owner(gateway, resolved).await?;
    gateway
        .sessions()
        .authorize_visible(user, &resolved.native)?;
    Ok(())
}

pub(crate) async fn store_summary(gateway: &Gateway, row: &StoreSession) -> HarnessSessionSummary {
    let cwd = match row.cwd.clone() {
        Some(cwd) => Some(cwd),
        None => recorded_cwd(gateway, row).await,
    };
    rows::summary(row, cwd, Utc::now())
}

async fn recorded_cwd(gateway: &Gateway, row: &StoreSession) -> Option<String> {
    if row.harness == HarnessId::ClaudeCode
        && let Some(lister) = gateway.session_lister()
    {
        match lister.store_cwd(row.harness, &row.id).await {
            Ok(Some(cwd)) => return Some(cwd),
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(%error, session = row.id, "recorded cwd is unreadable");
            }
        }
    }
    None
}
