use super::{BrowseWindow, TagWire};
use crate::message::Role;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(
    schemars::JsonSchema,
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
    schemars::JsonSchema,
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

#[cfg(test)]
mod summary_kind_tests {
    use super::SummaryKind;

    #[test]
    fn canonical_depths_round_trip_and_deeper_rows_are_roots() {
        for kind in [SummaryKind::Leaf, SummaryKind::Branch, SummaryKind::Root] {
            assert_eq!(SummaryKind::from_depth(i64::from(kind.depth())), Some(kind));
        }
        assert_eq!(SummaryKind::from_depth(4), Some(SummaryKind::Root));
        assert_eq!(SummaryKind::from_depth(-1), None);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CaptureStatus {
    Unknown,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct MemorySearchToolArgs {
    pub since: Option<DateTime<Utc>>,
    pub before: Option<DateTime<Utc>>,
    pub window: Option<BrowseWindow>,
    pub expand: Option<bool>,
    pub synthesize: Option<bool>,
    pub query: String,
    pub mode: Option<SearchMode>,
    pub scope: Option<MemoryScope>,
    pub limit: Option<u32>,
    pub agent: Option<String>,
    pub role: Option<Role>,
    pub tool_name: Option<String>,
    pub tag: Option<String>,
}

#[derive(schemars::JsonSchema, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MemoryScope {
    Messages,
    Summaries,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MemoryHitSource {
    Message,
    Summary,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemorySearchHit {
    pub chunk: Option<crate::memory::MemoryChunkMatch>,
    pub id: String,
    pub source: MemoryHitSource,
    pub content: String,
    pub created_at: DateTime<Utc>,
    pub score: f64,
    pub role: Option<Role>,
    pub agent: Option<String>,
    pub kind: Option<SummaryKind>,
    pub tool_name: Option<String>,
    pub conversation_id: Option<String>,
    pub session_id: Option<String>,
    pub title: Option<String>,
    pub fallback: Option<MemoryFallback>,
    pub tags: Option<Vec<TagWire>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub enum MemoryFallback {
    Trigram,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemoryDegraded {
    pub reason: String,
    pub effect: String,
}

#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct MemorySearchResponse {
    pub query: String,
    pub scope: MemoryScope,
    pub degraded: Option<MemoryDegraded>,
    pub fallback: Option<MemoryFallback>,
    pub results: Vec<MemorySearchHit>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expanded: Vec<crate::memory::MemoryEntry>,
    pub synthesis: Option<String>,
}
