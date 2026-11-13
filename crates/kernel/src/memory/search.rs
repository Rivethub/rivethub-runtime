use super::{
    MemoryDegraded, MemoryEntry, MemoryFallback, MemorySearchResult, SummaryKind, TagWire,
};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummaryDetail {
    pub kind: SummaryKind,
    pub earliest_at: Option<DateTime<Utc>>,
    pub latest_at: Option<DateTime<Utc>>,
    pub tags: Vec<TagWire>,
    pub children: Vec<SummaryExcerpt>,
    pub sources: Vec<MemoryEntry>,
    pub provenance: Option<super::Provenance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryExcerpt {
    pub kind: SummaryKind,
    pub content: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchOutcome {
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub summaries: BTreeMap<String, SummaryDetail>,
    pub results: Vec<MemorySearchResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub degraded: Option<MemoryDegraded>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback: Option<MemoryFallback>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expanded: Vec<MemoryEntry>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthesis: Option<String>,
}
