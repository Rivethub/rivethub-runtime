use jiff::Timestamp;
use std::fmt;

use serde::Deserialize;
use serde::Serialize;
use uuid::Uuid;

#[serde_with::skip_serializing_none]
#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FindRequest {
    pub query: String,
    #[serde(default)]
    #[serde_as(as = "Option<crate::time::Rfc3339>")]
    pub since: Option<Timestamp>,
    #[serde(default)]
    #[serde_as(as = "Option<crate::time::Rfc3339>")]
    pub before: Option<Timestamp>,
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FindReply {
    pub sessions: Vec<FoundSession>,
    pub dropped: usize,
}

#[serde_with::skip_serializing_none]
#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FoundSession {
    pub id: Uuid,
    #[serde(default)]
    pub title: Option<String>,
    pub agent: String,

    #[serde_as(as = "crate::time::Rfc3339")]
    pub first: Timestamp,
    #[serde_as(as = "crate::time::Rfc3339")]
    pub last: Timestamp,
    pub messages: u64,
    pub best: f64,
    pub hits: usize,
    pub excerpts: Vec<String>,
    #[serde(default)]
    pub imported_from: Option<String>,
    #[serde(default)]
    pub session: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftRequest {
    pub sessions: Vec<ChosenSession>,
    #[serde(default)]
    pub decide: Option<bool>,
    #[serde(default)]
    pub block: Option<bool>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ChosenSession {
    pub id: Uuid,
    pub decision: SessionDecision,
    #[serde(default)]
    pub redact: Option<RedactRules>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionDecision {
    Approve,
    Redact,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RedactRules {
    #[serde(default)]
    pub terms: Option<Vec<String>>,
    #[serde(default)]
    pub drop_tool_outputs: Option<bool>,
}

#[serde_with::skip_serializing_none]
#[serde_with::serde_as]
#[derive(Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DraftReport {
    pub draft: String,

    #[serde_as(as = "crate::time::Rfc3339")]
    pub expires_at: Timestamp,
    pub totals: Totals,
    pub rules: Vec<RuleCount>,
    pub sessions: Vec<DraftSession>,
    pub not_scanned: String,
    pub best_effort: String,
    #[serde(default)]
    pub decision: Option<Decision>,
    pub flags: Vec<Flag>,
}

#[cfg_attr(dylint_lib = "rr_lints", expect(manualderive))]
impl fmt::Debug for DraftReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DraftReport")
            .field("draft", &self.draft)
            .field("sessions", &self.sessions.len())
            .field("flags", &self.flags.len())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Totals {
    pub secrets: usize,
    pub mapped: usize,
    pub redacted: usize,
    pub escaped: usize,
    pub rechecked: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RuleCount {
    pub rule: String,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct DraftSession {
    pub id: Uuid,
    pub agent: String,
    pub messages: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Decision {
    pub model: String,
    pub scored: usize,
    pub calls: usize,
    pub trivial: usize,
    pub mode: DecideMode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DecideMode {
    Advisory,
    Block,
}

#[serde_with::skip_serializing_none]
#[derive(Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Flag {
    pub number: usize,
    pub score: f64,
    #[serde(default)]
    pub session: Option<Uuid>,
    #[serde(default)]
    pub place: Option<FlagPlace>,
    pub length: usize,
    pub text: String,
    pub state: FlagState,
}

#[cfg_attr(dylint_lib = "rr_lints", expect(manualderive))]
impl fmt::Debug for Flag {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Flag")
            .field("number", &self.number)
            .field("length", &self.length)
            .finish_non_exhaustive()
    }
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FlagPlace {
    pub session: usize,
    #[serde(default)]
    pub message: Option<usize>,
    pub field: String,
    pub more: usize,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FlagState {
    Open,
    Keep,
    Redact,
    Map { term: String, placeholder: String },
}

#[cfg_attr(dylint_lib = "rr_lints", expect(manualderive))]
impl fmt::Debug for FlagState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Open => "Open",
            Self::Keep => "Keep",
            Self::Redact => "Redact",
            Self::Map { .. } => "Map",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BundlesReply {
    pub bundles: Vec<ReadyBundle>,
}

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReadyBundle {
    pub name: String,
    pub sessions: u32,
    pub bytes: u64,
    pub open_flags: u32,

    #[serde_as(as = "crate::time::Rfc3339")]
    pub created_at: Timestamp,
    pub sanitized: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BundleDraftRequest {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MarksRequest {
    pub marks: Vec<Mark>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Mark {
    pub number: usize,
    pub choice: MarkChoice,
    #[serde(default)]
    pub learn: Option<bool>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MarkChoice {
    Keep,
    Redact,
    Map { term: String, placeholder: String },
}

#[cfg_attr(dylint_lib = "rr_lints", expect(manualderive))]
impl fmt::Debug for MarkChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Keep => "Keep",
            Self::Redact => "Redact",
            Self::Map { .. } => "Map",
        })
    }
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MarksReply {
    pub report: DraftReport,
    pub rechecked: Vec<String>,
    #[serde(default)]
    pub learned: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendRequest {
    pub server: String,
    pub signed: bool,
    pub confirm: SendConfirm,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PushRequest {
    pub hub: String,
    pub confirm: SendConfirm,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PushedReply {
    pub hub: String,
    pub user: String,
    pub sessions: usize,
    pub inserted: u64,
    pub skipped: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SendConfirm {
    pub sessions: usize,
    pub kept_flags: usize,
}

#[serde_with::skip_serializing_none]
#[serde_with::serde_as]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct SentReply {
    pub code: String,

    #[serde_as(as = "crate::time::Rfc3339")]
    pub expires_at: Timestamp,
    pub bytes: u64,
    #[serde(default)]
    pub signed: Option<String>,
}

#[cfg_attr(dylint_lib = "rr_lints", expect(manualderive))]
impl fmt::Debug for SentReply {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let id = self
            .code
            .split_once('#')
            .map_or(self.code.as_str(), |(id, _)| id);
        formatter
            .debug_struct("SentReply")
            .field("code", &format_args!("{id}#.."))
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServersReply {
    pub servers: Vec<ServerView>,
    pub cloud_disabled: bool,
    pub logins_file: String,
    pub token_store: TokenStore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TokenStore {
    Keyring,
    File,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ServerView {
    pub origin: String,
    pub kind: ServerKind,
    #[serde(default)]
    pub hub: Option<String>,
    #[serde(default)]
    pub user: Option<String>,
    pub signed_in: bool,
    pub default: bool,
    pub delivery: Delivery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Delivery {
    Push,
    Code,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ServerKind {
    Cloud,
    Team,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LoginRequest {
    Cloud { origin: String },
    Team { origin: String, token: String },
}

#[cfg_attr(dylint_lib = "rr_lints", expect(manualderive))]
impl fmt::Debug for LoginRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cloud { origin } => write!(formatter, "LoginRequest::Cloud({origin})"),
            Self::Team { origin, .. } => write!(formatter, "LoginRequest::Team({origin}, ..)"),
        }
    }
}

#[serde_with::skip_serializing_none]
#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum LoginStep {
    Started {
        login: String,
        user_code: String,
        verification_uri_complete: String,
        interval: u64,

        #[serde_as(as = "crate::time::Rfc3339")]
        expires_at: Timestamp,
    },
    Pending {
        interval: u64,
    },
    SignedIn {
        origin: String,
        #[serde(default)]
        user: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PollRequest {
    pub login: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LogoutRequest {
    pub origin: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct KeyReply {
    pub key: String,
    pub fingerprint: String,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ShareRefusal {
    pub error: String,
    pub reason: RefusalReason,
    #[serde(default)]
    pub upstream_status: Option<u16>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RefusalReason {
    NotLocal,
    UnknownDraft,
    UnknownBundle,
    Full,
    Invalid,
    OpenFlags,
    Counts,
    DecideUnconfigured,
    UnknownSessions,
    SignedOut,
    LoginExpired,
    LoginDenied,
    UnknownLogin,
    BadToken,
    BadServer,
    DropBox,
    Unreachable,
    Failed,
}
