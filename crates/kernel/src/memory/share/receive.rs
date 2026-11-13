use std::fmt;

use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use thiserror::Error;

#[async_trait]
pub trait ReceiveDesk: Send + Sync + fmt::Debug {
    async fn inspect(&self, code: ReceiveCode) -> Result<Inspected, ReceiveError>;

    async fn import(
        &self,
        code: ReceiveCode,
        request: ImportRequest,
    ) -> Result<Imported, ReceiveError>;

    async fn trusted(&self) -> Result<Vec<Sender>, ReceiveError>;

    async fn trust(&self, name: String, key: String) -> Result<Sender, ReceiveError>;

    async fn untrust(&self, name: String, key: Option<String>) -> Result<usize, ReceiveError>;
}

pub struct ReceiveCode(String);

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 140: a share code carries the bundle key; Debug must never print it"
    )
)]
impl fmt::Debug for ReceiveCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ReceiveCode(..)")
    }
}

impl ReceiveCode {
    #[must_use]
    pub const fn new(text: String) -> Self {
        Self(text)
    }

    #[must_use]
    pub fn reveal(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    Import,
    Skip,
    HoldSpans,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionChoice {
    pub number: usize,
    pub choice: Choice,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportRequest {
    pub sessions: Vec<SessionChoice>,
    pub force: bool,
    pub fresh_login: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    Verified,
    Unsigned,
    UnknownKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signer {
    pub key: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindingFamily {
    Secret,
    Injection,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingRow {
    pub event: Option<String>,
    pub field: String,
    pub rule: String,
    pub family: FindingFamily,
    pub kind: String,
    pub count: usize,
    pub excerpt: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inbound {
    pub number: usize,
    pub author: Option<String>,
    pub title: String,
    pub agent: String,
    pub messages: usize,
    pub created_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub findings: usize,
    pub rows: Vec<FindingRow>,
    pub secret: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inspected {
    pub bundle: String,
    pub server: String,
    pub downloaded: bool,
    pub author: String,
    pub node: String,
    pub verification: Verification,
    pub reason: String,
    pub signer: Option<Signer>,
    pub sessions: Vec<Inbound>,
    pub author_findings: usize,
    pub author_rows: Vec<FindingRow>,
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Clean,
    Import,
    HoldSpans,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedSession {
    pub number: usize,
    pub key: String,
    pub verification: Verification,
    pub verdict: Verdict,
    pub inserted: u64,
    pub skipped: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Imported {
    pub bundle: String,
    pub received_at: DateTime<Utc>,
    pub author: String,
    pub verification: Verification,
    pub signer: Option<Signer>,
    pub sessions: Vec<ImportedSession>,
    pub skipped: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sender {
    pub name: String,
    pub key: String,
    pub fingerprint: String,
}

#[derive(Debug, Error)]
pub enum ReceiveError {
    #[error("{0}")]
    BadCode(String),
    #[error("bundle {0} was already received on this machine; nothing was downloaded")]
    AlreadyReceived(String),
    #[error("{0}")]
    Gone(String),
    #[error("{0}")]
    Burned(String),
    #[error("the drop box sent more bytes than any drop holds, so the download was dropped")]
    TooLarge,
    #[error("{0}")]
    Unreachable(String),
    #[error("{0}")]
    Refused(String),
    #[error(
        "the bundle does not open with this share code's key; the code is wrong or the ciphertext was altered"
    )]
    Undecryptable,
    #[error("{0}")]
    Unreadable(String),
    #[error("{0}")]
    BadSignature(String),
    #[error(
        "the bundle is raw; a shared bundle must be sanitized by its sender, so nothing was imported"
    )]
    Raw,
    #[error("the bundle still withholds {0} flagged chunks; nothing was imported")]
    Withheld(usize),
    #[error("{0}")]
    NoSuchSession(String),
    #[error("{} still need a decision; nothing was imported", numbered(.0))]
    Undecided(Vec<usize>),
    #[error(
        "{} hold secret findings, which only skip or hold_spans may import unless force is set; nothing was imported",
        numbered(.0)
    )]
    SecretImport(Vec<usize>),
    #[error(
        "the bundle's own author fields carry findings; nothing was imported unless force is set"
    )]
    AuthorFindings,
    #[error("importing flagged content needs a fresh sign-in")]
    FreshLoginRequired,
    #[error("{0}")]
    BadKey(String),
    #[error("{0}")]
    BadName(String),
    #[error("{0}")]
    Duplicate(String),
    #[error("{0}")]
    Failed(String),
}

fn numbered(sessions: &[usize]) -> String {
    let numbers: Vec<String> = sessions.iter().map(ToString::to_string).collect();
    match numbers.as_slice() {
        [one] => format!("session {one}"),
        many => format!("sessions {}", many.join(", ")),
    }
}
