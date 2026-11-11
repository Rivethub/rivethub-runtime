use std::fmt;
use std::fmt::Write as _;
use std::str::FromStr;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use thiserror::Error;
use uuid::Uuid;

use super::SessionChoices;

pub const MAX_DRAFTS: usize = 4;
pub const DRAFT_IDLE: std::time::Duration = std::time::Duration::from_mins(30);
pub const MAX_DRAFT_BYTES: usize = 256 << 20;

#[async_trait]
pub trait ShareDesk: Send + Sync + fmt::Debug {
    async fn find(&self, query: FindQuery) -> Result<Found, DeskError>;

    async fn draft(&self, request: DraftRequest) -> Result<Draft, DeskError>;

    async fn report(&self, id: DraftId) -> Result<Draft, DeskError>;

    async fn mark(&self, id: DraftId, marks: Vec<Mark>) -> Result<Marked, DeskError>;

    async fn send(&self, id: DraftId, send: SendRequest) -> Result<Sent, DeskError>;

    async fn discard(&self, id: DraftId) -> Result<(), DeskError>;

    async fn servers(&self) -> Result<Servers, DeskError>;

    async fn login(&self, request: LoginRequest) -> Result<LoginStep, DeskError>;

    async fn poll(&self, login: LoginId) -> Result<LoginStep, DeskError>;

    async fn logout(&self, origin: ServerOrigin) -> Result<(), DeskError>;

    async fn key(&self) -> Result<NodeKeyView, DeskError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DraftId(Uuid);

impl DraftId {
    #[must_use]
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for DraftId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for DraftId {
    type Err = DeskError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Self).map_err(|_| DeskError::UnknownDraft)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct LoginId(Uuid);

impl LoginId {
    #[must_use]
    pub fn random() -> Self {
        Self(Uuid::new_v4())
    }
}

impl fmt::Display for LoginId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for LoginId {
    type Err = DeskError;

    fn from_str(text: &str) -> Result<Self, Self::Err> {
        text.parse().map(Self).map_err(|_| DeskError::UnknownLogin)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindQuery {
    pub query: String,
    pub since: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
    pub agent: Option<String>,
    pub tag: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Found {
    pub sessions: Vec<FoundSession>,
    pub dropped: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FoundSession {
    pub id: Uuid,
    pub title: Option<String>,
    pub agent: String,
    pub first: DateTime<Utc>,
    pub last: DateTime<Utc>,
    pub messages: u64,
    pub best: f64,
    pub hits: usize,
    pub excerpts: Vec<String>,
    pub imported_from: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftRequest {
    pub sessions: SessionChoices,
    pub decide: bool,
    pub block: bool,
}

#[derive(Clone, PartialEq)]
pub struct Draft {
    pub id: DraftId,
    pub expires_at: DateTime<Utc>,
    pub totals: DraftTotals,
    pub rules: Vec<RuleCount>,
    pub sessions: Vec<DraftSession>,
    pub not_scanned: String,
    pub best_effort: String,
    pub decision: Option<DraftDecision>,
    pub flags: Vec<FlagView>,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 151: a draft's flags carry flagged text; Debug shows counts only"
    )
)]
impl fmt::Debug for Draft {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Draft")
            .field("id", &self.id)
            .field("sessions", &self.sessions.len())
            .field("flags", &self.flags.len())
            .finish_non_exhaustive()
    }
}

impl Draft {
    #[must_use]
    pub fn open_flags(&self) -> usize {
        self.flags
            .iter()
            .filter(|flag| flag.state == FlagState::Open)
            .count()
    }

    #[must_use]
    pub fn kept_flags(&self) -> usize {
        self.flags
            .iter()
            .filter(|flag| flag.state == FlagState::Keep)
            .count()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DraftTotals {
    pub secrets: usize,
    pub mapped: usize,
    pub redacted: usize,
    pub escaped: usize,
    pub rechecked: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleCount {
    pub rule: String,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftSession {
    pub id: Uuid,
    pub agent: String,
    pub messages: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftDecision {
    pub model: String,
    pub scored: usize,
    pub calls: usize,
    pub trivial: usize,
    pub mode: super::DecideMode,
}

#[derive(Clone, PartialEq)]
pub struct FlagView {
    pub number: usize,
    pub score: f64,
    pub session: Option<Uuid>,
    pub place: Option<super::FlagPlace>,
    pub length: usize,
    pub text: EscapedText,
    pub state: FlagState,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 151: flagged text leaves rh only in the share report body; Debug never prints it"
    )
)]
impl fmt::Debug for FlagView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FlagView")
            .field("number", &self.number)
            .field("length", &self.length)
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct EscapedText(String);

impl EscapedText {
    #[must_use]
    pub fn escape(text: &str) -> Self {
        Self(Escaping(text).to_string())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 151: flagged text never reaches a log; Debug shows its length only"
    )
)]
impl fmt::Debug for EscapedText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "EscapedText({} bytes)", self.0.len())
    }
}

struct Escaping<'a>(&'a str);

impl fmt::Display for Escaping<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for c in self.0.chars() {
            if hidden(c) {
                write!(formatter, "<U+{:04X}>", u32::from(c))?;
            } else {
                formatter.write_char(c)?;
            }
        }
        Ok(())
    }
}

