use jiff::Timestamp;
use serde::Deserialize;
use serde::Serialize;

use crate::gateway::entities::FactSource;
use crate::gateway::entities::SessionMention;
use crate::session_id::SessionId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewGroup {
    Conflict,
    Fact,
    Filing,
    Tag,
    Tidy,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum SubjectRef {
    Topic { id: String, title: String },
    Entity { id: String, title: String },
    Session { id: SessionId, title: String },
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewOption {
    pub label: String,
    pub value: String,
    #[serde(default)]
    pub source: Option<FactSource>,
}

#[serde_with::skip_serializing_none]
#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewEvidence {
    pub session: SessionMention,

    #[serde_as(as = "crate::time::Rfc3339")]
    pub at: Timestamp,
    pub quote: String,
    #[serde(default)]
    pub highlight: Option<String>,
}

#[serde_with::skip_serializing_none]
#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ReviewItem {
    pub id: String,
    pub group: ReviewGroup,
    pub subject: SubjectRef,
    pub text: String,
    #[serde(default)]
    pub before: Option<String>,
    #[serde(default)]
    pub after: Option<String>,
    #[serde(default)]
    pub options: Option<Vec<ReviewOption>>,
    pub evidence: Vec<ReviewEvidence>,

    #[serde_as(as = "crate::time::Rfc3339")]
    pub created_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ReviewDecision {
    Accept,
    Reject,
    Later,
    Choose { value: String },
    Edit { value: String },
}
