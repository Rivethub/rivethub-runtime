use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::memory::Provenance;
use crate::session_id::SessionId;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WikiQuery {
    pub q: Option<String>,
    pub tag: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct WikiPageQuery {
    pub slug: WikiSlug,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(try_from = "String", into = "String")]
pub struct WikiSlug(String);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid wiki slug")]
pub struct WikiSlugError;

impl TryFrom<String> for WikiSlug {
    type Error = WikiSlugError;

    fn try_from(slug: String) -> Result<Self, Self::Error> {
        if slug.trim().is_empty()
            || matches!(slug.as_str(), "." | "..")
            || slug
                .chars()
                .any(|ch| ch.is_control() || matches!(ch, '/' | '\\'))
        {
            return Err(WikiSlugError);
        }
        Ok(Self(slug))
    }
}

impl AsRef<str> for WikiSlug {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<WikiSlug> for String {
    fn from(slug: WikiSlug) -> Self {
        slug.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WikiHistoryEntry {
    #[schemars(with = "String")]
    pub date: NaiveDate,
    pub title: String,
    pub body: String,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WikiCitation {
    pub summary_id: String,
    #[serde(default)]
    #[serde_as(as = "Option<crate::time::Millis>")]
    pub date: Option<DateTime<Utc>>,
    pub kind: Option<String>,
    pub note: Option<String>,
    #[serde(default)]
    pub conversation_id: Option<String>,
    #[serde(default)]
    pub session_id: Option<SessionId>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub channel: Option<String>,
    #[serde(default)]
    pub topic_id: Option<String>,
    #[serde(default)]
    pub imported: Option<Provenance>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WikiSource {
    pub kind: WikiSourceKind,
    pub ids: Vec<String>,
    pub conversation_id: Option<String>,
    pub span: Option<WikiSourceSpan>,
}

#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    strum::EnumString,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum WikiSourceKind {
    Summary,
    Message,
    Conversation,
    Task,
    Leaf,
    ExtractWiki,
    User,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct WikiPageEdit {
    pub current_state: Option<String>,
    pub article: Option<String>,
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WikiSourceSpan {
    #[serde_as(as = "crate::time::Millis")]
    pub earliest: DateTime<Utc>,
    #[serde_as(as = "crate::time::Millis")]
    pub latest: DateTime<Utc>,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WikiPageResponse {
    pub slug: String,
    pub title: String,
    pub aliases: Vec<String>,
    pub tags: Vec<String>,
    pub entities: Vec<String>,
    pub current_state: String,
    pub article: Option<String>,
    pub see_also: Option<Vec<String>>,
    pub history: Vec<WikiHistoryEntry>,
    pub citations: Vec<WikiCitation>,
    pub markdown: String,
    pub sources: Vec<WikiSource>,
    pub git_sha: Option<String>,
    #[serde(default)]
    #[serde_as(as = "Option<crate::time::Millis>")]
    pub last_verified: Option<DateTime<Utc>>,
    #[serde_as(as = "crate::time::Millis")]
    pub updated_at: DateTime<Utc>,
    pub related: Option<Vec<String>>,
    #[serde(default)]
    pub topic_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WikiMissSuggestion {
    pub slug: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WikiMissBody {
    pub error: String,
    pub suggestions: Vec<WikiMissSuggestion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WikiRedLink {
    pub entity: String,
    pub referenced_by: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WikiGapsResponse {
    pub red_links: Vec<WikiRedLink>,
    pub stalest: Vec<WikiTopic>,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WikiTopic {
    pub slug: String,
    pub title: String,
    pub tags: Vec<String>,
    pub entities: Vec<String>,
    #[serde_as(as = "crate::time::Millis")]
    pub updated_at: DateTime<Utc>,
    pub excerpt: String,
    #[serde(default)]
    pub topic_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct WikiTopicList {
    pub topics: Vec<WikiTopic>,
    pub total: u64,
}
