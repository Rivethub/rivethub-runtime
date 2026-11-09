use crate::JsonObject;
use crate::message::Role;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use sha2::{Digest as _, Sha256};

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CaptureBatchRequest {
    pub session_key: String,
    pub agent: String,
    pub channel: Option<String>,
    pub title: Option<String>,
    pub settings: Option<JsonObject>,
    pub task_id: Option<String>,
    pub finalize: Option<bool>,
    #[serde(default)]
    #[serde_as(as = "Option<crate::time::Millis>")]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    #[serde_as(as = "Option<crate::time::Millis>")]
    pub updated_at: Option<DateTime<Utc>>,
    pub messages: Vec<CaptureMessage>,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CaptureMessage {
    pub event_id: String,
    pub role: Role,
    pub content: String,
    pub tool_name: Option<String>,
    pub tool_args: Option<JsonObject>,
    pub tool_result: Option<String>,
    pub metadata: Option<JsonObject>,
    #[serde(default)]
    #[serde_as(as = "Option<crate::time::Millis>")]
    pub created_at: Option<DateTime<Utc>>,
}

impl CaptureBatchRequest {
    #[must_use]
    pub fn entries(&self) -> Vec<crate::memory::MemoryEntry> {
        self.messages
            .iter()
            .map(|message| crate::memory::MemoryEntry {
                id: Some(message.event_id.clone()),
                session_id: self.session_key.clone(),
                agent: self.agent.clone(),
                channel: self.channel.clone().unwrap_or_default(),
                role: message.role,
                content: message.content.clone(),
                tool_name: message.tool_name.clone(),
                tool_args: message.tool_args.clone(),
                tool_result: message.tool_result.clone(),
                metadata: message.metadata.clone(),
                created_at: message.created_at,
            })
            .collect()
    }
}

pub const SESSION_BUNDLE_VERSION: u32 = 2;

pub const UNKNOWN_ORIGIN: &str = "unknown";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionBundle {
    pub version: u32,
    pub manifest: BundleManifest,
    pub sessions: Vec<BundledSession>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BundleManifest {
    pub exported_at: DateTime<Utc>,
    pub exported_by: BundleAuthor,
    pub sanitization: Sanitization,
    pub session_count: usize,
    pub bundle_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BundleAuthor {
    pub user: String,
    pub node: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Sanitization {
    Raw,
    Sanitized(SanitizeStamp),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SanitizeStamp {
    pub at: DateTime<Utc>,
    pub policy_sha256: String,
    pub passes: Vec<String>,
    pub unresolved_flags: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BundledSession {
    pub origin: SessionOrigin,
    pub batch: CaptureBatchRequest,
}

pub const IMPORTED_SESSION_PREFIX: &str = "import:";

#[must_use]
pub fn is_imported_session(session_key: &str) -> bool {
    session_key.starts_with(IMPORTED_SESSION_PREFIX)
}

impl BundledSession {
    pub fn receiver_session_key(&self) -> Result<String, serde_json::Error> {
        let origin = &self.origin;
        if origin.node == UNKNOWN_ORIGIN || origin.conversation_id == UNKNOWN_ORIGIN {
            let first = self
                .batch
                .messages
                .first()
                .map_or("", |message| message.event_id.as_str());
            let identity = serde_json::to_string(&(&origin.session_key, &origin.agent, first))?;
            let digest = hex::encode(Sha256::digest(identity));
            Ok(format!(
                "{IMPORTED_SESSION_PREFIX}{UNKNOWN_ORIGIN}:{}",
                digest.get(..32).unwrap_or(&digest)
            ))
        } else {
            Ok(format!(
                "{IMPORTED_SESSION_PREFIX}{}:{}",
                origin.node, origin.conversation_id
            ))
        }
    }

    pub fn import_batch(&self) -> Result<CaptureBatchRequest, serde_json::Error> {
        let mut batch = self.batch.clone();
        batch.session_key = self.receiver_session_key()?;
        batch
            .settings
            .get_or_insert_default()
            .insert("origin", &self.origin)?;
        batch.created_at = self.origin.created_at.or(batch.created_at);
        batch.updated_at = self.origin.updated_at.or(batch.updated_at);
        Ok(batch)
    }
}

pub fn merge_imported_settings(
    stored: Option<&JsonObject>,
    incoming: Option<&JsonObject>,
) -> Result<JsonObject, serde_json::Error> {
    let empty = JsonObject::default();
    let incoming = incoming.unwrap_or(&empty);
    let Some(stored) = stored else {
        return Ok(incoming.clone());
    };
    let updated =
        |settings: &JsonObject| SessionOrigin::of(settings).and_then(|origin| origin.updated_at);
    let (mut merged, winner) = if updated(incoming) >= updated(stored) {
        (stored.clone(), incoming)
    } else {
        (incoming.clone(), stored)
    };
    for (key, value) in winner.as_map() {
        merged.insert(key, value)?;
    }
    Ok(merged)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SessionOrigin {
    pub author: String,
    pub node: String,
    pub conversation_id: String,
    pub session_key: String,
    pub agent: String,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl SessionOrigin {
    #[must_use]
    pub fn of(settings: &JsonObject) -> Option<Self> {
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                fallback,
                reason = "CONTRACT B3 (note 126): a stored origin that does not parse counts as the earliest when an import merges settings"
            )
        )]
        let origin = serde_json::from_value(settings.get("origin")?.clone()).ok();
        origin
    }

    #[must_use]
    pub fn known_author(&self) -> Option<&str> {
        (self.author != UNKNOWN_ORIGIN).then_some(self.author.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct CaptureResult {
    pub ok: bool,
    pub conversation_id: String,
    pub inserted: u64,
    pub skipped: u64,
}
