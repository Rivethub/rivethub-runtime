pub(crate) mod body;
mod context;
mod offering;
mod preview;
mod rows;

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::Utc;
use kernel::harness::HarnessCapabilities;
use kernel::transcript::HarnessToolOutput;
use kernel::transcript::{
    StoreSession, StoreTranscript, ToolOutputRead, TranscriptRead, TranscriptWindow,
};
use parking_lot::RwLock;
use wire::harness::{HarnessCode, HarnessId, HarnessOffering, HarnessSessionSummary};
use wire::session_id::{SessionId, SessionRef, decode_segment};

pub(crate) use context::ContextStamp;
use context::Stamps;
pub(crate) use rows::legacy_row;

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

pub(crate) async fn offerings(gateway: &Gateway) -> Result<Vec<HarnessOffering>, ApiError> {
    crate::routes::terminal::service(gateway)?
        .adapter()
        .ok_or_else(|| ApiError::Unavailable("chat harness unavailable".to_owned()))?;
    offered(gateway).await
}

pub(crate) async fn offered(gateway: &Gateway) -> Result<Vec<HarnessOffering>, ApiError> {
    let Some(adapter) = gateway.terminal().and_then(|terminal| terminal.adapter()) else {
        return Ok(Vec::new());
    };
    let mut harnesses: Vec<HarnessOffering> = adapter
        .offerings()
        .await?
        .into_iter()
        .map(offering::harness_offering_body)
        .collect();
    let images = gateway.uploads().is_some();
    let listed = gateway.session_lister().is_some();
    for offering in &mut harnesses {
        offering.capabilities.image_attachments = Some(images);
        if listed {
            offering.capabilities.observation.list_sessions = true;
        }
    }
    Ok(harnesses)
}

pub(crate) async fn capabilities(
    gateway: &Gateway,
    harness: HarnessId,
) -> Option<HarnessCapabilities> {
    let adapter = gateway.terminal()?.adapter()?;
    match adapter.offerings().await {
        Ok(offerings) => offerings
            .into_iter()
            .find(|offering| offering.harness_id == harness)
            .map(|offering| offering.capabilities),
        Err(error) => {
            tracing::debug!(%error, "harness offerings are unreadable; recorded models stand");
            None
        }
    }
}

pub(crate) fn name_model(
    capabilities: Option<&HarnessCapabilities>,
    summary: &mut HarnessSessionSummary,
) {
    if let (Some(capabilities), Some(model)) = (capabilities, summary.model.as_mut()) {
        let named = capabilities.offered_model(model);
        if named != model.as_str() {
            *model = named.to_owned();
        }
    }
}

async fn registered(gateway: &Gateway) -> Result<Vec<HarnessId>, ApiError> {
    Ok(offered(gateway)
        .await?
        .into_iter()
        .map(|offering| offering.harness_id)
        .collect())
}

impl History {
    pub(crate) async fn resolve(
        &self,
        gateway: &Gateway,
        segment: &str,
    ) -> Result<Resolved, ApiError> {
        let decoded = decode_segment(segment).map_err(|error| invalid(error.to_string()))?;
        let registry = registered(gateway).await?;
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
        let mut read = match lister
            .transcript(resolved.harness, &resolved.native, window)
            .await?
        {
            TranscriptRead::Read(read) => read,
            TranscriptRead::NoSource => return Err(no_source(resolved.harness)),
        };
        if gateway.uploads().is_some() {
            let journal = crate::routes::harnesses::open_store(gateway)
                .await?
                .attached_turns(&resolved.session)
                .await
                .map_err(crate::routes::harnesses::upload_error)?;
            crate::uploads::attach(&mut read.turns, &journal);
        }
        let spawn_model = live_summary(gateway, &resolved.native).and_then(|live| live.model);
        let stamp = self.stamps.stamp(
            &resolved.session,
            resolved.harness,
            &read.turns,
            spawn_model.as_deref(),
        );
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

pub(crate) fn live_summary(gateway: &Gateway, key: &str) -> Option<HarnessSessionSummary> {
    gateway.terminal()?.live_summary(key)
}

pub(crate) async fn store_summary(
    gateway: &Gateway,
    row: &StoreSession,
    live: Option<&HarnessSessionSummary>,
) -> HarnessSessionSummary {
    let cwd = match live
        .and_then(|live| live.cwd.clone())
        .or_else(|| row.cwd.clone())
    {
        Some(cwd) => Some(cwd),
        None => recorded_cwd(gateway, row).await,
    };
    let mut summary = rows::summary(row, cwd, Utc::now());
    if let Some(live) = live {
        summary.status = live.status;
        summary.blocked = live.blocked;
        summary.transport = live.transport;
        summary.effort.clone_from(&live.effort);
        if live.model.is_some() {
            summary.model.clone_from(&live.model);
        }
    }
    summary
}

pub(crate) async fn running_elsewhere(
    gateway: &Gateway,
    session: &SessionId,
) -> Result<Option<u32>, ApiError> {
    if gateway
        .terminal()
        .is_some_and(|terminal| terminal.holds(session.as_str()))
    {
        return Ok(None);
    }
    match gateway.session_lister() {
        Some(lister) => Ok(lister
            .running_elsewhere(session.harness(), session.native())
            .await?),
        None => Ok(None),
    }
}

pub(crate) async fn refuse_elsewhere(
    gateway: &Gateway,
    session: &SessionId,
) -> Result<(), ApiError> {
    match running_elsewhere(gateway, session).await? {
        Some(pid) => Err(ApiError::Code(
            HarnessCode::SessionRunningElsewhere,
            format!(
                "This session is running in another terminal (process {pid}), so it can't be resumed here until that one ends"
            ),
        )),
        None => Ok(()),
    }
}

pub(crate) async fn working_dir(gateway: &Gateway, session: &SessionId) -> Option<PathBuf> {
    let live = live_summary(gateway, session.native()).and_then(|live| live.cwd);
    let recorded = if let Some(cwd) = live {
        cwd
    } else {
        stored_cwd(gateway, session).await?
    };
    let directory = PathBuf::from(recorded);
    tokio::fs::metadata(&directory)
        .await
        .is_ok_and(|found| found.is_dir())
        .then_some(directory)
}

async fn stored_cwd(gateway: &Gateway, session: &SessionId) -> Option<String> {
    let read = gateway
        .session_lister()?
        .store_cwd(session.harness(), session.native())
        .await;
    #[cfg_attr(
        dylint_lib = "rr_lints",
        expect(
            fallback,
            reason = "CONTRACT B1 (note 112): an unreadable store record leaves the terminal in the roster command's folder"
        )
    )]
    let recorded = read.unwrap_or_else(|error| {
        tracing::debug!(%error, session = %session, "the session's folder is unreadable");
        None
    });
    recorded
}

async fn recorded_cwd(gateway: &Gateway, row: &StoreSession) -> Option<String> {
    if row.harness == HarnessId::ClaudeCode
        && let Some(lister) = gateway.session_lister()
    {
        match lister.store_cwd(row.harness, &row.id).await {
            Ok(Some(cwd)) => return Some(cwd),
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(%error, session = row.id, "recorded cwd is unreadable; the roster cwd applies");
            }
        }
    }
    gateway
        .terminal()
        .and_then(|terminal| terminal.roster_cwd(row.harness))
        .map(|cwd| cwd.to_string_lossy().into_owned())
}
