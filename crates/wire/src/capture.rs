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

pub const REPORT_SUFFIX: &str = ".report.md";

pub const LOCAL_SUFFIX: &str = ".review.local.json";

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review: Option<ReviewStamp>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReviewStamp {
    pub mode: ReviewMode,
    pub model: String,
    pub calls: usize,
    pub flags: Vec<FlagStamp>,
    pub seal: String,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, strum::Display,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum ReviewMode {
    Advisory,
    Block,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FlagStamp {
    pub number: usize,
    pub mark: FlagMark,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, strum::Display,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum FlagMark {
    Open,
    Keep,
    Redact,
    Map,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BundledSession {
    pub origin: SessionOrigin,
    pub batch: CaptureBatchRequest,
}

pub const RECEIPT_KEY: &str = "receipt";
pub const UNSIGNED: &str = "unsigned";
const SIGNER_PREFIX: &str = "ed25519:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptVerification {
    Verified,
    Unsigned,
    UnknownKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Receipt {
    pub bundle_id: String,
    pub received_at: DateTime<Utc>,
    pub verification: ReceiptVerification,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_fingerprint: Option<String>,
}

impl Receipt {
    #[must_use]
    pub fn signer(&self) -> &str {
        self.key_fingerprint.as_deref().unwrap_or(UNSIGNED)
    }
}

fn receipt_signer(settings: &JsonObject) -> Result<Option<String>, serde_json::Error> {
    settings
        .get(RECEIPT_KEY)
        .map(|value| {
            serde_json::from_value::<Receipt>(value.clone())
                .map(|receipt| receipt.signer().to_owned())
        })
        .transpose()
}

#[derive(Debug, thiserror::Error)]
pub enum MergeError {
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(
        "this import would join a conversation received from {stored}, but it comes from {incoming}; content from different signers never shares a conversation"
    )]
    ReceiptConflict { stored: String, incoming: String },
}

pub use values::IMPORTED_SESSION_PREFIX;

#[must_use]
pub fn is_imported_session(session_key: &str) -> bool {
    session_key.starts_with(IMPORTED_SESSION_PREFIX)
}

#[derive(Debug, thiserror::Error)]
#[error(
    "an import session key is import: followed by a JSON array naming its origin, [node, conversation_id] or [\"unknown\", session_key, agent, event_id], not {session_key:?}"
)]
pub struct ImportKeyError {
    pub session_key: String,
}

pub fn owned_import_key(owner: &str, session_key: &str) -> Result<Option<String>, ImportKeyError> {
    let Some(identity) = session_key.strip_prefix(IMPORTED_SESSION_PREFIX) else {
        return Ok(None);
    };
    let malformed = || ImportKeyError {
        session_key: session_key.to_owned(),
    };
    let identity: Vec<String> = serde_json::from_str(identity).map_err(|_| malformed())?;
    let names_an_origin = match identity.as_slice() {
        [node, conversation_id] => node != UNKNOWN_ORIGIN && conversation_id != UNKNOWN_ORIGIN,
        [node, conversation_id, signer] => {
            node != UNKNOWN_ORIGIN && conversation_id != UNKNOWN_ORIGIN && is_signer(signer)
        }
        [unknown, _session_key, _agent, _event_id] => unknown == UNKNOWN_ORIGIN,
        [unknown, _session_key, _agent, _event_id, signer] => {
            unknown == UNKNOWN_ORIGIN && is_signer(signer)
        }
        _ => false,
    };
    if !names_an_origin {
        return Err(malformed());
    }
    let preimage: Vec<&str> = std::iter::once(owner)
        .chain(identity.iter().map(String::as_str))
        .collect();
    let canonical = serde_json::to_string(&preimage).map_err(|_| malformed())?;
    Ok(Some(format!(
        "{IMPORTED_SESSION_PREFIX}{}",
        hex::encode(Sha256::digest(canonical))
    )))
}

fn is_signer(text: &str) -> bool {
    text == UNSIGNED || text.starts_with(SIGNER_PREFIX)
}

impl BundledSession {
    fn import_identity(&self) -> Vec<&str> {
        let origin = &self.origin;
        if origin.node == UNKNOWN_ORIGIN || origin.conversation_id == UNKNOWN_ORIGIN {
            let first = self
                .batch
                .messages
                .first()
                .map_or("", |message| message.event_id.as_str());
            vec![UNKNOWN_ORIGIN, &origin.session_key, &origin.agent, first]
        } else {
            let conversation = origin
                .identity
                .as_deref()
                .unwrap_or(&origin.conversation_id);
            vec![&origin.node, conversation]
        }
    }

    pub fn import_batch(&self) -> Result<CaptureBatchRequest, serde_json::Error> {
        self.batch_with_identity(&self.import_identity())
    }

    pub fn received_batch(
        &self,
        receipt: &Receipt,
    ) -> Result<CaptureBatchRequest, serde_json::Error> {
        let mut identity = self.import_identity();
        identity.push(receipt.signer());
        let mut batch = self.batch_with_identity(&identity)?;
        batch
            .settings
            .get_or_insert_default()
            .insert(RECEIPT_KEY, receipt)?;
        Ok(batch)
    }

    fn batch_with_identity(
        &self,
        identity: &[&str],
    ) -> Result<CaptureBatchRequest, serde_json::Error> {
        let mut batch = self.batch.clone();
        batch.session_key = format!(
            "{IMPORTED_SESSION_PREFIX}{}",
            serde_json::to_string(&identity)?
        );
        let settings = batch.settings.get_or_insert_default();
        settings.retain(|key| key != RECEIPT_KEY);
        settings.insert("origin", &self.origin)?;
        batch.created_at = self.origin.created_at.or(batch.created_at);
        batch.updated_at = self
            .origin
            .updated_at
            .or(batch.updated_at)
            .or_else(|| batch.messages.iter().filter_map(|m| m.created_at).max());
        batch.finalize = Some(true);
        Ok(batch)
    }

    #[must_use]
    pub fn forwarding_stored_origin(mut self) -> Self {
        if !is_imported_session(&self.batch.session_key) {
            return self;
        }
        let Some(origin) = self.batch.settings.as_ref().and_then(SessionOrigin::of) else {
            return self;
        };
        self.batch.session_key.clone_from(&origin.session_key);
        self.batch.created_at = origin.created_at;
        self.batch.updated_at = origin.updated_at;
        self.origin = origin;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedSettings {
    pub settings: JsonObject,
    pub incoming_is_newer: bool,
}

pub fn merge_imported_settings(
    stored: Option<&JsonObject>,
    incoming: Option<&JsonObject>,
) -> Result<ImportedSettings, MergeError> {
    let empty = JsonObject::default();
    let incoming = incoming.unwrap_or(&empty);
    let Some(stored) = stored else {
        return Ok(ImportedSettings {
            settings: incoming.clone(),
            incoming_is_newer: true,
        });
    };
    let (was, now) = (receipt_signer(stored)?, receipt_signer(incoming)?);
    if was != now {
        let name = |side: Option<String>| side.unwrap_or_else(|| "a plain import".to_owned());
        return Err(MergeError::ReceiptConflict {
            stored: name(was),
            incoming: name(now),
        });
    }
    let updated =
        |settings: &JsonObject| SessionOrigin::of(settings).and_then(|origin| origin.updated_at);
    let incoming_is_newer = updated(incoming) >= updated(stored);
    let (mut settings, winner) = if incoming_is_newer {
        (stored.clone(), incoming)
    } else {
        (incoming.clone(), stored)
    };
    for (key, value) in winner.as_map() {
        settings.insert(key, value)?;
    }
    Ok(ImportedSettings {
        settings,
        incoming_is_newer,
    })
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
}

impl SessionOrigin {
    #[must_use]
    pub fn of(settings: &JsonObject) -> Option<Self> {
        #[cfg_attr(
            dylint_lib = "rr_lints",
            expect(
                fallback,
                reason = "CONTRACT B3 (note 135): a stored origin that does not parse counts as the earliest when an import merges settings"
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
