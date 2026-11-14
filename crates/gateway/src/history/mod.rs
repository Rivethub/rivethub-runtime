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
use wire::session_id::{SessionId, SessionKey, SessionRef, decode_segment};

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

fn received() -> ApiError {
    ApiError::Code(
        HarnessCode::BadRequest,
        "a received session has no harness transcript on this node; its stored messages are at GET /api/sessions/{session}/messages".to_owned(),
    )
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
        let key = match &decoded.session {
            SessionRef::Canonical(session) => SessionKey::Harness(session.clone()),
            SessionRef::Import(key) => SessionKey::Import(key.clone()),
            SessionRef::Bare(native) => crate::session_address::bare(gateway, native)
                .await?
                .ok_or_else(|| {
                    invalid(format!(
                        "no registered harness owns bare session id {native}"
                    ))
                })?,
        };
        let session = match key {
            SessionKey::Harness(session) => session,
            SessionKey::Import(_) => return Err(received()),
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

    pub(crate) async fn bare_harness(
        &self,
        gateway: &Gateway,
        native: &str,
    ) -> Result<Option<HarnessId>, ApiError> {
        let registry = registered();
        let known = self.bare.read().get(native).copied();
        if let Some(harness) = known.filter(|harness| registry.contains(harness)) {
            return Ok(Some(harness));
        }
        if let Some(lister) = gateway.session_lister() {
            for harness in registry {
                if lister.describe(harness, native, None).await?.is_some() {
                    self.bare.write().insert(native.to_owned(), harness);
                    return Ok(Some(harness));
                }
            }
        }
        Ok(None)
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Need {
    Read,
    Change,
}

pub(crate) async fn authorize(
    gateway: &Gateway,
    user: &UserId,
    resolved: &Resolved,
    need: Need,
) -> Result<(), ApiError> {
    let key = wire::session_id::SessionKey::from(&resolved.session);
    let seen = crate::session_access::seen_one(gateway, user, &key).await?;
    if !seen.readable() {
        return Err(ApiError::SessionForbidden);
    }
    match need {
        Need::Read => Ok(()),
        Need::Change => seen.change(),
    }
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
