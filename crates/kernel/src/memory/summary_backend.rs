use jiff::Timestamp;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;
use uuid::Uuid;

use super::{MemoryEntry, summary_reference::PreparedSummaryRecord};

#[derive(Debug)]
pub struct SummaryInput {
    pub owner: String,
    pub expected_ids: BTreeSet<Uuid>,
    pub expected_root: Option<Uuid>,
    pub messages: Vec<MemoryEntry>,
    pub parts: Vec<PreparedSummaryRecord>,
}

#[derive(Debug)]
pub struct SummaryRun {
    pub id: Uuid,
    pub revision: i64,
    pub manifest: Value,
}

#[serde_with::serde_as]
#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct SummaryPassage {
    pub content: String,
    pub chars: std::ops::Range<i32>,
    #[serde_as(as = "values::time::Rfc3339")]
    pub earliest_at: Timestamp,
    #[serde_as(as = "values::time::Rfc3339")]
    pub latest_at: Timestamp,
    pub vector: Vec<f32>,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
pub struct SummaryIndex {
    pub passages: Vec<SummaryPassage>,
    pub pooled: Vec<f32>,
}

pub type SummaryIndexes = BTreeMap<Uuid, SummaryIndex>;

pub trait SummaryPermit: Send {}
impl<T: Send> SummaryPermit for T {}

#[must_use]
pub fn index_key(endpoint: &str, prefix: Option<&str>, model: &str, width: u32) -> [u8; 32] {
    use sha2::{Digest as _, Sha256};
    let mut hash = Sha256::new();
    for field in [
        "summary-passages-v1",
        endpoint,
        prefix.unwrap_or("/v1"),
        model,
    ] {
        hash.update((field.len() as u64).to_le_bytes());
        hash.update(field.as_bytes());
    }
    hash.update(width.to_le_bytes());
    hash.finalize().into()
}

#[derive(Debug)]
pub struct SummaryRepair {
    pub id: Uuid,
    pub content: String,
    pub input_key: Vec<u8>,
    pub sources: Vec<(i32, Timestamp)>,
    pub sections: Vec<super::summary_reference::SummarySectionBounds>,
}
