use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

use crate::events::ProtocolVersion;
use crate::gateway::wiki::WikiPageResponse;
use crate::gateway::wiki::WikiSlug;
use crate::session_id::SessionId;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum TopicDirectory {
    Local { node: String, path: String },
    Shared { path: String },
}

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
    schemars::JsonSchema,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "lowercase")]
pub enum TopicKind {
    #[default]
    Work,
    Type,
    Entity,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum PlacedBy {
    You,
    Categorizer,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopicRecord {
    pub id: String,
    #[serde(default)]
    pub kind: TopicKind,
    pub slug: String,
    pub name: String,
    pub aliases: Vec<String>,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub directory: Option<TopicDirectory>,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub created_at: DateTime<Utc>,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub visibility_mark: Option<String>,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopicSummary {
    #[serde(flatten)]
    pub topic: TopicRecord,
    pub session_count: u64,
    #[serde(default)]
    #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
    pub last_active: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopicList {
    pub topics: Vec<TopicSummary>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TopicCreateRequest {
    #[serde(default)]
    pub kind: TopicKind,
    pub name: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub directory: Option<TopicDirectory>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct TopicUpdate {
    pub name: Option<String>,
    pub aliases: Option<Vec<String>>,
    #[serde(default, with = "::serde_with::rust::double_option")]
    #[schemars(with = "Option<Option<String>>")]
    pub parent: Option<Option<String>>,
    #[serde(default, with = "::serde_with::rust::double_option")]
    #[schemars(with = "Option<Option<TopicDirectory>>")]
    pub directory: Option<Option<TopicDirectory>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "type", rename = "topic.changed", rename_all = "camelCase")]
pub struct TopicChangedNotice {
    pub v: ProtocolVersion,
    pub topic: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TopicMergeRequest {
    pub into: String,
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopicPlacement {
    pub session_id: SessionId,
    pub topic_id: String,
    pub by: PlacedBy,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub at: DateTime<Utc>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopicSuggestion {
    pub name: String,
    #[serde(default)]
    pub topic_id: Option<String>,
    #[serde(default)]
    pub parent_id: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlacementReply {
    #[serde(default)]
    pub placement: Option<TopicPlacement>,
    #[serde(default)]
    pub suggestion: Option<TopicSuggestion>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PlacementRequest {
    pub topic_id: String,
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionHistoryMark {
    pub session_id: SessionId,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub marked_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionHistoryList {
    pub sessions: Vec<SessionHistoryMark>,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopicSession {
    #[serde(flatten)]
    pub placement: TopicPlacement,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
    pub last_active: Option<DateTime<Utc>>,
    #[serde(default)]
    pub node: Option<String>,
    #[serde(default)]
    pub parent_session_id: Option<SessionId>,
    #[serde(default)]
    pub task_id: Option<String>,
    #[serde(default)]
    pub visibility: Option<String>,
    #[serde(default)]
    pub visibility_mark: Option<String>,
}

#[serde_as]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPlacement {
    pub conversation_id: String,
    pub topic_id: String,
    pub by: PlacedBy,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub at: DateTime<Utc>,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopicConversation {
    #[serde(flatten)]
    pub placement: ConversationPlacement,
    pub channel: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    #[serde_as(as = "Option<serde_with::TimestampMilliSeconds>")]
    pub last_active: Option<DateTime<Utc>>,
    #[serde(default)]
    pub node: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopicSessionList {
    pub sessions: Vec<TopicSession>,
    #[serde(default)]
    pub conversations: Vec<TopicConversation>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopicFiling {
    pub sessions: Vec<TopicSession>,
    pub conversations: Vec<TopicConversation>,
    pub history: Vec<SessionHistoryMark>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionSuggestion {
    pub session_id: SessionId,
    #[serde(flatten)]
    pub suggestion: TopicSuggestion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SessionSuggestionList {
    pub sessions: Vec<SessionSuggestion>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ConversationPlacementReply {
    #[serde(default)]
    pub placement: Option<ConversationPlacement>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopicPage {
    pub topic: TopicRecord,
    #[serde(default)]
    pub page: Option<WikiPageResponse>,
    #[serde(default)]
    pub mentions: Vec<TopicMention>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopicMention {
    pub topic_id: String,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum StartupKind {
    Topic,
    Recent,
    Relevant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum StartupSource {
    Session(SessionId),
    Wiki(WikiSlug),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StartupSection {
    pub kind: StartupKind,
    pub title: String,
    pub text: String,
    pub sources: Vec<StartupSource>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct StartupContext {
    pub topic_id: String,
    pub sections: Vec<StartupSection>,
    pub tokens: usize,
    pub delivered_via: DeliveredVia,
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
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "camelCase")]
pub enum DeliveredVia {
    MetaAppend,
    CliFlag,
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum DecisionStatus {
    Active,
    Superseded,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopicDecision {
    pub id: String,
    pub topic_id: String,
    pub text: String,
    pub status: DecisionStatus,
    #[serde(default)]
    pub superseded_by: Option<String>,
    pub by: String,
    #[serde(default)]
    pub session_id: Option<SessionId>,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub created_at: DateTime<Utc>,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopicDecisionList {
    pub decisions: Vec<TopicDecision>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DecisionCreateRequest {
    pub text: String,
    #[serde(default)]
    pub session_id: Option<SessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct DecisionUpdate {
    pub text: String,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields, rename_all = "camelCase")]
pub struct DecisionSupersedeRequest {
    pub text: Option<String>,
    pub session_id: Option<SessionId>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct DecisionSuperseded {
    pub superseded: TopicDecision,
    #[serde(default)]
    pub replacement: Option<TopicDecision>,
}
