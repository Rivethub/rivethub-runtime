use jiff::civil::{Date, Time};
use thiserror::Error;
use uuid::Uuid;
use wire::harness::HarnessId;
use wire::session_id::SessionId;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NativeSession {
    harness: HarnessId,
    native: String,
}

#[derive(Debug, Clone, Error)]
pub enum IdentityError {
    #[error("{harness} native session id {native:?} does not match the recorded shape")]
    Shape { harness: HarnessId, native: String },
    #[error("{harness} native session id {native:?} is not a version-7 uuid")]
    UuidVersion { harness: HarnessId, native: String },
    #[error("invalid Hermes session timestamp in {native:?}: {source}")]
    Timestamp { native: String, source: jiff::Error },
}

impl NativeSession {
    pub fn parse(harness: HarnessId, native: &str) -> Result<Self, IdentityError> {
        matches_shape(harness, native)?;
        Ok(Self {
            harness,
            native: native.to_owned(),
        })
    }

    #[must_use]
    pub fn canonical(&self) -> SessionId {
        SessionId::new(self.harness, &self.native)
    }
}

fn matches_shape(harness: HarnessId, native: &str) -> Result<(), IdentityError> {
    let shaped = match harness {
        HarnessId::ClaudeCode
        | HarnessId::Omp
        | HarnessId::Dsh
        | HarnessId::Cursor
        | HarnessId::Cowork
        | HarnessId::Codex
        | HarnessId::GrokBuild => is_uuid(native),
        HarnessId::Hermes => is_uuid(native) || is_hermes(native)?,
        HarnessId::KimiCode => is_kimi(native),
        HarnessId::Opencode => is_opencode(native),
    };
    if !shaped {
        return Err(IdentityError::Shape {
            harness,
            native: native.to_owned(),
        });
    }
    if harness == HarnessId::GrokBuild
        && !Uuid::parse_str(native).is_ok_and(|uuid| uuid.get_version_num() == 7)
    {
        return Err(IdentityError::UuidVersion {
            harness,
            native: native.to_owned(),
        });
    }
    Ok(())
}

fn is_uuid(native: &str) -> bool {
    Uuid::parse_str(native).is_ok_and(|uuid| uuid.hyphenated().to_string() == native)
}

fn is_hermes(native: &str) -> Result<bool, IdentityError> {
    let mut fields = native.split('_');
    let (Some(day), Some(clock), Some(tag), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return Ok(false);
    };
    let date = Date::strptime("%Y%m%d", day).map_err(|source| IdentityError::Timestamp {
        native: native.to_owned(),
        source,
    })?;
    let time = Time::strptime("%H%M%S", clock).map_err(|source| IdentityError::Timestamp {
        native: native.to_owned(),
        source,
    })?;
    Ok(date.year() > 0
        && time.hour() < 24
        && time.minute() < 60
        && time.second() < 60
        && is_hermes_tag(tag))
}

fn is_hermes_tag(tag: &str) -> bool {
    tag.len() == 6 && tag.chars().all(|c| c.is_ascii_hexdigit())
}

fn is_kimi(native: &str) -> bool {
    native.strip_prefix("session_").is_some_and(is_uuid)
}

fn is_opencode(native: &str) -> bool {
    native
        .strip_prefix("ses_")
        .is_some_and(|rest| rest.len() >= 20 && rest.chars().all(|c| c.is_ascii_alphanumeric()))
}
