use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SummaryReferenceError {
    #[error("summary {0} is gone")]
    Gone(Uuid),
    #[error("summary {0} has changed")]
    Stale(Uuid),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SummaryConsumer {
    Wiki,
    Entity,
}

impl SummaryConsumer {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Wiki => "wiki",
            Self::Entity => "entity",
        }
    }
}

#[must_use]
pub fn embedding_input_hash(content: &str) -> String {
    use sha2::{Digest as _, Sha256};
    hex::encode(Sha256::digest(content.trim().as_bytes()))
}

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Utc};

use super::SummaryKind;

#[derive(Debug)]
pub struct PreparedSummaryTree {
    pub conversation: Uuid,
    pub owner: String,
    pub expected_ids: BTreeSet<Uuid>,
    pub expected_revision: Option<i64>,
    pub historical: bool,
    pub created_at: DateTime<Utc>,
    pub records: Vec<PreparedSummaryRecord>,
}

#[derive(Debug)]
pub struct PreparedSummaryRecord {
    pub id: Uuid,
    pub parent: Option<Uuid>,
    pub kind: SummaryKind,
    pub ordinal: i32,
    pub content: String,
    pub input_key: [u8; 32],
    pub index_key: Option<[u8; 32]>,
    pub model: Option<String>,
    pub issued_part_ordinal: Option<i32>,
    pub input_tokens: Option<i64>,
    pub quality_flags: i32,
    pub section: Option<SummarySectionBounds>,
    pub sources: Vec<SummaryManifestEntry>,
}

#[derive(Debug)]
pub struct SummarySectionBounds {
    pub first: i32,
    pub last: i32,
    pub verified: bool,
    pub char_start: i32,
    pub char_end: i32,
}

