use jiff::Timestamp;
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

use super::SummaryKind;

#[derive(Debug, Clone)]
pub struct PreparedSummaryTree {
    pub conversation: Uuid,
    pub owner: String,
    pub expected_ids: BTreeSet<Uuid>,
    pub expected_revision: Option<i64>,
    pub historical: bool,
    pub created_at: Timestamp,
    pub records: Vec<PreparedSummaryRecord>,
    pub retained_parts: BTreeSet<Uuid>,
    pub part_order: Vec<Uuid>,
    pub passage_windows: BTreeMap<Uuid, Vec<std::ops::Range<i32>>>,
}

#[derive(Debug, Clone)]
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

#[derive(Debug, Clone)]
pub struct SummarySectionBounds {
    pub first: i32,
    pub last: i32,
    pub verified: bool,
    pub char_start: i32,
    pub char_end: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryManifestEntry {
    pub message: Uuid,
    pub ordinal: i32,
    pub render_key: Option<[u8; 32]>,
}

#[derive(Debug, thiserror::Error)]
#[error("invalid prepared summary manifest")]
pub struct InvalidSummaryManifest;

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SectionIdentity {
    pub part: Option<Uuid>,
    pub ordinal: i32,
    pub content: String,
    pub verified: bool,
    pub messages: Vec<Uuid>,
}

pub type SummaryCoverage = BTreeMap<Uuid, BTreeSet<Uuid>>;

impl PreparedSummaryTree {
    pub fn validate_index_key(&self, key: Option<[u8; 32]>) -> Result<(), SummaryReferenceError> {
        if let Some(expected) = key
            && let Some(row) = self.records.iter().find(|row| {
                !self.retained_parts.contains(&row.id)
                    && row.index_key.is_some_and(|index| index != expected)
            })
        {
            return Err(SummaryReferenceError::Stale(row.id));
        }
        Ok(())
    }

    pub fn coverage(&self) -> Result<SummaryCoverage, InvalidSummaryManifest> {
        let records: BTreeMap<_, _> = self.records.iter().map(|row| (row.id, row)).collect();
        if records.len() != self.records.len()
            || !self.retained_parts.is_subset(&self.expected_ids)
            || self.retained_parts.iter().any(|id| {
                records
                    .get(id)
                    .is_none_or(|row| row.kind != SummaryKind::Part)
            })
            || records
                .keys()
                .any(|id| self.expected_ids.contains(id) && !self.retained_parts.contains(id))
        {
            return Err(InvalidSummaryManifest);
        }
        for (id, windows) in &self.passage_windows {
            let row = records.get(id).ok_or(InvalidSummaryManifest)?;
            if !matches!(row.kind, SummaryKind::Session | SummaryKind::Part)
                || self.retained_parts.contains(id)
                || row.index_key.is_none()
            {
                return Err(InvalidSummaryManifest);
            }
            let mut end = 0i32;
            for range in windows {
                if range.start != end || range.end <= end || range.end - end > 6000 {
                    return Err(InvalidSummaryManifest);
                }
                end = range.end;
            }
            if end == 0 || usize::try_from(end).ok() != Some(row.content.chars().count()) {
                return Err(InvalidSummaryManifest);
            }
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

        let root_ids: BTreeSet<_> = root_members.values().copied().collect();
        let mut occupied = BTreeSet::new();
        let mut issued = BTreeSet::new();
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
                            || !row
                                .issued_part_ordinal
                                .is_some_and(|ordinal| ordinal > 0 && issued.insert(ordinal))
                            || !row.input_tokens.is_some_and(|tokens| tokens > 0))
                    {
                        return Err(InvalidSummaryManifest);
                    }
                    let mut ordinals = BTreeSet::new();
                    let mut members = BTreeSet::new();
                    for entry in &row.sources {
                        if entry.ordinal <= 0
                            || !root_ids.contains(&entry.message)
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
        if !self.part_order.is_empty() {
            let ids: BTreeSet<_> = parts.iter().map(|row| row.id).collect();
            if self.part_order.len() != ids.len()
                || self.part_order.iter().copied().collect::<BTreeSet<_>>() != ids
            {
                return Err(InvalidSummaryManifest);
            }
        }
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
    pub fn section_identity(&self, row: &PreparedSummaryRecord) -> Option<SectionIdentity> {
        let bounds = row.section.as_ref()?;
        let parent = self
            .records
            .iter()
            .find(|parent| Some(parent.id) == row.parent)?;
        let mut sources: Vec<_> = parent
            .sources
            .iter()
            .filter(|entry| (bounds.first..=bounds.last).contains(&entry.ordinal))
            .collect();
        sources.sort_by_key(|entry| entry.ordinal);
        Some(SectionIdentity {
            part: (parent.kind == SummaryKind::Part).then_some(parent.id),
            ordinal: row.ordinal,
            content: row.content.clone(),
            verified: bounds.verified,
            messages: sources.into_iter().map(|entry| entry.message).collect(),
        })
    }

    pub fn retain_sections(
        &mut self,
        coverage: &mut SummaryCoverage,
        old: impl IntoIterator<Item = (Uuid, SectionIdentity)>,
    ) -> BTreeSet<Uuid> {
        let mut candidates = BTreeMap::new();
        for (id, identity) in old {
            candidates
                .entry(identity)
                .and_modify(|slot| *slot = None)
                .or_insert(Some(id));
        }
        let matches: BTreeMap<_, _> = self
            .records
            .iter()
            .filter_map(|row| {
                let identity = self.section_identity(row)?;
                Some((row.id, candidates.remove(&identity).flatten()?))
            })
            .collect();
        for row in &mut self.records {
            if let Some(&id) = matches.get(&row.id) {
                if let Some(members) = coverage.remove(&row.id) {
                    coverage.insert(id, members);
                }
                row.id = id;
            }
        }
        matches.into_values().collect()
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
        if self.part_order.is_empty() {
            parts.sort_by_key(|record| record.ordinal);
        } else {
            let order: BTreeMap<_, _> = self
                .part_order
                .iter()
                .enumerate()
                .map(|(position, id)| (*id, position))
                .collect();
            parts.sort_by_key(|record| order.get(&record.id).copied());
        }
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
