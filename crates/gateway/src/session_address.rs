use wire::session_id::{SessionId, SessionKey, SessionRef, decode_segment};

use crate::Gateway;
use crate::error::ApiError;

const NOT_FOUND: &str = "not found";
pub(crate) const RECEIVED_UNFILED: &str = "a received session cannot be filed under a topic or marked as history; only sessions captured here can";

pub(crate) async fn resolve(gateway: &Gateway, segment: &str) -> Result<SessionKey, ApiError> {
    let decoded =
        decode_segment(segment).map_err(|error| ApiError::BadRequest(error.to_string()))?;
    match decoded.session {
        SessionRef::Canonical(session) => Ok(SessionKey::Harness(session)),
        SessionRef::Import(key) => Ok(SessionKey::Import(key)),
        SessionRef::Bare(native) => bare(gateway, &native)
            .await?
            .ok_or(ApiError::NotFound(NOT_FOUND)),
    }
}

pub(crate) async fn bare(gateway: &Gateway, native: &str) -> Result<Option<SessionKey>, ApiError> {
    if let Ok(conversation) = uuid::Uuid::parse_str(native)
        && let Some(catalog) = gateway.session_catalog()
        && let Some(key) = catalog.conversation_session(conversation).await?
    {
        return Ok(Some(key));
    }
    Ok(gateway
        .history()
        .bare_harness(gateway, native)
        .await?
        .map(|harness| SessionKey::Harness(SessionId::new(harness, native))))
}

pub(crate) async fn resolve_captured(
    gateway: &Gateway,
    segment: &str,
) -> Result<SessionId, ApiError> {
    match resolve(gateway, segment).await? {
        SessionKey::Harness(session) => Ok(session),
        SessionKey::Import(_) => Err(ApiError::Unprocessable(RECEIVED_UNFILED.to_owned())),
    }
}