#[derive(Debug)]
pub struct SummaryManifestEntry {
    pub message: Uuid,
    pub ordinal: i32,
    pub render_key: Option<[u8; 32]>,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid prepared summary manifest")]
pub struct InvalidSummaryManifest;

pub type SummaryCoverage = BTreeMap<Uuid, BTreeSet<Uuid>>;

impl PreparedSummaryTree {
    pub fn coverage(&self) -> Result<SummaryCoverage, InvalidSummaryManifest> {
        let records: BTreeMap<_, _> = self.records.iter().map(|row| (row.id, row)).collect();
        if records.len() != self.records.len()
            || records.keys().any(|id| self.expected_ids.contains(id))
        {
            return Err(InvalidSummaryManifest);
        }
        let mut roots = records
            .values()
            .filter(|row| row.kind == SummaryKind::Session);
        let root = roots.next().ok_or(InvalidSummaryManifest)?;
        if roots.next().is_some() || root.parent.is_some() || root.ordinal != 0 {
            return Err(InvalidSummaryManifest);
        }

        let root_members: BTreeMap<_, _> = root
            .sources
            .iter()
            .map(|entry| (entry.ordinal, entry.message))
            .collect();
        if root_members
            .keys()
            .copied()
            .ne(1..=i32::try_from(root.sources.len()).map_err(|_| InvalidSummaryManifest)?)
        {
            return Err(InvalidSummaryManifest);
        }

        let mut occupied = BTreeSet::new();
        let mut coverages = BTreeMap::new();
        for row in &self.records {
            if row.ordinal < 0
                || row.quality_flags < 0
                || !occupied.insert((row.parent, row.kind.as_str(), row.ordinal))
            {
                return Err(InvalidSummaryManifest);
            }
            let members = match row.kind {
                SummaryKind::Session | SummaryKind::Part => {
                    if row.section.is_some() {
                        return Err(InvalidSummaryManifest);
                    }
                    if row.kind == SummaryKind::Part
                        && (row.parent != Some(root.id)
                            || row.ordinal == 0
                            || !row.issued_part_ordinal.is_some_and(|ordinal| ordinal > 0)
                            || !row.input_tokens.is_some_and(|tokens| tokens > 0))
                    {
                        return Err(InvalidSummaryManifest);
                    }
                    let mut ordinals = BTreeSet::new();
                    let mut members = BTreeSet::new();
                    for entry in &row.sources {
                        if root_members.get(&entry.ordinal) != Some(&entry.message)
                            || !ordinals.insert(entry.ordinal)
                            || !members.insert(entry.message)
                        {
                            return Err(InvalidSummaryManifest);
                        }
                    }
                    members
                }
                SummaryKind::Section => {
                    let parent = row
                        .parent
                        .and_then(|id| records.get(&id))
                        .ok_or(InvalidSummaryManifest)?;
                    let bounds = row.section.as_ref().ok_or(InvalidSummaryManifest)?;
                    if !matches!(parent.kind, SummaryKind::Session | SummaryKind::Part)
                        || !row.sources.is_empty()
                        || bounds.first > bounds.last
                        || bounds.char_start < 0
                        || bounds.char_end <= bounds.char_start
                        || usize::try_from(bounds.char_end).map_err(|_| InvalidSummaryManifest)?
                            > parent.content.chars().count()
                    {
                        return Err(InvalidSummaryManifest);
                    }
                    let first = parent.sources.iter().map(|entry| entry.ordinal).min();
                    let last = parent.sources.iter().map(|entry| entry.ordinal).max();
                    let starts = parent
                        .sources
                        .iter()
                        .any(|entry| entry.ordinal == bounds.first);
                    let ends = parent
                        .sources
                        .iter()
                        .any(|entry| entry.ordinal == bounds.last);
                    if !(starts && ends)
                        || (!bounds.verified
                            && (first != Some(bounds.first) || last != Some(bounds.last)))
                    {
                        return Err(InvalidSummaryManifest);
                    }
                    parent
                        .sources
                        .iter()
                        .filter(|entry| (bounds.first..=bounds.last).contains(&entry.ordinal))
                        .map(|entry| entry.message)
                        .collect()
                }
                _ => return Err(InvalidSummaryManifest),
            };
            if members.is_empty() {
                return Err(InvalidSummaryManifest);
            }
            coverages.insert(row.id, members);
        }

        let parts: Vec<_> = self
            .records
            .iter()
            .filter(|row| row.kind == SummaryKind::Part)
            .collect();
        if !parts.is_empty() {
            let part_members: BTreeSet<_> = parts
                .iter()
                .flat_map(|part| part.sources.iter().map(|entry| entry.message))
                .collect();
            if coverages.get(&root.id) != Some(&part_members) {
                return Err(InvalidSummaryManifest);
            }
        }
        Ok(coverages)
    }

    #[must_use]
    pub fn anchors(&self) -> BTreeMap<Uuid, Uuid> {
        let mut parts: Vec<_> = self
            .records
            .iter()
            .filter(|record| record.kind == SummaryKind::Part)
            .collect();
        if parts.is_empty() {
            parts.extend(
                self.records
                    .iter()
                    .filter(|record| record.kind == SummaryKind::Session),
            );
        }
        parts.sort_by_key(|record| record.ordinal);
        let mut anchors = BTreeMap::new();
        for part in parts {
            for entry in &part.sources {
                anchors.entry(entry.message).or_insert(part.id);
            }
        }
        anchors
    }
}

#[must_use]
pub fn replacement_target<'a>(
    old: &BTreeSet<Uuid>,
    candidates: impl IntoIterator<Item = (Uuid, &'a BTreeSet<Uuid>)>,
) -> Option<Uuid> {
    if old.is_empty() {
        return None;
    }
    let mut best = None;
    let mut size = usize::MAX;
    for (id, coverage) in candidates {
        if coverage.len() <= size && old.is_subset(coverage) {
            best = (coverage.len() < size).then_some(id);
            size = coverage.len();
        }
    }
    best
}

#[must_use]
pub fn summary_job_target(task: &str, payload: &serde_json::Value) -> Option<Uuid> {
    let target = match task {
        "extract-wiki" | "extract-entities" => payload.get("summaryId"),
        "embed-target" if payload.get("targetTable")?.as_str()? == "ros_summaries" => {
            payload.get("targetId")
        }
        _ => None,
    }?;
    Uuid::parse_str(target.as_str()?).ok()
}
