use super::{
    MemoryDegraded, MemoryEntry, MemoryFallback, MemorySearchResult, SummaryKind, TagWire,
};
use jiff::Timestamp;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[serde_with::serde_as]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SummaryDetail {
    pub kind: SummaryKind,

    #[serde_as(as = "Option<values::time::Rfc3339>")]
    pub earliest_at: Option<Timestamp>,
    #[serde_as(as = "Option<values::time::Rfc3339>")]
    pub latest_at: Option<Timestamp>,
    pub tags: Vec<TagWire>,
    pub children: Vec<SummaryExcerpt>,
    pub sources: Vec<MemoryEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<super::PassageSection>,
    pub provenance: Option<super::Provenance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SummaryExcerpt {
    pub kind: SummaryKind,
    pub content: String,
}

impl MemorySearchResult {
    pub fn detail_id(&self) -> &str {
        self.passage
            .as_ref()
            .map_or(&self.id, |passage| &passage.id)
    }
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
