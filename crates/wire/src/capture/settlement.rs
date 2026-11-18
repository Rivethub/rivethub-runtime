use crate::time::TimestampFormat as _;
use jiff::Timestamp;

use super::CaptureBatchRequest;
use crate::JsonObject;

#[derive(Debug)]
pub struct SessionObservation {
    pub at: Timestamp,
    pub end_key: Option<String>,
}

impl CaptureBatchRequest {
    #[must_use]
    pub fn session_observation(&self) -> Option<SessionObservation> {
        SessionObservation::from_capture(&self.session_key, self.settings.as_ref(), self.finalize)
    }
}

impl SessionObservation {
    #[must_use]
    pub fn from_capture(
        session_key: &str,
        settings: Option<&JsonObject>,
        finalize: Option<bool>,
    ) -> Option<Self> {
        let settings = settings?;
        let source = live_source(session_key, settings)?;
        settings
            .get_str("last_event")
            .filter(|event| !event.is_empty())?;
        let observed = settings.get_str("last_ingest_at")?;
        let at = crate::time::parse_rfc3339(observed).ok()?;
        let end_key = (Self::is_session_end(session_key, Some(settings), finalize)
            && settings.get_bool("summary_end_suppressed") != Some(true))
        .then(|| {
            let native = settings.get_str("last_event_id").unwrap_or_default();
            format!("{source}/{}:{native}", at.rfc3339_offset())
        });
        Some(Self { at, end_key })
    }

    #[must_use]
    pub fn is_session_end(
        session_key: &str,
        settings: Option<&JsonObject>,
        finalize: Option<bool>,
    ) -> bool {
        let Some(settings) = settings else {
            return false;
        };
        let direct_session_hook = match live_source(session_key, settings) {
            Some("claude-code-hook") => settings.get("parent_session_id").is_none(),
            Some("codex-hook" | "grok-build-hook") => true,
            _ => false,
        };
        finalize == Some(true)
            && direct_session_hook
            && settings.get_str("last_event") == Some("SessionEnd")
    }
}

fn live_source<'a>(session_key: &str, settings: &'a JsonObject) -> Option<&'a str> {
    let source = settings.get_str("source")?;
    let prefix = match source {
        "claude-code-hook" => "claude-code:",
        "codex-hook" => "codex:",
        "grok-build-hook" => "grok-build:",
        "dsh-hook" => "dsh:",
        "opencode-hook" => "opencode:",
        _ => return None,
    };
    session_key.starts_with(prefix).then_some(source)
}
