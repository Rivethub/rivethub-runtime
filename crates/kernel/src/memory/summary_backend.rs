use jiff::Timestamp;
use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
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
    pub manifest: SummaryArtifact,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SummaryArtifact(Box<RawValue>);

impl SummaryArtifact {
    pub fn encode(value: &impl Serialize) -> Result<Self, serde_json::Error> {
        serde_json::value::to_raw_value(&serde_json::to_value(value)?).map(Self)
    }

    pub fn decode<T: serde::de::DeserializeOwned>(&self) -> Result<T, serde_json::Error> {
        serde_json::from_str(self.as_json())
    }

    #[must_use]
    pub fn as_json(&self) -> &str {
        self.0.get()
    }

    #[must_use]
    pub fn is_null(&self) -> bool {
        self.as_json() == "null"
    }
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

pub const SUMMARY_EMBEDDING_WIDTH: usize = 1024;

impl SummaryIndex {
    pub fn validate_width(&self) -> Result<(), super::EmbeddingStoreError> {
        std::iter::once(self.pooled.len())
            .chain(self.passages.iter().map(|passage| passage.vector.len()))
            .try_for_each(check_summary_embedding_width)
    }
}

pub fn check_summary_embedding_width(got: usize) -> Result<(), super::EmbeddingStoreError> {
    if got == SUMMARY_EMBEDDING_WIDTH {
        Ok(())
    } else {
        Err(super::EmbeddingStoreError::Width {
            got,
            expected: SUMMARY_EMBEDDING_WIDTH,
        })
    }
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
