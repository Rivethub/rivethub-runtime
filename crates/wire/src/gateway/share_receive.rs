use std::fmt;

use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;

use crate::capture::ReceiptVerification;
use crate::capture::ReceiveDecision;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(transparent)]
pub struct CodeText(pub String);

#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        manualderive,
        reason = "CONTRACT note 152: a share code carries the bundle key; Debug must never print it"
    )
)]
impl fmt::Debug for CodeText {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CodeText(..)")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReceiveRequest {
    pub code: CodeText,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SessionChoice {
    pub n: usize,
    pub decision: ReceiveDecision,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ImportRequest {
    pub code: CodeText,
    #[serde(default)]
    pub sessions: Vec<SessionChoice>,
    #[serde(default)]
    pub force: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct BundleSigner {
    pub key: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FindingFamily {
    Secret,
    Injection,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct FindingRow {
    #[serde(default)]
    pub event: Option<String>,
    pub field: String,
    pub rule: String,
    pub family: FindingFamily,
    pub kind: String,
    pub count: usize,
    pub excerpt: String,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct InboundSession {
    pub n: usize,
    #[serde(default)]
    pub author: Option<String>,
    pub title: String,
    pub agent: String,
    pub messages: usize,
    #[serde(default)]
    pub created_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub updated_at: Option<DateTime<Utc>>,
    pub findings: usize,
    pub rows: Vec<FindingRow>,
    pub secret: bool,
    pub allowed: Vec<ReceiveDecision>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReceiveReport {
    pub bundle: String,
    pub server: String,
    pub downloaded: bool,
    pub author: String,
    pub node: String,
    pub verification: ReceiptVerification,
    pub reason: String,
    #[serde(default)]
    pub signer: Option<BundleSigner>,
    pub sessions: Vec<InboundSession>,
    pub author_findings: usize,
    pub author_rows: Vec<FindingRow>,
    pub summary: String,
    pub held: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImportVerdict {
    Clean,
    Import,
    HoldSpans,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ImportedSession {
    pub n: usize,
    pub key: String,
    pub verification: ReceiptVerification,
    pub verdict: ImportVerdict,
    pub inserted: u64,
    pub skipped: u64,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportReply {
    pub bundle: String,
    pub received_at: DateTime<Utc>,
    pub author: String,
    pub verification: ReceiptVerification,
    #[serde(default)]
    pub signer: Option<BundleSigner>,
    pub imported: Vec<ImportedSession>,
    pub skipped: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TrustedSender {
    pub name: String,
    pub key: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TrustReply {
    pub senders: Vec<TrustedSender>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TrustRequest {
    pub name: String,
    pub key: String,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UntrustRequest {
    pub name: String,
    #[serde(default)]
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct UntrustReply {
    pub removed: usize,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct ReceiveRefusal {
    pub error: String,
    pub reason: ReceiveReason,
    #[serde(default)]
    pub sessions: Option<Vec<usize>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReceiveReason {
    NotLocal,
    Invalid,
    BadCode,
    AlreadyReceived,
    Gone,
    Burned,
    TooLarge,
    Unreachable,
    Refused,
    Undecryptable,
    Unreadable,
    BadSignature,
    Raw,
    Withheld,
    NoSuchSession,
    Undecided,
    SecretImport,
    AuthorFindings,
    BadKey,
    BadName,
    Duplicate,
    Failed,
}
