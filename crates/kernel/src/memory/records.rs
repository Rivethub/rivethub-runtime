use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_with::serde_as;
use values::{JsonObject, Role};

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryEntry {
    pub id: Option<String>,
    pub session_id: String,
    pub agent: String,
    pub channel: String,
    pub role: Role,
    pub content: String,
    pub tool_name: Option<String>,
    pub tool_args: Option<JsonObject>,
    pub tool_result: Option<String>,
    pub metadata: Option<JsonObject>,
    pub created_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySearchResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chunk: Option<MemoryChunkMatch>,
    pub id: String,
    pub source: MemoryHitSource,
    pub content: String,
    pub role: Option<Role>,
    pub agent: Option<String>,
    pub relevance_score: f64,
    pub created_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryChunkMatch {
    pub index: u32,
    pub total: u32,
    pub char_start: u32,
    pub char_end: u32,
    pub content: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MemoryHitSource {
    Message,
    Summary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MemoryScope {
    Messages,
    Summaries,
    Both,
}

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    Serialize,
    Deserialize,
    strum::Display,
    strum::EnumString,
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SearchMode {
    #[default]
    Hybrid,
    Fts,
    Vector,
    Trigram,
    Regex,
}

#[derive(
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
)]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum SummaryKind {
    Leaf,
    Branch,
    Root,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MemoryFallback {
    Trigram,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDegraded {
    pub reason: String,
    pub effect: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MemoryBrowseOrder {
    Asc,
    #[default]
    Desc,
}

#[derive(
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

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TopTool {
    pub tool: String,
    pub count: u64,
}

#[serde_as]
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemorySummaryNode {
    pub id: String,
    pub parent_id: Option<String>,
    pub level: u32,
    pub kind: SummaryKind,
    pub content: String,
    pub message_count: u32,
    pub earliest_at: Option<DateTime<Utc>>,
    pub latest_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
    pub source_message_ids: Vec<String>,
    pub vector: VectorState,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub truncated: bool,
    #[serde(default)]
    pub children: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, strum::EnumString)]
#[serde(rename_all = "camelCase")]
#[strum(serialize_all = "camelCase")]
pub enum VectorState {
    Embedded,
    Pending,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryMessageTally {
    pub total: u32,
    pub user: u32,
    pub assistant: u32,
    pub system: u32,
    pub tool: u32,
    pub embedded: u32,
    pub pending: u32,
    pub failed: u32,
    pub skipped: u32,
}

impl SummaryKind {
    #[must_use]
    pub const fn depth(self) -> i32 {
        match self {
            Self::Leaf => 0,
            Self::Branch => 1,
            Self::Root => 2,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Leaf => "leaf",
            Self::Branch => "branch",
            Self::Root => "root",
        }
    }

    #[must_use]
    pub const fn from_depth(depth: i64) -> Option<Self> {
        match depth {
            0 => Some(Self::Leaf),
            1 => Some(Self::Branch),
            2.. => Some(Self::Root),
            _ => None,
        }
    }
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
