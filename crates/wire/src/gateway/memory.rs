mod search;
mod summaries;
pub use search::*;
pub use summaries::*;

use std::collections::BTreeMap;

use crate::JsonObject;
use crate::message::{Role, ToolResult};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MemoryToolName {
    MemorySearch,
    MemoryBrowse,
    MemoryStats,
    MemoryGetFull,
    MemoryAppend,
    MemoryIngestSession,
    MemoryTags,
}

#[derive(
    schemars::JsonSchema, Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "camelCase")]
pub enum MemoryBrowseOrder {
    Asc,
    #[default]
    Desc,
}

#[derive(
    schemars::JsonSchema,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
    strum::EnumIter,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum BrowseWindow {
    Today,
    Yesterday,
    ThisMorning,
    ThisWeek,
    #[serde(rename = "last_24h")]
    #[strum(serialize = "last_24h")]
    Last24h,
    #[serde(rename = "last_7d")]
    #[strum(serialize = "last_7d")]
    Last7d,
    #[serde(rename = "last_14d")]
    #[strum(serialize = "last_14d")]
    Last14d,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BrowseWindowError {
    #[error("window is empty")]
    Empty,
    #[error("unknown window {raw:?}; expected {expected}")]
    Unknown { raw: String, expected: String },
}

impl BrowseWindow {
    pub fn parse(raw: &str) -> Result<Self, BrowseWindowError> {
        let normalized = raw.trim().to_ascii_lowercase().replace(' ', "_");
        if normalized.is_empty() {
            return Err(BrowseWindowError::Empty);
        }
        std::str::FromStr::from_str(&normalized).map_err(|_| BrowseWindowError::Unknown {
            raw: raw.to_owned(),
            expected: Self::expected(),
        })
    }

    fn expected() -> String {
        <Self as strum::IntoEnumIterator>::iter()
            .map(|window| window.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MemoryBrowseToolArgs {
    pub conversation_id: Option<String>,
    pub include_tools: Option<bool>,
    pub since: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
    pub window: Option<BrowseWindow>,
    pub agent: Option<String>,
    pub tag: Option<String>,
    pub limit: Option<u32>,
    pub order: Option<MemoryBrowseOrder>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MemoryAppendToolArgs {
    pub session_id: String,
    pub content: String,
    pub role: Role,
    pub tool_name: Option<String>,
    pub tool_args: Option<JsonObject>,
    pub tool_result: Option<String>,
    pub event_id: Option<String>,
    pub agent: Option<String>,
    pub persona: Option<String>,
    pub source: Option<String>,
    pub channel: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MemoryIngestMessage {
    pub role: Role,
    pub content: String,
    pub created_at: Option<DateTime<Utc>>,
    pub tool_calls: Option<Vec<MemoryIngestToolCall>>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MemoryIngestToolCall {
    pub id: Option<String>,
    pub name: String,
    pub input: Option<JsonObject>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MemoryIngestSessionToolArgs {
    pub session_id: String,
    pub messages: Vec<MemoryIngestMessage>,
    pub agent: Option<String>,
    pub persona: Option<String>,
    pub source: Option<String>,
    pub channel: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MemoryGetFullToolArgs {
    pub id: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MemoryTagsToolArgs {
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    pub session_key: Option<String>,
    pub agent: Option<String>,
    pub key: Option<String>,
    pub value: Option<String>,
    pub tag: Option<String>,
    pub display: Option<String>,
    pub state: Option<TagState>,
    pub ids: Option<Vec<String>>,
    pub session_keys: Option<Vec<String>>,
    pub entries: Option<Vec<TagEntryPair>>,
    pub parent_value: Option<String>,
    pub aliases: Option<Vec<String>>,
    pub from: Option<String>,
    pub into: Option<String>,
    pub reason: Option<String>,
    pub decided_by: Option<String>,
    pub limit: Option<u32>,
}

#[derive(schemars::JsonSchema, Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TagEntryPair {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemoryContractResponse {
    pub contract_version: u32,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemoryToolResponse {
    pub ok: bool,
    pub result: ToolResult,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemoryBrowseMessage {
    pub owner: Option<String>,
    pub id: String,
    pub role: Role,
    pub agent: Option<String>,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub conversation_id: Option<String>,
    pub session_id: Option<String>,
    pub tool_name: Option<String>,
    pub tags: Option<Vec<TagWire>>,
    pub provenance: Option<crate::memory::Provenance>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemoryBrowseResponse {
    pub messages: Vec<MemoryBrowseMessage>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TopTool {
    pub tool: String,
    pub count: u64,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct RecentSession {
    pub session_id: String,
    pub title: Option<String>,
    pub agent: String,
    #[serde_as(as = "serde_with::TimestampMilliSeconds")]
    pub last_active: DateTime<Utc>,
    pub messages: u64,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemoryStatsResponse {
    pub conversations: u64,
    pub messages: u64,
    pub tool_calls: u64,
    pub summaries: u64,
    pub embed_queue_depth: u64,
    pub embedded_messages: u64,
    pub failed_embeddings: u64,
    pub top_tools: Vec<TopTool>,
    pub recent_sessions: Vec<RecentSession>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MemoryHealthStatus {
    Ok,
    Degraded,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum EmbedStatus {
    Ok,
    Unavailable,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct EmbedHealth {
    pub status: EmbedStatus,
    pub error: Option<String>,
    pub impact: Option<String>,
    pub checked_at: Option<DateTime<Utc>>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemoryHealthResponse {
    pub observed_at: Option<DateTime<Utc>>,
    pub queue_status: Option<QueueStatus>,
    pub queues: Option<QueueHealth>,
    #[serde(rename = "compaction")]
    pub summarizer: Option<SummarizerHealth>,
    pub failed_embeddings: Option<u64>,
    pub skipped_embeddings: Option<u64>,
    pub capture: Option<CaptureHealth>,
    pub status: MemoryHealthStatus,
    pub embeddings: EmbedHealth,
    pub embed_queue_depth: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum QueueStatus {
    Available,
    Unavailable,
    Restricted,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueHealth {
    pub task: String,
    pub pending: u64,
    pub running: u64,
    pub scheduled: u64,
    pub dead: u64,
    pub oldest_pending_minutes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct SummarizerHealth {
    pub eligible: bool,
    pub active_tail: bool,
    pub below_floor: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct CaptureHealth {
    pub status: CaptureStatus,
    pub impact: String,
}

#[derive(
    schemars::JsonSchema,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "snake_case")]
pub enum TagEntityType {
    Conversation,
    Summary,
}

#[derive(
    schemars::JsonSchema,
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "snake_case")]
pub enum TagState {
    Suggested,
    Accepted,
    Rejected,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagWire {
    pub id: String,
    pub entity_type: TagEntityType,
    pub entity_id: String,
    pub key: String,
    pub value: String,
    pub display: String,
    pub source: String,
    pub state: TagState,
    pub confidence: Option<f64>,
    pub proposed_by: String,
    pub reason: String,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PendingTagAction {
    Add,
    Remove,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PendingTagWire {
    #[serde(flatten)]
    pub tag: TagWire,
    pub action: Option<PendingTagAction>,
    pub session_key: Option<String>,
    pub title: Option<String>,
    pub agent: Option<String>,
    pub conversation_id: Option<String>,
    pub excerpt: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MemoryTagsDecideRequest {
    pub ids: Option<Vec<String>>,
    pub state: Option<TagState>,
    pub decided_by: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MemoryTagsAddRequest {
    pub entity_type: Option<String>,
    pub entity_id: Option<String>,
    pub session_key: Option<String>,
    pub agent: Option<String>,
    pub tag: Option<String>,
    pub key: Option<String>,
    pub value: Option<String>,
    pub display: Option<String>,
    pub reason: Option<String>,
    pub decided_by: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MemoryTagsLookupRequest {
    pub session_keys: Option<Vec<String>>,
    pub states: Option<Vec<String>>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagTaxonomyEntry {
    pub key: String,
    pub value: String,
    pub display: String,
    pub parent_value: Option<String>,
    pub aliases: Vec<String>,
    pub state: TagState,
    pub source: String,
    pub reason: String,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagTaxonomyWire {
    pub key: String,
    pub entries: Vec<TagTaxonomyEntry>,
    pub decided_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TagTaxonomyUpsertRequest {
    pub key: Option<String>,
    pub value: Option<String>,
    pub display: Option<String>,
    pub parent_value: Option<String>,
    pub aliases: Option<Vec<String>>,
    pub reason: Option<String>,
    pub decided_by: Option<String>,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TagTaxonomyDecideRequest {
    pub entries: Option<Vec<TagEntryPair>>,
    pub state: Option<TagState>,
    pub decided_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagTaxonomyMerge {
    pub key: String,
    pub from: String,
    pub into: String,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct TagTaxonomyMergeRequest {
    pub key: Option<String>,
    pub from: Option<String>,
    pub into: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagTaxonomyMergeResult {
    pub moved: u64,
    pub dropped: u64,
    pub into: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagCount {
    pub key: String,
    pub value: String,
    pub display: String,
    pub conversations: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagListResult {
    pub tags: Vec<TagWire>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct PendingTagListResult {
    pub tags: Vec<PendingTagWire>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagCountsResult {
    pub counts: Vec<TagCount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagTaxonomyListResult {
    pub entries: Vec<TagTaxonomyEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagDecisionResult {
    pub changed: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagAddResult {
    pub tag: TagWire,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagLookupResult {
    pub sessions: BTreeMap<String, Vec<TagWire>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagTaxonomyUpsertResult {
    pub entry: TagTaxonomyEntry,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRuleResult {
    pub key: String,
    pub rule: ProjectRule,
    pub git_root: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectRule {
    GitRemote,
    GitRoot,
    CwdBasename,
}