fn hidden(c: char) -> bool {
    (c.is_control() && c != '\n' && c != '\t')
        || matches!(
            c,
            '\u{00AD}'
                | '\u{034F}'
                | '\u{061C}'
                | '\u{115F}'
                | '\u{1160}'
                | '\u{17B4}'
                | '\u{17B5}'
                | '\u{180B}'..='\u{180F}'
                | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}'
                | '\u{2060}'..='\u{206F}'
                | '\u{3164}'
                | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}'
                | '\u{FFA0}'
                | '\u{FFF0}'..='\u{FFFB}'
                | '\u{E0000}'..='\u{E0FFF}'
        )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagState {
    Open,
    Keep,
    Redact,
    Map { term: String, placeholder: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mark {
    pub number: usize,
    pub choice: FlagChoice,
    pub learn: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FlagChoice {
    Keep,
    Redact,
    Map { term: String, placeholder: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Marked {
    pub draft: Draft,
    pub rechecked: Vec<String>,
    pub learned: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendRequest {
    pub server: ServerOrigin,
    pub signed: bool,
    pub confirm: SendConfirm,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendConfirm {
    pub sessions: usize,
    pub kept_flags: usize,
}

#[derive(Clone, PartialEq, Eq)]
pub struct Sent {
    pub code: String,
    pub expires_at: DateTime<Utc>,
    pub bytes: u64,
    pub signed: Option<String>,
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 140: the share code's key is a secret; Debug shows only the drop id part"
    )
)]
impl fmt::Debug for Sent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let id = self
            .code
            .split_once('#')
            .map_or(self.code.as_str(), |(id, _)| id);
        formatter
            .debug_struct("Sent")
            .field("code", &format_args!("{id}#.."))
            .field("bytes", &self.bytes)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ServerOrigin(String);

impl ServerOrigin {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for ServerOrigin {
    fn from(host: String) -> Self {
        Self(host)
    }
}

impl fmt::Display for ServerOrigin {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerKind {
    Cloud,
    Team,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Servers {
    pub servers: Vec<ServerView>,
    pub logins_file: String,
    pub token_store: TokenStore,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenStore {
    Keyring,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerView {
    pub origin: ServerOrigin,
    pub kind: ServerKind,
    pub user: Option<String>,
    pub signed_in: bool,
    pub default: bool,
}

#[derive(Clone, PartialEq, Eq)]
pub enum LoginRequest {
    Cloud { origin: ServerOrigin },
    Team { origin: ServerOrigin, token: String },
}

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 151: a team token never reaches a log; Debug names only the server"
    )
)]
impl fmt::Debug for LoginRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cloud { origin } => write!(formatter, "LoginRequest::Cloud({origin})"),
            Self::Team { origin, .. } => write!(formatter, "LoginRequest::Team({origin}, ..)"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginStep {
    Started {
        login: LoginId,
        user_code: String,
        verification_uri_complete: String,
        interval: u64,
        expires_at: DateTime<Utc>,
    },
    Pending {
        interval: u64,
    },
    SignedIn {
        origin: ServerOrigin,
        user: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeKeyView {
    pub key: String,
    pub fingerprint: String,
}

#[derive(Debug, Error)]
pub enum DeskError {
    #[error("no draft has that id; it was sent, discarded, idle for 30 minutes, or rh restarted")]
    UnknownDraft,
    #[error("rh already holds {MAX_DRAFTS} drafts or 256 MiB of them; send or discard one first")]
    Full,
    #[error("{0}")]
    Choices(String),
    #[error("{0}")]
    Marks(String),
    #[error("{open} flags are still open; mark each one keep, redact or map before sending")]
    OpenFlags { open: usize },
    #[error(
        "the confirmation says {confirmed_sessions} sessions and {confirmed_kept} kept flags, but the draft holds {sessions} and {kept}; review the draft again"
    )]
    Counts {
        sessions: usize,
        kept: usize,
        confirmed_sessions: usize,
        confirmed_kept: usize,
    },
    #[error(
        "the decision model is not configured; set memory.sanitize.decide.url, or draft without decide"
    )]
    DecideUnconfigured,
    #[error(
        "no session this node can share has the id {0}; it is unknown, trashed or not the node owner's"
    )]
    UnknownSessions(String),
    #[error("this machine is not signed in to {0}; sign in to it first")]
    SignedOut(ServerOrigin),
    #[error("the login on {0} expired or was revoked; sign in again")]
    LoginExpired(ServerOrigin),
    #[error("the sign-in was {0}; start it again")]
    LoginDenied(&'static str),
    #[error("no sign-in has that id; it finished, expired, or rh restarted")]
    UnknownLogin,
    #[error("the share server refused the token: {0}")]
    BadToken(String),
    #[error("the share server is not a valid host: {0}")]
    BadServer(String),
    #[error("{message}")]
    DropBox { status: u16, message: String },
    #[error("the share server could not be reached: {0}")]
    Unreachable(String),
    #[error("{0}")]
    Failed(String),
}
