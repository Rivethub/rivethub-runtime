use chrono::DateTime;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;

use crate::gateway::session_catalog::SessionSummaryRow;
use crate::gateway::session_catalog::SharedWith;
use crate::gateway::session_catalog::TopicPath;
use crate::memory::Provenance;
use crate::session_id::SessionId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EntityTypeInfo {
    #[serde(rename = "type")]
    pub key: String,
    pub type_name: String,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionMention {
    pub id: SessionId,
    pub title: String,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FactSource {
    pub session: SessionMention,
    pub at: DateTime<Utc>,
    pub value: String,
    #[serde(default)]
    pub imported_from: Option<Provenance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FactConflict {
    pub candidates: Vec<FactSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct FactPrevious {
    pub value: String,
    pub until: DateTime<Utc>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct Fact {
    pub id: String,
    pub key: String,
    pub value: String,
    pub since: DateTime<Utc>,
    pub pinned: bool,
    pub sources: Vec<FactSource>,
    #[serde(default)]
    pub conflict: Option<FactConflict>,
    #[serde(default)]
    pub previous: Option<FactPrevious>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EntitySummary {
    pub id: String,
    pub title: String,
    #[serde(rename = "type")]
    pub entity_type: String,
    pub type_name: String,
    pub session_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum ChangeKind {
    Fact,
    Decision,
    New,
    Filed,
    Conflict,
    Tag,
    Merge,
    History,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TimelineEntry {
    pub at: DateTime<Utc>,
    pub text: String,
    pub kind: ChangeKind,
    #[serde(default)]
    pub source: Option<SessionMention>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConnectedEntity {
    pub id: String,
    pub title: String,
    pub shared_sessions: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum AssignmentState {
    Applied,
    Suggested,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagAssignment {
    pub key: String,
    pub value: String,
    pub state: AssignmentState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum Freshness {
    Live,
    Sample,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EntityArticle {
    #[serde(flatten)]
    pub summary: EntitySummary,
    pub aliases: Vec<String>,
    #[serde(default)]
    pub topic: Option<TopicPath>,
    pub visibility: SharedWith,
    pub lead: String,
    pub facts: Vec<Fact>,
    pub timeline: Vec<TimelineEntry>,
    pub connected: Vec<ConnectedEntity>,
    pub sessions: Vec<SessionSummaryRow>,
    pub tags: Vec<TagAssignment>,
    pub freshness: Freshness,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct FactEdit {
    #[serde(default)]
    pub id: Option<String>,
    pub key: String,
    pub value: String,
    pub pinned: bool,
}
